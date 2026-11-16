//! Startup-only lens calibration for a validated, looping CFR clip; not live tracking.

use chr_common::scene_frame as w;
use chr_renderer::{
    coordinates::ImageExtent,
    distortion::{CameraModel, FrameWarp, LensDistortion, RadialDistortion, SupportRect},
};
use serde::Deserialize;

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Radial {
    pub centre_px: [f64; 2],
    pub scale_px: f64,
    pub coefficients: [f64; 3],
    pub support: Option<[[f64; 2]; 2]>,
}

impl Radial {
    /// The startup lens shared by every frame of a clip.
    pub fn distortion(&self) -> Result<RadialDistortion, Error> {
        Ok(RadialDistortion::new(
            self.centre_px,
            self.scale_px,
            self.coefficients,
            self.support.map(|[min, max]| SupportRect { min, max }),
        )?)
    }

    /// The same lens as tracking data carries it, validated.
    pub fn wire(&self) -> Result<w::RadialLens, Error> {
        super::lens::wire(
            self.centre_px,
            self.scale_px,
            self.coefficients,
            self.support,
        )
    }

    pub(crate) fn models(
        self,
        extent: ImageExtent,
        frame_warps: &[[[f64; 3]; 3]],
    ) -> Result<Vec<CameraModel>, Error> {
        let radial = self.distortion()?;
        frame_warps
            .iter()
            .map(|rows| {
                Ok(CameraModel::Distorted(Box::new(LensDistortion::new(
                    extent,
                    radial,
                    Some(FrameWarp::new(*rows)?),
                ))))
            })
            .collect()
    }
}

/// Bound to the native producer, not output slots or mutable scene revisions.
pub struct VideoLens {
    stream: w::Id,
    models: Vec<CameraModel>,
}

impl VideoLens {
    /// `models` holds one entry per decoded clip frame; callers reject empty clips.
    pub(crate) fn new(stream: w::Id, models: Vec<CameraModel>) -> Self {
        Self { stream, models }
    }

    pub fn frame_count(&self) -> usize {
        self.models.len()
    }

    pub fn model(&self, frame: &w::FrameKey) -> Result<&CameraModel, Error> {
        if frame.stream_id != self.stream {
            return Err("calibrated video requires its registered native input stream".into());
        }
        // The decoder loops an exact, startup-validated CFR clip. Sequence is
        // assigned before queue drops, so gaps never shift the calibration.
        Ok(&self.models[(frame.sequence.get() % self.models.len() as u64) as usize])
    }
}
