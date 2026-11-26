//! SimpleWave flags: a closed-form sine/cosine surface over a shared unit grid.
//!
//! Flag-local coordinates are metres: X along the width from the left edge, Y down
//! from the top edge and Z = X × Y. As in image coordinates, the artwork reads
//! upright from the -Z side, where the front normal points (like an image quad's);
//! the +Z side shows it mirrored. The centre line `u = 0.5` stays pinned and both
//! halves wave outwards; there is no pole, gravity or cloth solver.
//! The GPU evaluates the same formulas (`flag.wgsl`); [`Flag::sample`] is their
//! reference.

use std::{f64::consts::TAU, sync::OnceLock};

use super::{Aabb, MeshAttributes, MeshData, SceneError};

/// Grid cells across the flag's width.
pub const FLAG_COLUMNS: u32 = 96;
/// Grid cells down the flag's height.
pub const FLAG_ROWS: u32 = 60;
/// Grid vertices; vertex `i` is at column `i % (FLAG_COLUMNS + 1)`, row
/// `i / (FLAG_COLUMNS + 1)`.
pub const FLAG_VERTICES: u32 = (FLAG_COLUMNS + 1) * (FLAG_ROWS + 1);

/// Peak normal displacement per metre of amplitude: `1 + 0.35 + 0.18`.
const NORMAL_REACH: f64 = 1.53;
/// Largest downward sag per metre of amplitude: `0.14 + 0.08`.
const SAG_REACH: f64 = 0.22;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagShading {
    /// SimpleWave's two-sided diffuse and highlight under a fixed key light.
    Simple,
    /// The material's PBR shading under the scene's lights, environment and shadows.
    Pbr,
}

/// Size and motion of a flag instance; the instance model maps flag-local metres
/// to the world and should be rigid so the amplitude stays in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flag {
    pub width: f32,
    pub height: f32,
    /// Wave coefficient in metres; the peak displacement is at most 1.53 times this.
    pub amplitude: f32,
    /// Primary wave cycles from the centre line to either free edge.
    pub frequency: f32,
    /// Time scale: the wave advances `speed` seconds per second.
    pub speed: f32,
    pub shading: FlagShading,
    /// Clockwise quarter turns of the artwork within the flag as it reads, 0 to 3.
    pub artwork_turns: u8,
    /// Fraction of the width shown, opening outwards from the centre line: 1 shows
    /// the whole flag and 0 hides it. Columns beyond it collapse onto its edge.
    pub revealed: f32,
}

impl Flag {
    pub(crate) fn validate(self) -> Result<(), SceneError> {
        let finite = [
            self.width,
            self.height,
            self.amplitude,
            self.frequency,
            self.speed,
        ]
        .into_iter()
        .all(|v| v.is_finite() && v >= 0.0);
        if !finite
            || self.width == 0.0
            || self.height == 0.0
            || self.artwork_turns > 3
            || !(0.0..=1.0).contains(&self.revealed)
        {
            return Err(SceneError::InvalidFlag);
        }
        Ok(())
    }

    /// The grid coordinate `u` in [0, 1] moved onto the revealed part of the width.
    pub fn revealed_u(self, u: f64) -> f64 {
        let half = 0.5 * f64::from(self.revealed);
        0.5 + (u - 0.5).clamp(-half, half)
    }

    /// Texture coordinates (right, down) of the artwork at flag coordinates `(u, v)`.
    pub fn artwork_uv(self, u: f64, v: f64) -> [f64; 2] {
        match self.artwork_turns {
            1 => [v, 1.0 - u],
            2 => [1.0 - u, 1.0 - v],
            3 => [1.0 - v, u],
            _ => [u, v],
        }
    }

    /// Wave phases at `seconds`, reduced in f64 so long-running clocks keep their
    /// precision once the GPU evaluates them in f32.
    pub fn phases(self, seconds: f64) -> [f64; 3] {
        let t = f64::from(self.speed) * seconds;
        [1.6 * t, 2.3 * t, 0.9 * t].map(|phase| phase.rem_euclid(TAU))
    }

