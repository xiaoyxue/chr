//! Native flags: a physically based cloth ported from the AER WebGPU flag simulation
//! (xiaoyxue/Simulation, `src/simulation.js` and `src/wind.js`).
//!
//! The cloth is a grid of [`CLOTH_COLUMNS`] × [`CLOTH_ROWS`] particles in flag-local
//! metres (X along the width, Y down, Z = X × Y). Its top row is pinned along Y = 0
//! and its bottom row along Y = height; the sides are free. Its rest length down the
//! flag exceeds the pinned span by its [`Cloth::slack`], so it billows. Each fixed step of
//! [`STEP`] seconds integrates gravity, damping and an aerodynamic load from the
//! local normal and the gusting, wind-relative velocity (Verlet), then relaxes
//! structural, shear and bending distances in [`ITERATIONS`] Jacobi iterations
//! (`flag_cloth.wgsl`). The reference is Y-up with its front at +Z, the side that
//! reads the artwork; here those are -Y and -Z, a proper rotation, so its formulas
//! carry over with only its constant vectors turned. Its floor contact is dropped:
//! flag-local space has no ground.
//!
//! Every cloth is simulated [`SIMULATED_HEIGHT`] tall, the reference's, at its flag's
//! aspect, and scaled to its flag ([`simulated_size`]): a flag of any size folds and
//! ripples as deeply and as quickly as the reference's does, where a real cloth tens
//! of metres tall would move in proportionally shallower, slower folds.

use super::{Aabb, SceneError};

/// Particles across the cloth's width.
pub const CLOTH_COLUMNS: u32 = 65;
/// Particles down the cloth, from the pinned top row to the pinned bottom row.
pub const CLOTH_ROWS: u32 = 41;
/// Fixed simulation step in seconds.
pub const STEP: f64 = 1.0 / 120.0;
/// Jacobi constraint iterations per step.
pub const ITERATIONS: u32 = 36;
/// The largest slack: a cloth 25 % longer than its pinned span.
pub const MAX_SLACK: f32 = 0.2;
/// The height in metres every cloth is simulated at: the reference's pinned span.
pub const SIMULATED_HEIGHT: f32 = 4.8;
/// The span a raised cloth starts folded over, per metre of its height: the
/// reference's 0.25 m over its 4.8 m.
const FOLDED: f64 = 0.25 / 4.8;
/// Cloth rows per accordion fold of a folded cloth, as the reference's.
const FOLD_ROWS: u32 = 8;
/// The strongest wind the reference keeps stable at either tension, in m/s.
pub const MAX_WIND: f32 = 18.0;
/// The reference's visually tuned gravity in m/s², lighter than free fall.
pub const GRAVITY: f64 = 3.8;
/// The reference's ripple strength per m/s of wind.
const RIPPLE_PER_WIND: f64 = 1.0 / 7.0;
/// Seconds between automatic wind targets and their spread around the base, in degrees.
const AUTO_INTERVAL: f64 = 0.5;
const AUTO_RANGE: f64 = 45.0;
/// The iterative constraints let the cloth stretch under load; a particle stays
/// within this factor of its rest path from a pinned edge (checked on the GPU).
const STRETCH: f64 = 1.5;

/// The wind and fabric of a native flag, and its clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cloth {
    /// Wind speed in m/s, 0 to [`MAX_WIND`].
    pub wind_speed: f32,
    /// Base direction the wind blows towards, in degrees about flag-local Y: 0 along
    /// +X (towards the right edge as the artwork reads) and 90 along -Z (towards the
    /// side that reads it).
    pub wind_direction: f32,
    /// Turns the wind smoothly to a new target within 45° of the base direction every
    /// 0.5 s of the cloth's clock, the same sequence every run.
    pub auto_direction: bool,
    /// Bending stiffness from 0 (soft and flowing) to 1 (crisp and firm).
    pub tension: f32,
    /// The part of the cloth's rest length down the flag that the pinned edges do
    /// not span, 0 (taut) to [`MAX_SLACK`]: the reference's 0.1 makes it 11 % longer
    /// than the flag, so it billows; a large flag lying along a stand sags by as
    /// much, about a fifth of its height.
    pub slack: f32,
    /// Seconds over which the cloth is raised from its clock's zero, or `None` when
    /// it hangs raised from the start. A raised cloth starts folded just above its
    /// pinned bottom edge and its top edge rises to Y = 0 ([`Cloth::top_at`]).
    pub raise: Option<f32>,
    /// Seconds since the cloth hung in its slack arc. The renderer simulates each
    /// cloth up to its time in fixed steps as frames are prepared (`flag_cloth`).
    pub seconds: f64,
}

