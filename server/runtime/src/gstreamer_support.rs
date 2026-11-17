//! Shared support for the opt-in software input pipeline.

use crate::sdi::VideoMode;
use gstreamer::{self as gst, prelude::*};

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn validate_mode(mode: VideoMode, max: [u32; 2]) -> Result<(), Error> {
    if mode.width == 0
        || mode.height == 0
        || mode.width > max[0]
        || mode.height > max[1]
        || !mode.width.is_multiple_of(2)
        || !mode.height.is_multiple_of(2)
    {
        return Err("video mode requires even dimensions within the profile limit".into());
    }
    Ok(())
}

pub(crate) fn check_bus(pipeline: &gst::Pipeline) -> Result<(), Error> {
    if let Some(message) = pipeline
        .bus()
        .and_then(|bus| bus.pop_filtered(&[gst::MessageType::Error]))
        && let gst::MessageView::Error(error) = message.view()
    {
        return Err(format!("GStreamer: {}", error.error()).into());
    }
    Ok(())
}
