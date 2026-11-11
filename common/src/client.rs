//! Minimal NativeSocket client for native tools and the Agent.

#[cfg(unix)]
pub use native::NativeSocketClient;

/// Failure of one client call.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Frame(#[from] crate::framing::FrameError),
    #[error(transparent)]
    Rpc(#[from] crate::jsonrpc::ErrorObject),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("connection closed before a response arrived")]
    Closed,
}

#[cfg(unix)]
mod native {
    use std::path::Path;

    use serde_json::Value;
    use tokio::net::UnixStream;

    use super::ClientError;
    use crate::{
        framing::{self, DEFAULT_MAX_MESSAGE_BYTES},
        jsonrpc::{Id, Request, Response},
    };

    /// Sends one request at a time and waits for its response.
    pub struct NativeSocketClient {
        stream: UnixStream,
        next_id: u64,
    }

    impl NativeSocketClient {
        pub async fn connect(path: impl AsRef<Path>) -> std::io::Result<Self> {
            Ok(Self {
                stream: UnixStream::connect(path).await?,
                next_id: 1,
            })
        }

        /// Calls `method` and returns its result, or the server's error object.
        pub async fn call(
            &mut self,
            method: &str,
            params: Option<Value>,
        ) -> Result<Value, ClientError> {
            let id = Id::String(format!("native-{}", self.next_id));
            self.next_id += 1;
            let request = Request {
                method: method.to_owned(),
                params,
                id: Some(id.clone()),
            };
            self.send(&request).await?;
            loop {
                let payload = framing::read_frame(&mut self.stream, DEFAULT_MAX_MESSAGE_BYTES)
                    .await?
                    .ok_or(ClientError::Closed)?;
                let value: Value = serde_json::from_slice(&payload)
                    .map_err(|error| ClientError::Protocol(error.to_string()))?;
                let response = Response::from_value(value)
                    .map_err(|error| ClientError::Protocol(error.message))?;
                // Server-initiated messages could be interleaved in the future; skip them.
                if response.id == id {
                    return Ok(response.outcome?);
                }
            }
        }

        /// Sends a notification. The server executes it but never replies.
        pub async fn notify(
            &mut self,
            method: &str,
            params: Option<Value>,
        ) -> Result<(), ClientError> {
            self.send(&Request {
                method: method.to_owned(),
                params,
                id: None,
            })
            .await
        }

        /// Sends raw bytes as one frame; for tests of malformed input.
        pub async fn send_raw(&mut self, payload: &[u8]) -> Result<Option<Vec<u8>>, ClientError> {
            framing::write_frame(&mut self.stream, payload, DEFAULT_MAX_MESSAGE_BYTES).await?;
            Ok(framing::read_frame(&mut self.stream, DEFAULT_MAX_MESSAGE_BYTES).await?)
        }

        async fn send(&mut self, request: &Request) -> Result<(), ClientError> {
            let payload = request.to_value().to_string();
            framing::write_frame(
                &mut self.stream,
                payload.as_bytes(),
                DEFAULT_MAX_MESSAGE_BYTES,
            )
            .await?;
            Ok(())
        }
    }
}
