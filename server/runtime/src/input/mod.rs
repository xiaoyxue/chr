//! Video acquisition adapters; admission and scheduling remain runtime-owned.

pub mod ring;
#[cfg(all(unix, feature = "gstreamer-media"))]
pub mod software;
