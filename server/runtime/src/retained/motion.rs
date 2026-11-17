//! Rigid animation of whole instances (#157): a pivot, a rotation about a fixed world
//! axis and a linear path of offset and scale, evaluated on the CPU for each frame.
//! The renderer receives ordinary model matrices, so its shaders are unchanged.

use std::f64::consts::{PI, TAU};

use chr_common::scene_frame as w;
use chr_renderer::scene::ModelMatrix;

#[derive(Clone, Debug, PartialEq)]
pub struct Motion {
    pivot: [f64; 3],
    rotation: Option<Rotation>,
    path: Option<Path>,
}

#[derive(Clone, Debug, PartialEq)]
struct Rotation {
    /// Unit world axis.
    axis: [f64; 3],
    continuous: bool,
    amplitude: f64,
    period: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct Path {
    start: ([f64; 3], f64),
    end: ([f64; 3], f64),
    duration: f64,
    playback: w::Playback,
}

/// Where a motion stands at one time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// World offset of the pivot, metres.
    pub offset: [f64; 3],
    /// Uniform multiplier of the neutral scale.
    pub scale: f64,
    /// Radians about the rotation axis.
    pub angle: f64,
}

impl Motion {
    pub fn new(motion: &w::Motion) -> Result<Self, String> {
        let rotation = motion
            .rotation
            .as_ref()
            .map(|r| {
                let axis = r.axis.map(w::Finite::get);
                let length = axis.iter().map(|v| v * v).sum::<f64>().sqrt();
                if length < 1e-9 {
                    return Err("motion rotation axis must not be zero".to_owned());
                }
                let period = r.period.get();
                if !(0.1..=60.0).contains(&period) {
                    return Err("motion rotation period must be 0.1 to 60 seconds".to_owned());
                }
                let continuous = r.mode == w::RotationMode::Continuous;
                let amplitude = r.amplitude.get();
                if !continuous && !(amplitude > 0.0 && amplitude <= PI) {
                    return Err("motion swing amplitude must be in (0, pi] radians".to_owned());
                }
                Ok(Rotation {
                    axis: axis.map(|v| v / length),
                    continuous,
                    amplitude,
                    period,
                })
            })
            .transpose()?;
        let path = motion
            .path
            .as_ref()
            .map(|p| {
                let key = |k: &w::MotionKey| {
                    let scale = k.scale.get();
                    if scale <= 0.0 {
                        return Err("motion scale must be positive".to_owned());
                    }
                    Ok((k.offset.map(w::Finite::get), scale))
                };
                let duration = p.duration.get();
                if duration <= 0.0 {
                    return Err("motion duration must be positive".to_owned());
                }
                Ok(Path {
                    start: key(&p.start)?,
                    end: key(&p.end)?,
                    duration,
                    playback: p.playback,
                })
            })
            .transpose()?;
        Ok(Self {
            pivot: motion.pivot.map(w::Finite::get),
            rotation,
            path,
        })
    }

    /// The motion `seconds` after its start; earlier times hold the start.
    pub fn sample(&self, seconds: f64) -> Sample {
        let t = seconds.max(0.0);
        let angle = self.rotation.as_ref().map_or(0.0, |r| {
            if r.continuous {
                TAU * (t / r.period).rem_euclid(1.0)
            } else {
                r.amplitude * (TAU * t / r.period).sin()
            }
        });
        let Some(path) = &self.path else {
            return Sample {
                offset: [0.0; 3],
                scale: 1.0,
                angle,
            };
        };
        let d = path.duration;
        let progress = match path.playback {
            w::Playback::Once => (t / d).min(1.0),
            w::Playback::Loop => t.rem_euclid(d) / d,
            w::Playback::PingPong => {
                let u = t.rem_euclid(2.0 * d) / d;
                if u > 1.0 { 2.0 - u } else { u }
            }
        };
        let lerp = |a: f64, b: f64| a + (b - a) * progress;
        Sample {
            offset: std::array::from_fn(|i| lerp(path.start.0[i], path.end.0[i])),
            scale: lerp(path.start.1, path.end.1),
            angle,
        }
    }

