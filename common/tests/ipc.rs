#![cfg(unix)]

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chr_common::{
    client::{ClientError, NativeSocketClient},
    jsonrpc::{ErrorObject, INVALID_PARAMS, METHOD_NOT_FOUND, PARSE_ERROR},
    server::{
        ConnectionInfo, Handler, IpcServer, NativeSocketListener, ServerConfig, Transport,
        WebSocketListener,
    },
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

/// Records every executed method so tests can check ordering and shared state.
#[derive(Default)]
struct Recorder {
    log: Mutex<Vec<(Transport, String)>>,
    disconnected: Mutex<Vec<ConnectionInfo>>,
}

impl Handler for Recorder {
    async fn handle(
        &self,
        connection: &ConnectionInfo,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, ErrorObject> {
        self.log
            .lock()
            .unwrap()
            .push((connection.transport, method.to_owned()));
        match method {
            "test.echo" => Ok(params.unwrap_or(Value::Null)),
            "test.count" => Ok(json!(self.log.lock().unwrap().len())),
            "test.slow" => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(json!("slow"))
            }
            "test.strict" => Err(ErrorObject::invalid_params("bad")),
            _ => Err(ErrorObject::method_not_found(method)),
        }
    }

    fn disconnected(&self, connection: &ConnectionInfo) {
        self.disconnected.lock().unwrap().push(*connection);
    }
}

fn socket_path(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "chr-ipc-test-{}-{}-{name}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

struct Fixture {
    recorder: Arc<Recorder>,
    socket: PathBuf,
    ws_url: String,
}

async fn start() -> Fixture {
    let recorder = Arc::new(Recorder::default());
    let server = IpcServer::new(Arc::clone(&recorder), ServerConfig::default());
    let socket = socket_path("server");
    let native = NativeSocketListener::bind(&socket).await.unwrap();
    let ws = WebSocketListener::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let ws_url = format!("ws://{}", ws.local_addr().unwrap());
    let native_server = server.clone();
    tokio::spawn(async move { native_server.serve_native_socket(native).await });
    tokio::spawn(async move { server.serve_websocket(ws).await });
    Fixture {
        recorder,
        socket,
        ws_url,
    }
}

async fn ws_call(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    text: &str,
) -> Value {
    socket.send(Message::text(text)).await.unwrap();
    loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Text(reply) => return serde_json::from_str(reply.as_str()).unwrap(),
            _ => continue,
        }
    }
}

#[tokio::test]
async fn native_socket_round_trips_and_reports_errors() {
    let fixture = start().await;
    let mut client = NativeSocketClient::connect(&fixture.socket).await.unwrap();
    assert_eq!(
        client
            .call("test.echo", Some(json!({"a": 1})))
            .await
            .unwrap(),
        json!({"a": 1})
    );
    let error = client.call("test.missing", None).await.unwrap_err();
    assert!(matches!(
        error,
        ClientError::Rpc(ErrorObject {
            code: METHOD_NOT_FOUND,
            ..
        })
    ));
    let error = client.call("test.strict", None).await.unwrap_err();
    assert!(matches!(
        error,
        ClientError::Rpc(ErrorObject {
            code: INVALID_PARAMS,
            ..
        })
    ));

    // A notification executes without a reply; the next call still pairs correctly.
    client.notify("test.echo", None).await.unwrap();
    assert_eq!(client.call("test.count", None).await.unwrap(), json!(5));

    let reply = client.send_raw(b"{oops").await.unwrap().unwrap();
    let reply: Value = serde_json::from_slice(&reply).unwrap();
    assert_eq!(reply["error"]["code"], json!(PARSE_ERROR));
    assert_eq!(reply["id"], Value::Null);

    let batch = br#"[{"jsonrpc":"2.0","method":"test.echo","params":[1],"id":"a"},
                    {"jsonrpc":"2.0","method":"test.echo"},
                    {"jsonrpc":"2.0","method":"test.slow","id":"b"}]"#;
    let reply: Value =
        serde_json::from_slice(&client.send_raw(batch).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        reply,
        json!([
            {"jsonrpc": "2.0", "result": [1], "id": "a"},
            {"jsonrpc": "2.0", "result": "slow", "id": "b"}
        ])
    );
}

#[tokio::test]
async fn one_connection_executes_in_send_order() {
    let fixture = start().await;
    let mut socket = tokio::net::UnixStream::connect(&fixture.socket)
        .await
        .unwrap();
    // Write both requests before reading: the slow one must still finish first.
    for (method, id) in [("test.slow", "1"), ("test.echo", "2")] {
        let request = json!({"jsonrpc": "2.0", "method": method, "id": id}).to_string();
        chr_common::framing::write_frame(&mut socket, request.as_bytes(), 1024)
            .await
            .unwrap();
    }
    for expected in ["1", "2"] {
        let reply = chr_common::framing::read_frame(&mut socket, 1024)
            .await
            .unwrap()
            .unwrap();
        let reply: Value = serde_json::from_slice(&reply).unwrap();
        assert_eq!(reply["id"], json!(expected));
    }
    let log = fixture.recorder.log.lock().unwrap();
    let methods: Vec<_> = log.iter().map(|(_, method)| method.as_str()).collect();
    assert_eq!(methods, ["test.slow", "test.echo"]);
}

#[tokio::test]
async fn both_transports_share_one_handler_and_report_disconnects() {
    let fixture = start().await;
    let mut native = NativeSocketClient::connect(&fixture.socket).await.unwrap();
    native.call("test.echo", None).await.unwrap();

    let (mut ws, _) = tokio_tungstenite::connect_async(&fixture.ws_url)
        .await
        .unwrap();
    let reply = ws_call(
        &mut ws,
        r#"{"jsonrpc":"2.0","method":"test.count","id":"w1"}"#,
    )
    .await;
    assert_eq!(reply, json!({"jsonrpc": "2.0", "result": 2, "id": "w1"}));

    let transports: Vec<_> = fixture
        .recorder
        .log
        .lock()
        .unwrap()
        .iter()
        .map(|(transport, _)| *transport)
        .collect();
    assert_eq!(transports, [Transport::NativeSocket, Transport::WebSocket]);

    ws.close(None).await.unwrap();
    drop(native);
    for _ in 0..50 {
        if fixture.recorder.disconnected.lock().unwrap().len() == 2 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("disconnects were not reported");
}

#[tokio::test]
async fn websocket_rejects_foreign_origins() {
    let fixture = start().await;
    let mut request = fixture.ws_url.as_str().into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "http://evil.example".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(request).await.is_err());

    let mut request = fixture.ws_url.as_str().into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:5173".parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let reply = ws_call(&mut ws, r#"{"jsonrpc":"2.0","method":"test.echo","id":1}"#).await;
    assert_eq!(reply["result"], Value::Null);
    assert!(fixture.recorder.disconnected.lock().unwrap().is_empty());
}

#[tokio::test]
async fn listeners_enforce_local_binding() {
    assert!(
        WebSocketListener::bind("0.0.0.0:0".parse().unwrap())
            .await
            .is_err()
    );

    let path = socket_path("stale");
    // A socket file nobody listens on is replaced.
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    let live = NativeSocketListener::bind(&path).await.unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // A live server is never displaced.
    let error = NativeSocketListener::bind(&path).await.err().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
    drop(live);
    assert!(!path.exists());

    let file = socket_path("plain");
    std::fs::write(&file, b"").unwrap();
    assert!(NativeSocketListener::bind(&file).await.is_err());
    std::fs::remove_file(&file).unwrap();
}
