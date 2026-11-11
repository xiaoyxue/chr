//! IPC server shared by the NativeSocket and WebSocket transports.
//!
//! Each connection processes its messages in arrival order: a request is fully handled and
//! answered before the next message on that connection is read. Commands from one client
//! therefore reach the scene in the order sent. Clients that need concurrency open more
//! connections.

use std::{
    future::Future,
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::{
    self,
    handshake::server::{
        Callback, ErrorResponse, Request as HttpRequest, Response as HttpResponse,
    },
    protocol::WebSocketConfig,
};

use crate::{
    framing::{self, DEFAULT_MAX_MESSAGE_BYTES},
    jsonrpc::{ErrorObject, Incoming, Message, Request, Response, parse_message},
};

/// Which transport a connection arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transport {
    NativeSocket,
    WebSocket,
}

/// Unique for the lifetime of one [`IpcServer`], across both transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(pub u64);

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "conn-{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub id: ConnectionId,
    pub transport: Transport,
}

/// Executes business methods. Both transports call the same handler.
pub trait Handler: Send + Sync + 'static {
    /// Runs one method. Notifications are executed too; their result is discarded.
    fn handle(
        &self,
        connection: &ConnectionInfo,
        method: &str,
        params: Option<Value>,
    ) -> impl Future<Output = Result<Value, ErrorObject>> + Send;

    /// Called once after a connection closes, for cleaning up per-connection resources.
    fn disconnected(&self, _connection: &ConnectionInfo) {}
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Upper bound for one incoming or outgoing message, in bytes.
    pub max_message_bytes: usize,
    /// Browser origins accepted in addition to loopback origins
    /// (`http(s)://localhost`, `127.0.0.1`, or `[::1]`, any port).
    pub extra_allowed_origins: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            extra_allowed_origins: Vec::new(),
        }
    }
}

/// Serves one handler over any number of listeners.
pub struct IpcServer<H> {
    handler: Arc<H>,
    config: Arc<ServerConfig>,
    next_connection: Arc<AtomicU64>,
}

impl<H> Clone for IpcServer<H> {
    fn clone(&self) -> Self {
        Self {
            handler: Arc::clone(&self.handler),
            config: Arc::clone(&self.config),
            next_connection: Arc::clone(&self.next_connection),
        }
    }
}

impl<H: Handler> IpcServer<H> {
    pub fn new(handler: Arc<H>, config: ServerConfig) -> Self {
        Self {
            handler,
            config: Arc::new(config),
            next_connection: Arc::new(AtomicU64::new(1)),
        }
    }

    fn connection(&self, transport: Transport) -> ConnectionInfo {
        ConnectionInfo {
            id: ConnectionId(self.next_connection.fetch_add(1, Ordering::Relaxed)),
            transport,
        }
    }

