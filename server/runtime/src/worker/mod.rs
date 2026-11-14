//! The out-of-process workers that `chr-server` starts and supervises: the
//! renderer (#94, #97) here, and the tracking worker in [`tracking`] (#128).
//!
//! `chr-server` keeps the output clock, scheduling and scene state and starts one
//! `chr-renderer-cli worker`. Requests and replies are JSON lines on the worker's
//! stdin/stdout ([`protocol`]); input and output pixels travel through two
//! `chr-shm` rings. [`supervisor`] runs on the server and replaces a worker that
//! exits, fails or hangs; [`serve`] is the worker's side.

pub mod protocol;
pub mod serve;
pub mod supervisor;
pub mod tracking;
