//! The out-of-process workers that `chr-server` starts and supervises: the
//! renderer (#94, #97) here, and the tracking worker in [`tracking`] (#128).
//!
//! `chr-server` keeps the output clock, scheduling and scene state and starts one
//! `chr-renderer-cli worker`. Requests and replies ([`protocol`]) travel over two
//! GStreamer unixfd sockets per worker ([`transport`]): each message is a JSON
//! object in a buffer's custom meta, and input and output pixels are that
//! buffer's memfd-backed memory. [`supervisor`] runs on the server, grants each
//! worker a bounded number of requests in flight and replaces a worker that
//! exits, fails or hangs; [`serve`] is the worker's side.

pub mod protocol;
pub mod serve;
pub mod supervisor;
pub mod tracking;
pub mod transport;