impl Cloth {
    pub(crate) fn validate(self) -> bool {
        (0.0..=MAX_WIND).contains(&self.wind_speed)
            && self.wind_direction.is_finite()
            && (0.0..=1.0).contains(&self.tension)
            && (0.0..=MAX_SLACK).contains(&self.slack)
            && self
                .raise
                .is_none_or(|duration| duration.is_finite() && duration > 0.0)
            && self.seconds.is_finite()
            && self.seconds >= 0.0
    }

    /// The direction the wind blows towards at `seconds` of the cloth's clock, in
    /// degrees in [-180, 180), as the reference's `windDirectionAt`.
    pub fn direction_at(self, seconds: f64) -> f64 {
        let base = f64::from(self.wind_direction);
        if !self.auto_direction {
            return base;
        }
        let sample = seconds / AUTO_INTERVAL;
        let segment = sample.floor();
        let fraction = sample - segment;
        let blend = fraction * fraction * (3.0 - 2.0 * fraction);
        let segment = segment as u32;
        let from = if segment == 0 {
            0.0
        } else {
            auto_offset(segment - 1)
        };
        let to = auto_offset(segment);
        (base + from + (to - from) * blend + 180.0).rem_euclid(360.0) - 180.0
    }

    /// Flag-local air velocity in m/s at `seconds` and the strength of its ripple.
    pub(crate) fn wind_at(self, seconds: f64) -> ([f64; 3], f64) {
        let speed = f64::from(self.wind_speed);
        let (sin, cos) = self.direction_at(seconds).to_radians().sin_cos();
        ([speed * cos, 0.0, -speed * sin], speed * RIPPLE_PER_WIND)
    }

    /// Flag-local Y of the pinned top edge of a cloth `height` tall at `seconds` of its
    /// clock: 0 once raised, else rising from just above the bottom edge on the
    /// reference's smoothstep.
    pub fn top_at(self, height: f32, seconds: f64) -> f64 {
        let Some(duration) = self.raise else {
            return 0.0;
        };
        let height = f64::from(height);
        let progress = (seconds / f64::from(duration)).clamp(0.0, 1.0);
        let eased = progress * progress * (3.0 - 2.0 * progress);
        (height - height * FOLDED) * (1.0 - eased)
    }

    /// The cloth's particles when its clock starts: folded to be raised, or hanging.
    pub fn start_positions(self, width: f32, height: f32) -> Vec<[f64; 3]> {
        match self.raise {
            Some(_) => folded_positions(width, height, self.slack),
            None => initial_positions(width, height, self.slack),
        }
    }

    /// The reference's bending strength at this tension.
    pub(crate) fn bending(self) -> f64 {
        0.005 + (0.12 - 0.005) * f64::from(self.tension)
    }
}

/// The reference's fixed hash sequence of automatic wind offsets, in degrees within
/// ±45; its 32-bit wrapping arithmetic matches `Math.imul` bit for bit.
fn auto_offset(segment: u32) -> f64 {
    let mut value = segment.wrapping_add(1).wrapping_mul(0x45d9f3b) ^ 0x6d2b79f5;
    value = (value ^ (value >> 16)).wrapping_mul(0x45d9f3b);
    value = (value ^ (value >> 16)).wrapping_mul(0x45d9f3b);
    value ^= value >> 16;
    (f64::from(value) / f64::from(u32::MAX) * 2.0 - 1.0) * AUTO_RANGE
}

/// The size a `width` × `height` flag's cloth is simulated at: [`SIMULATED_HEIGHT`]
/// tall at the flag's aspect. Its surface is scaled back by `height /
/// SIMULATED_HEIGHT` (`flag_cloth.wgsl`).
pub(crate) fn simulated_size(width: f32, height: f32) -> [f32; 2] {
    [width * (SIMULATED_HEIGHT / height), SIMULATED_HEIGHT]
}