    /// Flag-local position and unit front normal (-Z at rest) at grid coordinates
    /// `(u, v)` in [0, 1].
    pub fn sample(self, u: f64, v: f64, phases: [f64; 3]) -> ([f64; 3], [f64; 3]) {
        let [width, height, amplitude, frequency] =
            [self.width, self.height, self.amplitude, self.frequency].map(f64::from);
        let r = (2.0 * u - 1.0).abs();
        let drdu = 2.0 * (2.0 * u - 1.0).signum();
        let k = TAU * frequency;
        let a = k * r - phases[0];
        let b = 1.8 * k * r + 3.0 * v - phases[1];
        let c = 0.65 * k * r - 4.0 * v - phases[2];
        let wave = a.sin() + 0.35 * b.sin() + 0.18 * c.cos();
        let envelope = amplitude * r * r;
        let envelope_du = 2.0 * amplitude * r * drdu;
        let sag = 0.14 - 0.08 * b.sin();
        let position = [width * u, height * v + envelope * sag, envelope * wave];
        let du = [
            width,
            envelope_du * sag - envelope * 0.144 * k * b.cos() * drdu,
            envelope_du * wave + envelope * k * (a.cos() + 0.63 * b.cos() - 0.117 * c.sin()) * drdu,
        ];
        let dv = [
            0.0,
            height - envelope * 0.24 * b.cos(),
            envelope * (1.05 * b.cos() + 0.72 * c.sin()),
        ];
        let normal = [
            dv[1] * du[2] - dv[2] * du[1],
            dv[2] * du[0] - dv[0] * du[2],
            dv[0] * du[1] - dv[1] * du[0],
        ];
        let length = normal.iter().map(|n| n * n).sum::<f64>().sqrt();
        (position, normal.map(|n| n / length))
    }

    /// Conservative flag-local bounds of every phase.
    pub(crate) fn local_bounds(self) -> Result<Aabb, SceneError> {
        let amplitude = f64::from(self.amplitude);
        Aabb::new(
            [0.0, 0.0, -NORMAL_REACH * amplitude],
            [
                f64::from(self.width),
                f64::from(self.height) + SAG_REACH * amplitude,
                NORMAL_REACH * amplitude,
            ],
        )
    }
}

impl MeshData {
    /// The shared unit flag grid in flag-local coordinates with UVs equal to its
    /// XY, one part. Every call shares the same data, so GPU buffers are uploaded
    /// once. Instances using it may carry a [`Flag`].
    pub fn flag_grid() -> Self {
        static GRID: OnceLock<MeshData> = OnceLock::new();
        GRID.get_or_init(|| {
            let columns = FLAG_COLUMNS + 1;
            let uv: Vec<[f32; 2]> = (0..FLAG_VERTICES)
                .map(|i| {
                    [
                        (i % columns) as f32 / FLAG_COLUMNS as f32,
                        (i / columns) as f32 / FLAG_ROWS as f32,
                    ]
                })
                .collect();
            let indices: Vec<u32> = (0..FLAG_ROWS)
                .flat_map(|row| (0..FLAG_COLUMNS).map(move |column| row * columns + column))
                // (b - a) × (c - a) points along the front normal -Z, as in other chr meshes.
                .flat_map(|i| [i, i + columns, i + 1, i + 1, i + columns, i + columns + 1])
                .collect();
            let count = indices.len() as u32;
            let mut mesh = MeshData::new(
                uv.iter().map(|&[u, v]| [u, v, 0.0]).collect(),
                indices,
                std::iter::once(0..count).collect(),
            )
            .and_then(|mesh| {
                mesh.with_attributes(MeshAttributes {
                    normals: vec![[0.0, 0.0, -1.0]; uv.len()],
                    // Bitangent cross(normal, tangent) * w must run down the flag (+Y).
                    tangents: vec![[1.0, 0.0, 0.0, -1.0]; uv.len()],
                    tex_coords: uv,
                    ..Default::default()
                })
            })
            .expect("static flag grid");
            mesh.flag_grid = true;
            mesh
        })
        .clone()
    }

