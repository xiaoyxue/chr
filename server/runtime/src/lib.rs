//! Runtime integration boundaries and an explicit synthetic program-output opt-in.
pub mod calibration;
pub mod control;
pub mod frame_resources;
pub mod frame_submissions;
pub mod gst_core;
#[cfg(all(unix, feature = "gstreamer-media"))]
mod gstreamer_support;
pub mod input;
pub mod pipeline;
pub mod program;
pub mod retained;
mod scene;
pub mod sdi;
pub mod worker;
