//! The renderer's side of a tracked frame's lens. Which lens a frame is
//! rendered with is `chr_tracker::lens`'s choice
//! (`server/docs/tracking-worker.md#lens`); this module turns the wire lens into
//! the renderer's and gives chr-tracker the renderer's geometry checks.

use chr_common::scene_frame as w;
use chr_renderer::{
    coordinates::Ti,
    distortion::{FrameWarp, RadialDistortion, SupportRect},
};
use chr_tracker::Checks;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// The renderer's lens of a wire lens: finite, a positive scale and a nonempty
/// support, as [`RadialDistortion::new`] requires. Its monotonic radius is
/// computed there too; points beyond it are never drawn.
pub fn radial(lens: &w::RadialLens) -> Result<RadialDistortion, Error> {
    Ok(RadialDistortion::new(
        lens.centre_px.map(w::Finite::get),
        lens.scale_px.get(),
        lens.k.map(w::Finite::get),
        lens.support.map(|[min, max]| SupportRect {
            min: min.map(w::Finite::get),
            max: max.map(w::Finite::get),
        }),
    )?)
}

/// Tracked geometry is accepted only if the renderer can draw it: its own
/// `Ti`, lens and frame warp constructors validate it.
pub const CHECKS: Checks = Checks {
    ti: |rows| Ok(Ti::new(rows).map(drop)?),
    lens: |lens| radial(lens).map(drop),
    frame_warp: |rows| Ok(FrameWarp::new(rows).map(drop)?),
};
