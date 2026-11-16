//! chr's side of tracking (`server/docs/tracking-worker.md`): the contract with
//! the tracking worker and what chr does with its answers. The estimation
//! itself stays in the worker (vchao's `pitch-analysis`).
//!
//! - [`protocol`]: vchao's `VchaoFrame` v1 mirror with chr's `lens` and
//!   `frame_warp` extensions, and its boundary validation.
//! - [`pose`]: a returned pose as the camera of a captured frame.
//! - [`lens`]: which camera model a tracked frame is rendered with (#150).
//! - [`join`]: captured frames paired with their cameras, the fixed program
//!   delay and the clean-feed decision.
//! - [`replay`]: the `chr-tracking-sim-cli` stand-in's poses and lens from a
//!   calibrated clip.
//!
//! Pure logic: no processes, sockets, GStreamer or async runtime. chr-runtime
//! supervises the worker, carries frames over its GStreamer unixfd transport
//! and supplies the renderer's geometry checks ([`Checks`]).

pub mod join;
pub mod lens;
pub mod pose;
pub mod protocol;
pub mod replay;

use chr_common::scene_frame as w;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// The renderer's validation of tracked geometry, which every camera passes at
/// the tracking boundary so that the render worker can draw it. chr-runtime
/// supplies chr-renderer's `Ti::new`, `RadialDistortion::new` and
/// `FrameWarp::new`; chr-tracker does not depend on the GPU renderer itself.
#[derive(Clone, Copy)]
pub struct Checks {
    /// A finite, invertible forward row-major ground-to-SNC `Ti`.
    pub ti: fn([[f64; 3]; 3]) -> Result<(), Error>,
    /// A `radial-poly-native-px-v1` lens: finite, a positive scale and a
    /// nonempty support.
    pub lens: fn(&w::RadialLens) -> Result<(), Error>,
    /// A finite, invertible frame warp (reference native pixel to frame SNC).
    pub frame_warp: fn([[f64; 3]; 3]) -> Result<(), Error>,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chr_renderer::{
        coordinates::Ti,
        distortion::{FrameWarp, RadialDistortion, SupportRect},
    };

    /// chr-runtime's checks: the renderer's.
    pub const CHECKS: Checks = Checks {
        ti: |rows| Ok(Ti::new(rows).map(drop)?),
        lens: |lens| {
            Ok(RadialDistortion::new(
                lens.centre_px.map(w::Finite::get),
                lens.scale_px.get(),
                lens.k.map(w::Finite::get),
                lens.support.map(|[min, max]| SupportRect {
                    min: min.map(w::Finite::get),
                    max: max.map(w::Finite::get),
                }),
            )
            .map(drop)?)
        },
        frame_warp: |rows| Ok(FrameWarp::new(rows).map(drop)?),
    };
}
