//! The out-of-process renderer (#94, #97).
//!
//! `chr-server` keeps the output clock, scheduling and scene state and starts one
//! `chr-renderer-cli worker`. Requests and replies are JSON lines on the worker's
//! stdin/stdout ([`protocol`]); input and output pixels travel through two
//! `chr-shm` rings. [`supervisor`] runs on the server and replaces a worker that
//! exits, fails or hangs; [`serve`] is the worker's side.

pub mod protocol;
pub mod serve;
pub mod supervisor;