    /// `T(pivot + offset) * scale * R(axis, angle) * T(-pivot) * model` at `seconds`.
    pub fn model(&self, model: ModelMatrix, seconds: f64) -> Result<ModelMatrix, String> {
        let Sample {
            offset,
            scale,
            angle,
        } = self.sample(seconds);
        let r = self
            .rotation
            .as_ref()
            .map_or(IDENTITY3, |r| rotation(r.axis, angle));
        let p = self.pivot;
        // Linear part L = scale * R; translation pivot + offset - L * pivot.
        let mut motion = [[0.0; 4]; 4];
        for (row, out) in motion.iter_mut().enumerate().take(3) {
            for column in 0..3 {
                out[column] = scale * r[row][column];
            }
            out[3] = p[row] + offset[row] - (0..3).map(|c| out[c] * p[c]).sum::<f64>();
        }
        motion[3][3] = 1.0;
        let base = model.rows();
        let rows = std::array::from_fn(|r| {
            std::array::from_fn(|c| (0..4).map(|k| motion[r][k] * base[k][c]).sum())
        });
        ModelMatrix::new(rows).map_err(|e| e.to_string())
    }
}

const IDENTITY3: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Right-handed rotation by `angle` about the unit `axis` (Rodrigues).
fn rotation(axis: [f64; 3], angle: f64) -> [[f64; 3]; 3] {
    let (s, c) = angle.sin_cos();
    let [x, y, z] = axis;
    let k = [[0.0, -z, y], [z, 0.0, -x], [-y, x, 0.0]];
    std::array::from_fn(|r| {
        std::array::from_fn(|col| {
            IDENTITY3[r][col] * c + s * k[r][col] + (1.0 - c) * axis[r] * axis[col]
        })
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn motion(value: serde_json::Value) -> Result<Motion, String> {
        Motion::new(&serde_json::from_value(value).unwrap())
    }

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    fn path(playback: &str) -> Motion {
        motion(json!({"pivot": [10, 5, 0], "path": {
            "start": {"offset": [-8, 2, 0], "scale": 0.5},
            "end": {"offset": [16, -6, -4], "scale": 1.5},
            "duration": 4, "playback": playback}}))
        .unwrap()
    }

    #[test]
    fn ping_pong_samples_start_middle_end_middle_start() {
        let m = path("ping_pong");
        let expected = [
            ([-8.0, 2.0, 0.0], 0.5),
            ([4.0, -2.0, -2.0], 1.0),
            ([16.0, -6.0, -4.0], 1.5),
            ([4.0, -2.0, -2.0], 1.0),
            ([-8.0, 2.0, 0.0], 0.5),
        ];
        for (i, (offset, scale)) in expected.into_iter().enumerate() {
            let s = m.sample(2.0 * i as f64);
            assert!(close(s.offset, offset), "{i}: {s:?}");
            assert!((s.scale - scale).abs() < 1e-12, "{i}: {s:?}");
        }
    }

    #[test]
    fn once_holds_its_end_and_loop_restarts() {
        let once = path("once");
        assert!(close(once.sample(4.0).offset, [16.0, -6.0, -4.0]));
        assert_eq!(once.sample(100.0), once.sample(4.0));
        assert_eq!(once.sample(-3.0), once.sample(0.0));
        let looped = path("loop");
        assert!(close(looped.sample(4.0).offset, [-8.0, 2.0, 0.0]));
        assert!(close(looped.sample(5.0).offset, looped.sample(1.0).offset));
        let still = motion(json!({"pivot": [0, 0, 0]})).unwrap();
        assert_eq!(
            still.sample(7.0),
            Sample {
                offset: [0.0; 3],
                scale: 1.0,
                angle: 0.0
            }
        );
    }

    #[test]
    fn rotation_continues_after_a_once_path_ends() {
        let mut m = path("once");
        m.rotation = motion(json!({"pivot": [0, 0, 0], "rotation":
            {"axis": [0, 0, 2], "mode": "continuous", "amplitude": 0, "period": 8}}))
        .unwrap()
        .rotation;
        assert!((m.sample(6.0).angle - 1.5 * PI).abs() < 1e-12);
        assert!(close(m.sample(6.0).offset, [16.0, -6.0, -4.0]));
        let swing = motion(json!({"pivot": [0, 0, 0], "rotation":
            {"axis": [1, 0, 0], "mode": "swing", "amplitude": 0.5, "period": 2}}))
        .unwrap();
        assert!((swing.sample(0.5).angle - 0.5).abs() < 1e-12);
        assert!((swing.sample(1.5).angle + 0.5).abs() < 1e-12);
    }

    #[test]
    fn translation_stays_in_world_axes_and_scale_and_rotation_act_about_the_pivot() {
        // A heading of 90 degrees about +Z in the neutral model must not turn the path.
        let heading = ModelMatrix::new([
            [0.0, -2.0, 0.0, 10.0],
            [2.0, 0.0, 0.0, 5.0],
            [0.0, 0.0, 2.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])
        .unwrap();
        let mut m = path("once");
        m.rotation = motion(json!({"pivot": [0, 0, 0], "rotation":
            {"axis": [0, 0, 1], "mode": "continuous", "amplitude": 0, "period": 4}}))
        .unwrap()
        .rotation;
        let at = |t: f64| m.model(heading, t).unwrap().rows();
        let apply = |rows: [[f64; 4]; 4], p: [f64; 3]| -> [f64; 3] {
            std::array::from_fn(|r| (0..3).map(|c| rows[r][c] * p[c]).sum::<f64>() + rows[r][3])
        };
        // The model's bottom centre (local origin) is the pivot: it follows the path only.
        assert!(close(
            apply(at(1.0), [0.0; 3]),
            [10.0 - 2.0, 5.0 + 0.0, -1.0]
        ));
        assert!(close(apply(at(4.0), [0.0; 3]), [26.0, -1.0, -4.0]));
        // At t = 1 the rotation is a quarter turn and the scale 0.75: a local +X point
        // (world +Y at 2 m) turns to world -X and shrinks to 1.5 m from the pivot.
        let pivot = apply(at(1.0), [0.0; 3]);
        let tip = apply(at(1.0), [1.0, 0.0, 0.0]);
        assert!(close(
            std::array::from_fn(|i| tip[i] - pivot[i]),
            [-1.5, 0.0, 0.0]
        ));
    }

    #[test]
    fn invalid_settings_are_rejected_with_a_reason() {
        for (bad, reason) in [
            (
                json!({"pivot": [0,0,0], "rotation": {"axis": [0,0,0], "mode": "swing", "amplitude": 1, "period": 2}}),
                "axis",
            ),
            (
                json!({"pivot": [0,0,0], "rotation": {"axis": [0,0,1], "mode": "swing", "amplitude": 0, "period": 2}}),
                "amplitude",
            ),
            (
                json!({"pivot": [0,0,0], "rotation": {"axis": [0,0,1], "mode": "swing", "amplitude": 4, "period": 2}}),
                "amplitude",
            ),
            (
                json!({"pivot": [0,0,0], "rotation": {"axis": [0,0,1], "mode": "continuous", "amplitude": 0, "period": 0.05}}),
                "period",
            ),
            (
                json!({"pivot": [0,0,0], "path": {"start": {"offset": [0,0,0], "scale": 0}, "end": {"offset": [0,0,0], "scale": 1}, "duration": 1, "playback": "once"}}),
                "scale",
            ),
            (
                json!({"pivot": [0,0,0], "path": {"start": {"offset": [0,0,0], "scale": 1}, "end": {"offset": [0,0,0], "scale": 1}, "duration": 0, "playback": "once"}}),
                "duration",
            ),
        ] {
            let error = motion(bad).unwrap_err();
            assert!(error.contains(reason), "{error}");
        }
        let unknown = json!({"pivot": [0,0,0], "path": {"start": {"offset": [0,0,0], "scale": 1},
            "end": {"offset": [0,0,0], "scale": 1}, "duration": 1, "playback": "bounce"}});
        assert!(serde_json::from_value::<w::Motion>(unknown).is_err());
    }
}
