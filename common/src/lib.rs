//! Shared chr control protocol and local IPC transports.
//!
//! Control messages use [JSON-RPC 2.0](https://www.jsonrpc.org/specification) on every
//! transport. Two local transports carry the same messages:
//!
//! - **NativeSocket**: a Unix Domain Socket. Each message is a 4-byte big-endian payload
//!   length followed by UTF-8 JSON (see [`framing`]). Used by Node.js clients, native tools,
//!   and the Agent.
//! - **WebSocket**: one text message per JSON-RPC message. Used by browsers, which cannot
//!   open Unix sockets. The listener only binds loopback addresses.
//!
//! Both transports feed the same [`server::Handler`], so commands share validation and
//! execution regardless of where they come from. WebRTC media is not part of this crate:
//! only its signaling messages ([`methods::webrtc`]) are, so control clients do not pull in
//! a media stack.

pub mod client;
pub mod framing;
pub mod jsonrpc;
pub mod methods;
pub mod server;
