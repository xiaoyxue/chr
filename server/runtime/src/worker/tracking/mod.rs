//! The tracking worker: a process that `chr-server` starts, hands every captured
//! frame to and supervises, and that returns each frame with its camera.
//!
//! ```text
//! chr-server ── frame N + VchaoFrame{frame_id, selection: null} ──► tracking worker
//!            ◄── frame N + VchaoFrame{selection: {pose: t | unavailable}} ──┘
//! ```
//!
//! The contract is vchao's `VchaoFrame` v1 raw-frame boundary, mirrored in
//! [`chr_tracker::protocol`], so the real tracking module, vchao's
//! `pitch-analysis` (#148), runs unchanged. [`wire`] carries it on GStreamer
//! buffers over two unixfd sockets ([`transport`](crate::worker::transport)).
//! The frame is the memory the render worker receives too: the server copies
//! each captured frame once. [`supervisor`] is chr-server's side.
//! `chr-tracking-sim-cli` (`src/tracking_sim_cli.rs`) is a stand-in worker that
//! returns a calibrated clip's recorded `Ti` for every frame
//! ([`chr_tracker::replay`]); it estimates nothing.

pub mod supervisor;
pub mod wire;