    pub fn is_flag_grid(&self) -> bool {
        self.flag_grid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag() -> Flag {
        Flag {
            width: 4.0,
            height: 2.0,
            amplitude: 0.7,
            frequency: 3.0,
            speed: 1.0,
            shading: FlagShading::Simple,
            artwork_turns: 0,
            revealed: 1.0,
        }
    }

    #[test]
    fn the_centre_line_stays_pinned_and_zero_amplitude_is_flat() {
        let phases = flag().phases(12.3);
        for v in [0.0, 0.4, 1.0] {
            let (position, normal) = flag().sample(0.5, v, phases);
            assert_eq!(position, [2.0, 2.0 * v, 0.0]);
            assert!((normal[2] + 1.0).abs() < 1e-12);
        }
        let still = Flag {
            amplitude: 0.0,
            ..flag()
        };
        let (position, normal) = still.sample(0.9, 0.3, phases);
        assert_eq!(
            (position, normal),
            ([4.0 * 0.9, 2.0 * 0.3, 0.0], [0.0, 0.0, -1.0])
        );
    }

    #[test]
    fn normals_follow_the_surface_and_samples_stay_in_bounds() {
        let tifo = Flag {
            width: 36.0,
            height: 13.5,
            amplitude: 5.0,
            frequency: 1.35,
            ..flag()
        };
        for flag in [flag(), tifo] {
            check_normals_and_bounds(flag);
        }
    }

    fn check_normals_and_bounds(flag: Flag) {
        let bounds = flag.local_bounds().unwrap();
        let phases = flag.phases(7.0);
        let h = 1e-6;
        for (u, v) in [(0.1, 0.2), (0.8, 0.7), (0.97, 0.05)] {
            let (p, n) = flag.sample(u, v, phases);
            for (axis, value) in p.into_iter().enumerate() {
                assert!(bounds.min()[axis] <= value && value <= bounds.max()[axis]);
            }
            for [du, dv] in [[h, 0.0], [0.0, h]] {
                let (ahead, _) = flag.sample(u + du, v + dv, phases);
                let (behind, _) = flag.sample(u - du, v - dv, phases);
                let along: f64 = (0..3)
                    .map(|i| (ahead[i] - behind[i]) / (2.0 * h) * n[i])
                    .sum();
                assert!(along.abs() < 1e-6, "normal is not perpendicular: {along}");
            }
        }
    }

    #[test]
    fn phases_wrap_without_changing_the_surface() {
        let flag = flag();
        let late = flag.phases(86_400.0 * 30.0);
        assert!(late.iter().all(|p| (0.0..TAU).contains(p)));
        let unwrapped = [1.6, 2.3, 0.9].map(|w| w * 1000.0);
        let (a, _) = flag.sample(0.2, 0.3, flag.phases(1000.0));
        let (b, _) = flag.sample(0.2, 0.3, unwrapped);
        assert!((0..3).all(|i| (a[i] - b[i]).abs() < 1e-9));
    }

    #[test]
    fn invalid_flags_are_rejected() {
        for bad in [
            Flag {
                width: 0.0,
                ..flag()
            },
            Flag {
                amplitude: -1.0,
                ..flag()
            },
            Flag {
                speed: f32::NAN,
                ..flag()
            },
            Flag {
                artwork_turns: 4,
                ..flag()
            },
            Flag {
                revealed: 1.5,
                ..flag()
            },
        ] {
            assert_eq!(bad.validate(), Err(SceneError::InvalidFlag));
        }
    }

    #[test]
    fn the_reveal_opens_from_the_centre_line() {
        let half = Flag {
            revealed: 0.5,
            ..flag()
        };
        assert_eq!(
            [0.0, 0.3, 0.5, 0.7, 1.0].map(|u| half.revealed_u(u)),
            [0.25, 0.3, 0.5, 0.7, 0.75]
        );
        let hidden = Flag {
            revealed: 0.0,
            ..flag()
        };
        assert_eq!([0.0, 1.0].map(|u| hidden.revealed_u(u)), [0.5, 0.5]);
        assert_eq!(flag().revealed_u(0.0), 0.0);
    }

    #[test]
    fn artwork_turns_clockwise_as_it_reads() {
        // The flag's top-left corner shows the artwork corner that a clockwise turn
        // brings there: its bottom-left after a quarter turn.
        let corners = |turns| {
            let flag = Flag {
                artwork_turns: turns,
                ..flag()
            };
            [(0.0, 0.0), (1.0, 0.0)].map(|(u, v)| flag.artwork_uv(u, v))
        };
        assert_eq!(corners(0), [[0.0, 0.0], [1.0, 0.0]]);
        assert_eq!(corners(1), [[0.0, 1.0], [0.0, 0.0]]);
        assert_eq!(corners(2), [[1.0, 1.0], [0.0, 1.0]]);
        assert_eq!(corners(3), [[1.0, 0.0], [1.0, 1.0]]);
    }
}
