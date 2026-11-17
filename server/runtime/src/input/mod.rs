//! Video acquisition adapters; admission and scheduling remain runtime-owned.
//! The live capture is `chr-sdi-in`'s, over unixfd to the GStreamer core
//! ([`crate::gst_core::input`]); this is the generated `--software-video`
//! program's in-process file input.

#[cfg(all(unix, feature = "gstreamer-media"))]
pub mod software;
