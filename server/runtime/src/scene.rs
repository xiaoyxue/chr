//! Authoritative state of the mock scene, advanced in logical (frame) time.

use std::f64::consts::TAU;

use chr_common::methods::triangle::{DEFAULT_ANGULAR_VELOCITY_RAD_PER_S, TriangleState};

/// The rotating triangle. Only the render loop owns and mutates it.
///
/// Time advances by whole frames, not by the wall clock, so the same command sequence at
/// the same frames yields the same angles in real time, slowed down, or stepped.
#[derive(Debug, Clone)]
pub struct Scene {
    fps: u32,
    frame: u64,
    rotating: bool,
    angle_rad: f64,
    angular_velocity_rad_per_s: f64,
}

impl Scene {
    pub fn new(fps: u32) -> Self {
        Self {
            fps,
            frame: 0,
            rotating: false,
            angle_rad: 0.0,
            angular_velocity_rad_per_s: DEFAULT_ANGULAR_VELOCITY_RAD_PER_S,
        }
    }

    /// Starts rotating from the current angle. Restarting replaces the velocity.
    pub fn start_rotation(&mut self, velocity: Option<f64>) -> Result<TriangleState, String> {
        let velocity = velocity.unwrap_or(DEFAULT_ANGULAR_VELOCITY_RAD_PER_S);
        if !velocity.is_finite() {
            return Err("angular_velocity_rad_per_s must be finite".into());
        }
        self.rotating = true;
        self.angular_velocity_rad_per_s = velocity;
        Ok(self.state())
    }

    /// Stops rotating and keeps the current angle.
    pub fn stop_rotation(&mut self) -> TriangleState {
        self.rotating = false;
        self.state()
    }

    pub fn state(&self) -> TriangleState {
        TriangleState {
            rotating: self.rotating,
            angle_rad: self.angle_rad,
            angular_velocity_rad_per_s: self.angular_velocity_rad_per_s,
            frame: self.frame,
        }
    }

    /// Index of the next frame to render.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    pub fn angle_rad(&self) -> f64 {
        self.angle_rad
    }

    /// Moves to the next frame after the current one was rendered.
    pub fn advance(&mut self) {
        self.frame += 1;
        if self.rotating {
            let step = self.angular_velocity_rad_per_s / f64::from(self.fps);
            self.angle_rad = (self.angle_rad + step).rem_euclid(TAU);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_advances_per_frame_and_stops_in_place() {
        let mut scene = Scene::new(4);
        scene.advance();
        assert_eq!(scene.state().angle_rad, 0.0, "idle scene must not rotate");

        let started = scene.start_rotation(Some(std::f64::consts::PI)).unwrap();
        assert_eq!((started.rotating, started.frame), (true, 1));
        for _ in 0..2 {
            scene.advance();
        }
        assert!((scene.angle_rad() - std::f64::consts::FRAC_PI_2).abs() < 1e-12);

        let stopped = scene.stop_rotation();
        assert_eq!((stopped.rotating, stopped.frame), (false, 3));
        scene.advance();
        assert_eq!(scene.angle_rad(), stopped.angle_rad);
    }

    #[test]
    fn angle_wraps_and_supports_reverse_rotation() {
        let mut scene = Scene::new(1);
        scene.start_rotation(Some(-1.0)).unwrap();
        scene.advance();
        assert!((scene.angle_rad() - (TAU - 1.0)).abs() < 1e-12);
        scene.start_rotation(Some(TAU)).unwrap();
        scene.advance();
        assert!((0.0..TAU).contains(&scene.angle_rad()));
        assert!(scene.start_rotation(Some(f64::NAN)).is_err());
        assert!(scene.start_rotation(Some(f64::INFINITY)).is_err());
    }
}
