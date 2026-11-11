//! Approved `scene_frame_v1` payloads, not a runtime implementation.
//!
//! Call [`parse_params`] at the shared handler boundary. It rejects shape and
//! metadata errors before resource resolution. Camera/material/light math,
//! installed limits, authorization and state transitions remain runtime duties.
//! DTO decoding alone is not scene application, submission or transmission.

// Serde's derived struct decoder also accepts positional arrays. The wire
// contract requires objects, including for nested metadata, on every transport.
macro_rules! object {
    ($(#[$attr:meta])* $name:ident { $($(#[$meta:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        object!(@define (pub) $(#[$attr])* $name { $($(#[$meta])* $field: $ty),* });
    };
    (@private $name:ident { $($(#[$meta:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        object!(@define () $name { $($(#[$meta])* $field: $ty),* });
    };
    (@define ($($vis:tt)*) $(#[$attr:meta])* $name:ident { $($(#[$meta:meta])* $field:ident : $ty:ty),* }) => {
        #[derive(Debug, Clone, PartialEq, serde::Serialize)]
        $(#[$attr])*
        $($vis)* struct $name { $($(#[$meta])* pub $field: $ty),* }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Fields { $($(#[$meta])* $field: $ty),* }
                let fields: Fields = $crate::scene_frame::core::deserialize_object(deserializer)?;
                Ok(Self { $($field: fields.$field),* })
            }
        }
    };
}

mod core;
mod types;
mod validation;
mod wire;

pub use core::*;
pub use types::*;
pub use wire::*;