    /// Accepts NativeSocket connections until the listener fails.
    #[cfg(unix)]
    pub async fn serve_native_socket(&self, listener: NativeSocketListener) -> io::Result<()> {
        loop {
            let stream = match listener.listener.accept().await {
                Ok((stream, _)) => stream,
                Err(error) => {
                    log::warn!("native socket accept failed: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let server = self.clone();
            let connection = self.connection(Transport::NativeSocket);
            tokio::spawn(async move {
                log::info!("{} opened (native socket)", connection.id);
                if let Err(error) = server.run_native_socket(&connection, stream).await {
                    log::warn!("{} failed: {error}", connection.id);
                }
                server.handler.disconnected(&connection);
                log::info!("{} closed", connection.id);
            });
        }
    }

    #[cfg(unix)]
    async fn run_native_socket(
        &self,
        connection: &ConnectionInfo,
        mut stream: tokio::net::UnixStream,
    ) -> Result<(), framing::FrameError> {
        let max = self.config.max_message_bytes;
        while let Some(payload) = framing::read_frame(&mut stream, max).await? {
            if let Some(reply) = self.process(connection, &payload).await {
                framing::write_frame(&mut stream, reply.as_bytes(), max).await?;
            }
        }
        Ok(())
    }

    /// Accepts WebSocket connections until the listener fails.
    pub async fn serve_websocket(&self, listener: WebSocketListener) -> io::Result<()> {
        loop {
            let stream = match listener.listener.accept().await {
                Ok((stream, _)) => stream,
                Err(error) => {
                    log::warn!("websocket accept failed: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let server = self.clone();
            let connection = self.connection(Transport::WebSocket);
            tokio::spawn(async move {
                match server.run_websocket(&connection, stream).await {
                    Ok(()) => {
                        server.handler.disconnected(&connection);
                        log::info!("{} closed", connection.id);
                    }
                    Err(WebSocketError::Handshake(error)) => {
                        log::warn!("{} rejected during handshake: {error}", connection.id);
                    }
                    Err(WebSocketError::Session(error)) => {
                        log::warn!("{} failed: {error}", connection.id);
                        server.handler.disconnected(&connection);
                    }
                }
            });
        }
    }

    async fn run_websocket(
        &self,
        connection: &ConnectionInfo,
        stream: TcpStream,
    ) -> Result<(), WebSocketError> {
        let max = self.config.max_message_bytes;
        let check_origin = OriginCheck {
            extra: self.config.extra_allowed_origins.clone(),
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(max))
            .max_frame_size(Some(max));
        let mut socket =
            tokio_tungstenite::accept_hdr_async_with_config(stream, check_origin, Some(config))
                .await
                .map_err(WebSocketError::Handshake)?;
        log::info!("{} opened (websocket)", connection.id);
        while let Some(message) = socket.next().await {
            let payload = match message.map_err(WebSocketError::Session)? {
                tungstenite::Message::Text(text) => text.as_bytes().to_vec(),
                tungstenite::Message::Binary(bytes) => bytes.to_vec(),
                tungstenite::Message::Close(_) => break,
                _ => continue,
            };
            if let Some(reply) = self.process(connection, &payload).await {
                socket
                    .send(tungstenite::Message::text(reply))
                    .await
                    .map_err(WebSocketError::Session)?;
            }
        }
        Ok(())
    }

    /// Handles one raw message and returns the serialized reply, if one is due.
    async fn process(&self, connection: &ConnectionInfo, payload: &[u8]) -> Option<String> {
        match parse_message(payload) {
            Message::Single(item) => self
                .process_one(connection, item)
                .await
                .map(|response| response.to_value().to_string()),
            Message::Batch(items) => {
                let mut replies = Vec::new();
                for item in items {
                    if let Some(response) = self.process_one(connection, item).await {
                        replies.push(response.to_value());
                    }
                }
                (!replies.is_empty()).then(|| Value::Array(replies).to_string())
            }
        }
    }

    async fn process_one(&self, connection: &ConnectionInfo, item: Incoming) -> Option<Response> {
        match item {
            Incoming::Invalid { id, error } => Some(Response {
                id,
                outcome: Err(error),
            }),
            Incoming::Request(Request { method, params, id }) => {
                let outcome = self.handler.handle(connection, &method, params).await;
                if let Err(error) = &outcome {
                    log::debug!("{} {method} failed: {error}", connection.id);
                }
                id.map(|id| Response { id, outcome })
            }
        }
    }
}

/// Handshake callback. Non-browser clients send no Origin header and are accepted; the
/// loopback-only bind already limits them to this machine.
struct OriginCheck {
    extra: Vec<String>,
}

impl Callback for OriginCheck {
    fn on_request(
        self,
        request: &HttpRequest,
        response: HttpResponse,
    ) -> Result<HttpResponse, ErrorResponse> {
        let Some(origin) = request.headers().get("origin") else {
            return Ok(response);
        };
        let origin = origin.to_str().unwrap_or("");
        if origin_allowed(origin, &self.extra) {
            return Ok(response);
        }
        let mut rejection = ErrorResponse::new(Some(format!("origin {origin:?} is not allowed")));
        *rejection.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
        Err(rejection)
    }
}

#[derive(Debug, thiserror::Error)]
enum WebSocketError {
    #[error("{0}")]
    Handshake(tungstenite::Error),
    #[error("{0}")]
    Session(tungstenite::Error),
}

/// Accepts an exact extra origin, or any loopback `http(s)` origin without a path.
fn origin_allowed(origin: &str, extra: &[String]) -> bool {
    if extra.iter().any(|allowed| allowed == origin) {
        return true;
    }
    let Some(authority) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let (host, rest) = if authority.starts_with('[') {
        match authority.find(']') {
            Some(end) => authority.split_at(end + 1),
            None => return false,
        }
    } else {
        authority.split_at(authority.find(':').unwrap_or(authority.len()))
    };
    let port_ok = rest.is_empty()
        || rest
            .strip_prefix(':')
            .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()));
    port_ok && matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// A TCP listener for WebSocket clients, restricted to loopback addresses.
pub struct WebSocketListener {
    listener: TcpListener,
}

impl WebSocketListener {
    /// Binds `addr`. Non-loopback addresses are rejected: browsers on this machine reach
    /// the server through loopback, and nothing else should.
    pub async fn bind(addr: SocketAddr) -> io::Result<Self> {
        if !addr.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("websocket address {addr} is not a loopback address"),
            ));
        }
        Ok(Self {
            listener: TcpListener::bind(addr).await?,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

#[cfg(unix)]
pub use native::NativeSocketListener;

#[cfg(unix)]
mod native {
    use std::{
        io,
        os::unix::fs::{FileTypeExt, PermissionsExt},
        path::{Path, PathBuf},
    };

    use tokio::net::{UnixListener, UnixStream};

    /// A Unix Domain Socket listener. The socket file is owner-only (mode 0600) and is
    /// removed on drop. Put it in a directory only the owner can enter, such as
    /// `$XDG_RUNTIME_DIR`, so no other user can connect before the mode is applied.
    pub struct NativeSocketListener {
        pub(super) listener: UnixListener,
        path: PathBuf,
    }

    impl NativeSocketListener {
        /// Binds `path`, replacing a stale socket file left by a crashed server. Fails if
        /// another server still accepts connections there or if the path is not a socket.
        pub async fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
            let path = path.as_ref().to_path_buf();
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_socket() => {
                    if UnixStream::connect(&path).await.is_ok() {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            format!("another server is listening on {}", path.display()),
                        ));
                    }
                    std::fs::remove_file(&path)?;
                }
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{} exists and is not a socket", path.display()),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let listener = UnixListener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self { listener, path })
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for NativeSocketListener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::origin_allowed;

    #[test]
    fn accepts_only_loopback_or_listed_origins() {
        for origin in [
            "http://localhost",
            "http://localhost:5173",
            "https://127.0.0.1:8443",
            "http://[::1]:3000",
        ] {
            assert!(origin_allowed(origin, &[]), "{origin}");
        }
        for origin in [
            "null",
            "http://example.com",
            "http://localhost.example.com",
            "http://127.0.0.1.nip.io",
            "http://localhost:5173/path",
            "http://localhost:",
            "ws://localhost",
        ] {
            assert!(!origin_allowed(origin, &[]), "{origin}");
        }
        assert!(origin_allowed(
            "http://studio.lan:8080",
            &["http://studio.lan:8080".to_owned()]
        ));
    }
}
