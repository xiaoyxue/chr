//! Approved `scene_frame_v1` payloads, not a runtime implementation.
//!
//! Call [`parse_params`] at the shared handler boundary. It rejects shape and
//! metadata errors before resource resolution. Camera/material/light math,
//! installed limits, authorization and state transitions remain runtime duties.
//! DTO decoding alone is not scene application, submission or transmission.

mod core;
mod types;
mod validation;
mod wire;

pub use core::*;
pub use types::*;
pub use wire::*;
