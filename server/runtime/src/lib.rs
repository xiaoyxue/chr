//! Runtime integration boundaries and an explicit synthetic program-output opt-in.
pub mod calibration;
pub mod control;
pub mod frame_resources;
pub mod frame_submissions;
#[cfg(all(unix, feature = "gstreamer-media"))]
mod gstreamer_support;
#[cfg(all(unix, feature = "gstreamer-media"))]
pub mod input;
pub mod pipeline;
pub mod program;
pub mod retained;
mod scene;
pub mod sdi;
