//! The tracking worker: a process that `chr-server` starts, hands every captured
//! frame to and supervises, and that answers each frame with its camera.
//!
//! ```text
//! chr-server ── `track` N with the frame's shared memory ──► tracking worker
//!            ◄── `camera` (Ti, C, frame_warp of frame N) | `unavailable` ──┘
//! ```
//!
//! [`protocol`] is the language-neutral contract: JSON messages and RGBA8 frames
//! over two GStreamer unixfd sockets ([`transport`](crate::worker::transport)),
//! so the real tracking module may be written in another language, as long as
//! it uses GStreamer's `unixfdsrc`/`unixfdsink`. The frame is the memory the
//! render worker receives too: the server copies each captured frame once.
//! [`supervisor`] is chr-server's side. `chr-tracking-sim-cli` (`src/tracking_sim_cli.rs`) is a stand-in worker that
//! replays a calibrated clip's recorded cameras
//! ([`TrackingReplay`](crate::calibration::tracking::TrackingReplay)); it
//! estimates nothing.

pub mod protocol;
pub mod supervisor;
