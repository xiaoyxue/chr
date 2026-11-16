//! The tracking worker: a process that `chr-server` starts, hands every captured
//! frame to and supervises, and that returns each frame with its camera.
//!
//! ```text
//! chr-server ── frame N + VchaoFrame{frame_id, selection: null} ──► tracking worker
//!            ◄── frame N + VchaoFrame{selection: {pose: t | unavailable}} ──┘
//! ```
//!
//! [`protocol`] is vchao's `VchaoFrame` v1 raw-frame contract over two
//! GStreamer unixfd sockets ([`transport`](crate::worker::transport)), so the
//! real tracking module, vchao's `pitch-analysis` (#148), runs unchanged. The
//! frame is the memory the render worker receives too: the server copies each
//! captured frame once. [`supervisor`] is chr-server's side.
//! `chr-tracking-sim-cli` (`src/tracking_sim_cli.rs`) is a stand-in worker that
//! returns a calibrated clip's recorded `Ti` for every frame
//! ([`TrackingReplay`](crate::calibration::tracking::TrackingReplay)); it
//! estimates nothing.

pub mod protocol;
pub mod supervisor;
