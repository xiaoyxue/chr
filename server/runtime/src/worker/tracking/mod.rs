//! The tracking worker: a process that `chr-server` starts, hands every captured
//! frame to and supervises, and that answers each frame with its camera.
//!
//! ```text
//! chr-server ── frame N: shared-memory ring + `track` line ──► tracking worker
//!            ◄── `camera` (Ti, C, frame_warp of frame N) | `unavailable` ──┘
//! ```
//!
//! [`protocol`] is the language-neutral contract: JSON lines on the worker's
//! stdin/stdout and a `chr-shm` ring of RGBA8 frames, so the real tracking module
//! may be written in another language. [`supervisor`] is chr-server's side.
//! `chr-tracking-sim-cli` (`src/tracking_sim_cli.rs`) is a stand-in worker that
//! replays a calibrated clip's recorded cameras
//! ([`TrackingReplay`](crate::calibration::tracking::TrackingReplay)); it
//! estimates nothing.

pub mod protocol;
pub mod supervisor;
