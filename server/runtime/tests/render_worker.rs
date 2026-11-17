//! Render worker acceptance on a hardware GPU: the worker renders exactly what the
//! in-process bridge renders, and `chr-server` keeps its output cadence while the
//! worker is killed, stopped or too slow.
use chr_common::scene_frame as w;
use chr_renderer::Gpu;
use chr_runtime::{
    program::{
        Frame, ProgramRenderer,
        synthetic::{Synthetic, runtime_id},
    },
    retained::SceneControl,
    sdi::VideoMode,
    worker::{
        protocol::{Output, WorkerProgram},
        serve::DELAY_ENV,
        supervisor::{Supervisor, WorkerConfig, status_handle},
    },
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

const MODE: VideoMode = VideoMode {
    width: 192,
    height: 128,
    fps: chr_common::rate::Rate::fps(50),
};
const CLI: &str = env!("CARGO_BIN_EXE_chr-renderer-cli");

fn temp_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("chr-render-worker-{name}-{}", std::process::id()))
}

#[tokio::test]
#[ignore = "requires a native hardware GPU"]
async fn gpu_worker_renders_exactly_what_the_in_process_bridge_renders() {
    let gpu = Gpu::request(&Gpu::instance(), None).await.unwrap();
    let (mut synthetic, adapter) = Synthetic::install(MODE, runtime_id().unwrap()).unwrap();
    let (control, owner) = SceneControl::spawn(adapter, 64).unwrap();
    synthetic.create_scene(&control).await.unwrap();
    synthetic.inject(&control).await.unwrap().unwrap();
    let execution = synthetic.scheduler.next_ready().unwrap().unwrap();
    let mut renderer = ProgramRenderer::new(&gpu, &synthetic.configuration).unwrap();
    let local = renderer
        .render_frame(&gpu, &Frame::of(&execution).unwrap())
        .unwrap();

    let mut supervisor = Supervisor::new(
        WorkerConfig {
            executable: CLI.into(),
            program: WorkerProgram::Synthetic,
            runtime_id: synthetic.configuration.runtime_id.clone(),
            output: Output {
                width: MODE.width,
                height: MODE.height,
                fps: MODE.fps,
            },
            socket_dir: temp_dir("parity"),
            hang: Duration::from_secs(10),
            assets: Default::default(),
            host: Default::default(),
        },
        status_handle(),
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    while !supervisor.is_ready() {
        assert!(Instant::now() < until, "the worker did not become ready");
        supervisor.maintain();
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut remote = |execution: &chr_runtime::frame_submissions::Execution| {
        supervisor
            .render(
                &execution.input.metadata().image,
                execution.input.stored().unwrap(),
                &execution.provenance.input,
                execution.scene.snapshot(),
                MODE.fps.time_base(),
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap()
    };
    let first = remote(&execution);
    assert_eq!(first.len(), local.len());
    assert!(first == local, "worker and in-process UYVY differ");
    execution.failed().unwrap();

    // The same scene ID recreated with other content starts at revision 0 again;
    // the worker must not reuse its binding of the released scene.
    let target =
        json!({"runtime_id": synthetic.configuration.runtime_id, "scene_id": synthetic.scene_id});
    control.call(w::SCENE_RELEASE, Some(target)).await.unwrap();
    let mut empty = synthetic.initial_state().clone();
    empty.instances.clear();
    synthetic.create_scene_with(&control, &empty).await.unwrap();
    synthetic.inject(&control).await.unwrap().unwrap();
    let execution = synthetic.scheduler.next_ready().unwrap().unwrap();
    assert_eq!(execution.scene.snapshot().scene_revision.get(), 0);
    let local = renderer
        .render_frame(&gpu, &Frame::of(&execution).unwrap())
        .unwrap();
    let second = remote(&execution);
    assert!(second != first, "the recreated scene renders differently");
    assert!(second == local, "worker reused the released scene");
    execution.failed().unwrap();
    drop(supervisor);
    control.shutdown().await;
    owner.await.unwrap();
}

/// A `chr-server` process with its WebSocket control connection.
struct Server {
    child: Child,
    ws: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    next: u64,
}

impl Server {
    async fn start(extra: &[&str], env: &[(&str, &str)]) -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let address = format!("127.0.0.1:{port}");
        let child = Command::new(env!("CARGO_BIN_EXE_chr-server"))
            .args([
                "--synthetic-program",
                "--size",
                "192x128",
                "--fps",
                "50",
                "--ws",
                &address,
                "--no-socket",
                "--monitor-tap",
                "none",
                "--worker-hang-ms",
                "500",
                "--renderer-cli",
                CLI,
            ])
            .args(extra)
            .envs(env.iter().copied())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(30);
        let ws = loop {
            match tokio_tungstenite::connect_async(format!("ws://{address}")).await {
                Ok((ws, _)) => break ws,
                Err(_) if Instant::now() < until => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                Err(error) => panic!("chr-server did not start: {error}"),
            }
        };
        Self { child, ws, next: 0 }
    }

    async fn call(&mut self, method: &str) -> Value {
        self.next += 1;
        let request = json!({"jsonrpc": "2.0", "id": self.next, "method": method});
        self.ws
            .send(Message::text(request.to_string()))
            .await
            .unwrap();
        let reply: Value =
            serde_json::from_str(self.ws.next().await.unwrap().unwrap().to_text().unwrap())
                .unwrap();
        reply["result"].clone()
    }

    async fn worker(&mut self) -> Value {
        self.call("render.get_worker_status").await
    }

    async fn frames_output(&mut self) -> u64 {
        self.call("sdi.get_status").await["frames_output"]
            .as_u64()
            .unwrap()
    }

    /// Waits until the worker is ready after `restarts` restarts.
    async fn ready_after(&mut self, restarts: u64) -> Value {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let status = self.worker().await;
            if status["state"] == "ready" && status["restarts"] == restarts {
                return status;
            }
            assert!(Instant::now() < until, "worker not ready again: {status}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Output slots and clean-feed frames over `period`.
    async fn measure(&mut self, period: Duration) -> (u64, u64) {
        let (slots, clean) = (self.frames_output().await, self.clean_feed().await);
        tokio::time::sleep(period).await;
        (
            self.frames_output().await - slots,
            self.clean_feed().await - clean,
        )
    }

    /// Waits until the worker renders every frame: the GPU warm-up of its first
    /// frames can outlast their deadlines.
    async fn settle(&mut self) {
        let until = Instant::now() + Duration::from_secs(15);
        while self.measure(Duration::from_millis(200)).await.1 > 0 {
            assert!(
                Instant::now() < until,
                "the worker never renders every frame"
            );
        }
    }

    async fn clean_feed(&mut self) -> u64 {
        self.worker().await["clean_feed_frames"].as_u64().unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = signal("INT", self.child.id());
        let until = Instant::now() + Duration::from_secs(5);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn signal(name: &str, pid: u32) -> std::io::Result<std::process::ExitStatus> {
    Command::new("kill")
        .args([format!("-{name}"), pid.to_string()])
        .status()
}

fn pid(status: &Value) -> u32 {
    status["pid"].as_u64().unwrap() as u32
}

/// Slots in `period` at 50 fps, allowing scheduling jitter.
fn at_cadence(slots: u64, period: Duration) -> bool {
    let expected = period.as_secs_f64() * 50.;
    (slots as f64) > expected * 0.9 && (slots as f64) < expected * 1.1 + 2.
}

#[tokio::test]
#[ignore = "requires a native hardware GPU"]
async fn gpu_output_keeps_its_cadence_while_the_worker_is_killed_or_stopped() {
    // A generous deadline: this test is about failures, not render time in debug builds.
    let mut server = Server::start(&["--render-deadline-ms", "200"], &[]).await;
    let ready = server.ready_after(0).await;
    server.settle().await;
    let second = Duration::from_secs(1);
    let (slots, clean) = server.measure(second).await;
    assert!(
        at_cadence(slots, second),
        "{slots} slots with a healthy worker"
    );
    assert_eq!(clean, 0, "a healthy worker renders every frame");

    // kill -9: clean feed at cadence until a new worker renders again.
    signal("KILL", pid(&ready)).unwrap();
    let (slots, clean) = server.measure(Duration::from_millis(400)).await;
    assert!(
        at_cadence(slots, Duration::from_millis(400)),
        "{slots} slots after kill -9"
    );
    assert!(clean > 0, "frames without a worker are clean feed");
    let restarted = server.ready_after(1).await;
    server.settle().await;
    let (slots, clean) = server.measure(second).await;
    assert!(at_cadence(slots, second), "{slots} slots after the restart");
    assert_eq!(clean, 0, "CG returns after the restart");

    // SIGSTOP: replaced once its oldest request is older than the hang limit.
    let stopped = Instant::now();
    signal("STOP", pid(&restarted)).unwrap();
    let replaced = server.ready_after(2).await;
    assert_ne!(pid(&replaced), pid(&restarted));
    assert!(
        stopped.elapsed() < Duration::from_secs(15),
        "{:?}",
        stopped.elapsed()
    );
    assert!(
        replaced["last_error"].as_str().unwrap().contains("hung"),
        "{replaced}"
    );
    server.settle().await;
    let (slots, clean) = server.measure(second).await;
    assert!(at_cadence(slots, second), "{slots} slots after the hang");
    assert_eq!(clean, 0);
    assert_eq!(server.call("sdi.get_status").await["dropped_frames"], 0);
}

#[tokio::test]
#[ignore = "requires a native hardware GPU"]
async fn gpu_frames_later_than_the_deadline_are_clean_feed() {
    // 60 ms per frame against a 20 ms deadline: every frame misses, none is dropped.
    // The hang limit leaves room for the GPU warm-up of the first frame.
    let mut server = Server::start(&["--worker-hang-ms", "2000"], &[(DELAY_ENV, "60")]).await;
    server.ready_after(0).await;
    let second = Duration::from_secs(1);
    let (slots, clean) = server.measure(second).await;
    assert!(
        at_cadence(slots, second),
        "{slots} slots with a slow worker"
    );
    assert!(clean > 25, "{clean} clean-feed frames");
    let status = server.worker().await;
    assert!(status["deadline_misses"].as_u64().unwrap() > 0, "{status}");
    assert_eq!(status["restarts"], 0, "a slow worker is not a hung one");
}