/// The cloth's rest length down a flag of `height` with `slack`.
fn rest_length(height: f32, slack: f32) -> f64 {
    f64::from(height) / (1.0 - f64::from(slack))
}

/// Rest spacing of neighbouring particles across and down a `width` × `height` flag.
pub(crate) fn spacing(width: f32, height: f32, slack: f32) -> [f64; 2] {
    [
        f64::from(width) / f64::from(CLOTH_COLUMNS - 1),
        rest_length(height, slack) / f64::from(CLOTH_ROWS - 1),
    ]
}

/// The cloth at rest, row by row: a slack arc bulging towards the reading side (-Z)
/// whose segments down the flag keep their rest length, as the reference's
/// `initialParticles` (unfolded); flat without slack.
pub fn initial_positions(width: f32, height: f32, slack: f32) -> Vec<[f64; 3]> {
    let span = f64::from(height);
    let [across, segment] = spacing(width, height, slack);
    let segments = f64::from(CLOTH_ROWS - 1);
    if slack == 0.0 {
        return (0..CLOTH_ROWS)
            .flat_map(|row| {
                (0..CLOTH_COLUMNS)
                    .map(move |column| [f64::from(column) * across, f64::from(row) * segment, 0.0])
            })
            .collect();
    }
    // The arc's turning angle: its chord of equal segments spans the pinned edges.
    let (mut low, mut high) = (0.0, std::f64::consts::PI);
    let mut turn = 0.0;
    for _ in 0..40 {
        turn = (low + high) / 2.0;
        let chord = segment * (turn / 2.0).sin() / (turn / (2.0 * segments)).sin();
        if chord > span {
            low = turn;
        } else {
            high = turn;
        }
    }
    let radius = span / (2.0 * (turn / 2.0).sin());
    (0..CLOTH_ROWS)
        .flat_map(|row| {
            let pinned = row == 0 || row == CLOTH_ROWS - 1;
            let angle = (f64::from(row) / segments - 0.5) * turn;
            let (y, z) = if pinned {
                (if row == 0 { 0.0 } else { span }, 0.0)
            } else {
                (
                    span / 2.0 + radius * angle.sin(),
                    radius * ((turn / 2.0).cos() - angle.cos()),
                )
            };
            (0..CLOTH_COLUMNS).map(move |column| [f64::from(column) * across, y, z])
        })
        .collect()
}

/// The cloth folded to be raised, row by row, as the reference's folded
/// `initialParticles`: its rows evenly spaced down the folded span above the bottom
/// edge, zigzagging towards the reading side (-Z) in accordion folds of
/// [`FOLD_ROWS`] rows whose segments keep their rest length.
fn folded_positions(width: f32, height: f32, slack: f32) -> Vec<[f64; 3]> {
    let [across, segment] = spacing(width, height, slack);
    let span = f64::from(height) * FOLDED;
    let rise = span / f64::from(CLOTH_ROWS - 1);
    let step = (segment * segment - rise * rise).sqrt();
    (0..CLOTH_ROWS)
        .flat_map(|row| {
            let fold = row % FOLD_ROWS;
            let depth = f64::from(fold.min(FOLD_ROWS - fold)) * step;
            let y = f64::from(height) - span + f64::from(row) * rise;
            (0..CLOTH_COLUMNS).map(move |column| [f64::from(column) * across, y, -depth])
        })
        .collect()
}

/// Conservative flag-local bounds of a `width` × `height` cloth in any state: every
/// particle stays within its stretched rest path of a pinned edge, at most half the
/// rest length down the flag.
pub(crate) fn local_bounds(width: f32, height: f32, slack: f32) -> Result<Aabb, SceneError> {
    let reach = STRETCH * rest_length(height, slack) / 2.0;
    Aabb::new(
        [-reach, -reach, -reach],
        [f64::from(width) + reach, f64::from(height) + reach, reach],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cloth() -> Cloth {
        Cloth {
            wind_speed: 7.0,
            wind_direction: 25.0,
            auto_direction: true,
            tension: 0.65,
            slack: 0.1,
            raise: None,
            seconds: 0.0,
        }
    }

    #[test]
    fn a_raised_cloth_starts_folded_and_its_top_rises_smoothly() {
        let raised = Cloth {
            raise: Some(6.0),
            ..cloth()
        };
        let (width, height) = (40.0, 40.0);
        let folded = raised.start_positions(width, height);
        let [_, down] = spacing(width, height, 0.1);
        let at = |row: u32| folded[(row * CLOTH_COLUMNS + 3) as usize];
        // Pinned rows: the top just above the bottom edge, which stays at Y = height.
        assert!((at(0)[1] - raised.top_at(height, 0.0)).abs() < 1e-9 && at(0)[2] == 0.0);
        assert_eq!((at(CLOTH_ROWS - 1)[1], at(CLOTH_ROWS - 1)[2]), (40.0, 0.0));
        for row in 1..CLOTH_ROWS {
            let (a, b) = (at(row - 1), at(row));
            let length = (0..3).map(|i| (b[i] - a[i]).powi(2)).sum::<f64>().sqrt();
            assert!(
                (length - down).abs() < 1e-9,
                "row {row}: {length} vs {down}"
            );
            assert!(b[2] <= 0.0, "folds towards -Z");
        }
        let bounds = local_bounds(width, height, 0.1).unwrap();
        assert!(
            folded
                .iter()
                .all(|p| (0..3).all(|i| bounds.min()[i] <= p[i] && p[i] <= bounds.max()[i]))
        );
        // The top rises monotonically on a smoothstep and then holds at Y = 0.
        let tops: Vec<f64> = (0..=80)
            .map(|k| raised.top_at(height, f64::from(k) * 0.1))
            .collect();
        assert!((tops[0] - 40.0 * (1.0 - 0.25 / 4.8)).abs() < 1e-9);
        assert!(tops.windows(2).all(|w| w[1] <= w[0]));
        assert!(
            (tops[30] - tops[0] / 2.0).abs() < 1e-9,
            "halfway at half time"
        );
        assert_eq!(tops[60], 0.0);
        assert_eq!(tops[80], 0.0);
        assert_eq!(
            cloth().top_at(height, 0.0),
            0.0,
            "an unraised cloth hangs raised"
        );
        assert_eq!(
            cloth().start_positions(width, height),
            initial_positions(width, height, 0.1)
        );
    }

    #[test]
    fn the_rest_arc_keeps_its_segments_and_pins_both_edges() {
        let (width, height) = (3.2, 4.8);
        let positions = initial_positions(width, height, 0.1);
        assert_eq!(positions.len(), (CLOTH_COLUMNS * CLOTH_ROWS) as usize);
        let [across, down] = spacing(width, height, 0.1);
        let at = |column: u32, row: u32| positions[(row * CLOTH_COLUMNS + column) as usize];
        assert_eq!(at(0, 0), [0.0, 0.0, 0.0]);
        assert_eq!(
            at(CLOTH_COLUMNS - 1, CLOTH_ROWS - 1),
            [3.2_f32 as f64, 4.8_f32 as f64, 0.0]
        );
        for row in 1..CLOTH_ROWS {
            let (a, b) = (at(7, row - 1), at(7, row));
            let length = (0..3).map(|i| (b[i] - a[i]).powi(2)).sum::<f64>().sqrt();
            assert!(
                (length - down).abs() < 1e-9,
                "row {row}: {length} vs {down}"
            );
            assert!(b[2] <= 0.0, "the arc bulges towards -Z");
        }
        assert!((at(1, 9)[0] - at(0, 9)[0] - across).abs() < 1e-12);
        let bounds = local_bounds(width, height, 0.1).unwrap();
        for p in positions {
            assert!((0..3).all(|i| bounds.min()[i] <= p[i] && p[i] <= bounds.max()[i]));
        }
    }

    #[test]
    fn less_slack_bulges_less_and_none_hangs_flat() {
        // A tifo-sized flag: 10 % slack bows it out by about a fifth of its height.
        let depth = |slack| {
            initial_positions(26.0, 22.66, slack)
                .into_iter()
                .map(|p| -p[2])
                .fold(0.0, f64::max)
        };
        assert!((4.5..4.9).contains(&depth(0.1)), "{}", depth(0.1));
        assert!((1.8..2.2).contains(&depth(0.02)), "{}", depth(0.02));
        let flat = initial_positions(26.0, 22.66, 0.0);
        let last = flat[flat.len() - 1];
        assert_eq!(last, [26.0, f64::from(22.66_f32), 0.0]);
        assert!(flat.iter().all(|p| p[2] == 0.0));
    }

    #[test]
    fn automatic_wind_matches_the_reference_sequence() {
        // xiaoyxue/Simulation `windDirectionAt(t, { direction, autoDirection: true })`.
        for (seconds, direction, expected) in [
            (0.3, 25.0, 10.023014532854518),
            (1.37, 25.0, 27.040520015593643),
            (7.25, -170.0, -177.58095059254697),
            (123.4, 170.0, 144.30564734558254),
        ] {
            let angle = Cloth {
                wind_direction: direction,
                ..cloth()
            }
            .direction_at(seconds);
            assert!((angle - expected).abs() < 1e-9, "{seconds} s: {angle}");
        }
    }

    #[test]
    fn automatic_wind_wanders_smoothly_within_45_degrees_and_wraps() {
        let manual = Cloth {
            auto_direction: false,
            ..cloth()
        };
        assert_eq!(manual.direction_at(3.3), 25.0);
        assert_eq!(cloth().direction_at(0.0), 25.0);
        let mut previous = cloth().direction_at(0.0);
        for step in 1..2400 {
            let angle = cloth().direction_at(step as f64 * STEP);
            assert!((angle - 25.0).abs() <= 45.0 + 1e-9, "{angle}");
            // A 90° turn over 0.5 s peaks at 270°/s mid-segment: 2.25° per step.
            assert!(
                (angle - previous).abs() < 2.5,
                "smooth: {previous} -> {angle}"
            );
            previous = angle;
        }
        // Each segment ends on its target, which the next segment starts from.
        assert!((cloth().direction_at(1.0) - 25.0 - auto_offset(1)).abs() < 1e-9);
        let near_edge = Cloth {
            wind_direction: 170.0,
            ..cloth()
        };
        let wrapped = (0..240).map(|i| near_edge.direction_at(f64::from(i) * 0.05));
        assert!(wrapped.clone().all(|a| (-180.0..180.0).contains(&a)));
        assert!(wrapped.clone().any(|a| a < 0.0), "crosses the ±180° line");
    }

    #[test]
    fn the_wind_blows_along_its_direction_in_the_flag_plane() {
        let blow = |direction| {
            Cloth {
                auto_direction: false,
                wind_direction: direction,
                ..cloth()
            }
            .wind_at(0.0)
            .0
        };
        let close = |a: [f64; 3], b: [f64; 3]| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-12);
        assert!(close(blow(0.0), [7.0, 0.0, 0.0]));
        assert!(
            close(blow(90.0), [0.0, 0.0, -7.0]),
            "towards the reading side"
        );
        assert!(close(blow(-90.0), [0.0, 0.0, 7.0]));
        assert!((cloth().bending() - 0.08).abs() < 1e-3);
    }

    #[test]
    fn out_of_range_wind_tension_and_time_are_invalid() {
        assert!(cloth().validate());
        for bad in [
            Cloth {
                wind_speed: 18.5,
                ..cloth()
            },
            Cloth {
                wind_speed: -1.0,
                ..cloth()
            },
            Cloth {
                tension: 1.2,
                ..cloth()
            },
            Cloth {
                wind_direction: f32::NAN,
                ..cloth()
            },
            Cloth {
                seconds: -0.1,
                ..cloth()
            },
            Cloth {
                slack: 0.25,
                ..cloth()
            },
            Cloth {
                slack: -0.01,
                ..cloth()
            },
            Cloth {
                raise: Some(0.0),
                ..cloth()
            },
            Cloth {
                raise: Some(f32::INFINITY),
                ..cloth()
            },
        ] {
            assert!(!bad.validate(), "{bad:?}");
        }
    }
}
