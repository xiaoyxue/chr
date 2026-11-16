//! The server's side: starts `chr-renderer-cli worker`, sends it frames and replaces
//! it when it exits, reports a fatal error, or hangs.
//!
//! ```text
//! Starting ──ready──► Ready ──exit / fatal / hang──► Backoff ──timer──► Starting
//! ```
//!
//! Nothing here blocks longer than the caller's deadline: a request that misses it
//! stays outstanding in the worker and its late completion only returns its credit.
//! At most [`CREDITS`] requests are outstanding, so a slow or stopped worker holds
//! a bounded number of frames.
//!
//! Assets registered at runtime ([`WorkerAssets`]) reach a worker in its `hello`;
//! when they change, the current worker is replaced at once, without backoff.

use std::{
    collections::VecDeque,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chr_common::{
    description::Asset,
    methods::render_worker::{WorkerState, WorkerStatus},
    scene_frame as w,
};

use super::{
    protocol::{CREDITS, Output, Reply, Request, VERSION, WorkerProgram},
    transport::{self, Channel, Event, Inbound, Incoming, SharedPixels},
};
use crate::frame_resources::Pixels;

type Error = Box<dyn std::error::Error + Send + Sync>;

const FIRST_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// Time in `Ready` after which the backoff starts again from the beginning.
const STABLE: Duration = Duration::from_secs(10);
/// Installation can load assets; a worker that never becomes ready is replaced.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

pub struct WorkerConfig {
    /// The `chr-renderer-cli` executable.
    pub executable: PathBuf,
    pub program: WorkerProgram,
    pub runtime_id: w::Id,
    pub output: Output,
    /// Private directory for the worker's two sockets; removed with the supervisor.
    pub socket_dir: PathBuf,
    /// The oldest outstanding request may be this old before the worker is killed.
    pub hang: Duration,
    /// Assets every worker registers after installing its program.
    pub assets: WorkerAssets,
}

/// Assets registered after startup, shared by the control plane and the supervisor.
#[derive(Clone, Default)]
pub struct WorkerAssets(Arc<Mutex<Vec<Asset>>>);

impl WorkerAssets {
    pub fn push(&self, asset: Asset) {
        self.lock().push(asset);
    }

    /// The assets registered so far, in registration order.
    pub fn snapshot(&self) -> Vec<Asset> {
        self.lock().clone()
    }

    fn len(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Asset>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Shared with the control plane for `render.get_worker_status`.
pub type StatusHandle = Arc<Mutex<WorkerStatus>>;

pub fn status_handle() -> StatusHandle {
    Arc::new(Mutex::new(WorkerStatus {
        state: WorkerState::Backoff,
        pid: None,
        restarts: 0,
        deadline_misses: 0,
        clean_feed_frames: 0,
        repeated_for_unavailable_clean_feed: 0,
        last_error: None,
    }))
}

/// Why a frame got no CG.
#[derive(Debug, PartialEq)]
pub enum Miss {
    /// No ready worker.
    NotReady,
    /// Every credit is held by an outstanding request.
    NoCredit,
    /// The worker did not finish before the deadline.
    Deadline,
    /// The worker reported or caused a failure.
    Failed(String),
}

pub struct Supervisor {
    config: WorkerConfig,
    status: StatusHandle,
    state: State,
    /// Outstanding requests in send order, with their send time; each holds a credit.
    outstanding: VecDeque<(u64, Instant)>,
    next_id: u64,
    backoff: Duration,
    started: bool,
    /// Runtime assets the current worker received.
    assets_sent: usize,
}

enum State {
    Starting {
        worker: Running<Incoming<Reply>>,
        since: Instant,
    },
    Ready {
        worker: Running<Incoming<Reply>>,
        since: Instant,
    },
    Backoff {
        until: Instant,
    },
}

impl Supervisor {
    /// Creates the socket directory; the first worker starts on the first
    /// [`Self::maintain`].
    pub fn new(config: WorkerConfig, status: StatusHandle) -> Result<Self, Error> {
        create_private_dir(&config.socket_dir)?;
        Ok(Self {
            config,
            status,
            state: State::Backoff {
                until: Instant::now(),
            },
            outstanding: VecDeque::new(),
            next_id: 0,
            backoff: FIRST_BACKOFF,
            started: false,
            assets_sent: 0,
        })
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.state, State::Ready { .. })
    }

    /// Advances supervision without blocking: handles replies, replaces exited,
    /// hung or stuck workers, and starts one when the backoff ends.
    pub fn maintain(&mut self) {
        while let Some(event) = self.try_event() {
            self.handle(event, None);
        }
        let now = Instant::now();
        let mut spawn = false;
        let reap = match &mut self.state {
            State::Backoff { until } => {
                spawn = now >= *until;
                None
            }
            State::Starting { worker, since } => worker.check().or_else(|| {
                (now.duration_since(*since) > STARTUP_TIMEOUT)
                    .then(|| "the worker did not become ready in time".to_owned())
            }),
            State::Ready { worker, since } => {
                if now.duration_since(*since) > STABLE {
                    self.backoff = FIRST_BACKOFF;
                }
                worker.check().or_else(|| {
                    self.outstanding
                        .front()
                        .is_some_and(|(_, sent)| now.duration_since(*sent) > self.config.hang)
                        .then(|| format!("the worker hung for more than {:?}", self.config.hang))
                })
            }
        };
        if let Some(reason) = reap {
            self.reap(reason);
        } else if spawn {
            self.spawn();
        } else if matches!(self.state, State::Starting { .. } | State::Ready { .. })
            && self.config.assets.len() != self.assets_sent
        {
            self.replace();
        }
    }

    /// Replaces a running worker so that its successor registers new assets.
    fn replace(&mut self) {
        log::info!("render worker: new scene assets; replacing the worker");
        self.kill();
        self.backoff = FIRST_BACKOFF;
        self.spawn();
    }

    /// Renders one program frame. Shared `pixels` go to the worker as they are;
    /// owned ones are first copied into shared memory.
    pub fn render(
        &mut self,
        image: &w::ImageDescription,
        pixels: &Pixels,
        frame: &w::FrameInput,
        scene: &w::SceneSnapshot,
        deadline: Instant,
    ) -> Result<Vec<u8>, Miss> {
        let id = self.reserve()?;
        let memory = match pixels {
            Pixels::Shared(shared) => shared.memory(),
            Pixels::Owned(bytes) => SharedPixels::copy_from(bytes)
                .map_err(|error| Miss::Failed(error.to_string()))?
                .memory(),
        };
        self.send(
            id,
            &Request::Render {
                id,
                image: image.clone(),
                frame: frame.clone(),
                scene: Box::new(scene.clone()),
            },
            Some(memory),
            deadline,
        )
    }

    /// Renders one frame of the default triangle.
    pub fn triangle(
        &mut self,
        frame: u64,
        angle_rad: f64,
        deadline: Instant,
    ) -> Result<Vec<u8>, Miss> {
        let id = self.reserve()?;
        self.send(
            id,
            &Request::Triangle {
                id,
                frame,
                angle_rad,
            },
            None,
            deadline,
        )
    }

    fn reserve(&mut self) -> Result<u64, Miss> {
        self.maintain();
        if !self.is_ready() {
            return Err(Miss::NotReady);
        }
        if self.outstanding.len() >= CREDITS {
            return Err(Miss::NoCredit);
        }
        let id = self.next_id;
        self.next_id += 1;
        Ok(id)
    }

    fn send(
        &mut self,
        id: u64,
        request: &Request,
        payload: Option<gstreamer::Memory>,
        deadline: Instant,
    ) -> Result<Vec<u8>, Miss> {
        let State::Ready { worker, .. } = &mut self.state else {
            unreachable!("reserved while ready");
        };
        if let Err(error) = worker.channel.send(request, payload) {
            self.reap(format!("cannot send to the worker: {error}"));
            return Err(Miss::Failed(error.to_string()));
        }
        self.outstanding.push_back((id, Instant::now()));
        loop {
            let event = match &self.state {
                State::Ready { worker, .. } => match worker
                    .channel
                    .event_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Some(event) => event,
                    None => {
                        self.status().deadline_misses += 1;
                        return Err(Miss::Deadline);
                    }
                },
                _ => return Err(Miss::Failed("the worker was replaced".into())),
            };
            if let Some(result) = self.handle(event, Some(id)) {
                return result;
            }
        }
    }

    /// Applies one event. Returns the outcome when it completes `awaited`.
    fn handle(
        &mut self,
        event: Event<Incoming<Reply>>,
        awaited: Option<u64>,
    ) -> Option<Result<Vec<u8>, Miss>> {
        let incoming = match event {
            Event::Message(incoming) => incoming,
            Event::Closed(reason) => {
                self.reap(reason);
                return awaited.map(|_| Err(Miss::Failed("the worker exited".into())));
            }
        };
        match &incoming.message {
            Reply::Ready { adapter } => {
                let State::Starting { .. } = self.state else {
                    self.reap("unexpected ready".into());
                    return None;
                };
                let State::Starting { worker, .. } = std::mem::replace(
                    &mut self.state,
                    State::Backoff {
                        until: Instant::now(),
                    },
                ) else {
                    unreachable!()
                };
                log::info!("render worker {} ready on {adapter}", worker.child.id());
                self.state = State::Ready {
                    worker,
                    since: Instant::now(),
                };
                self.status().state = WorkerState::Ready;
                None
            }
            Reply::Fatal { message } => {
                self.reap(format!("worker failed: {message}"));
                None
            }
            Reply::Done { id } => {
                let result = (awaited == Some(*id)).then(|| self.output(&incoming));
                self.complete(*id);
                result
            }
            Reply::Failed { id, message } => {
                log::warn!("render request {id} failed: {message}");
                self.status().last_error = Some(message.clone());
                self.complete(*id);
                (awaited == Some(*id)).then(|| Err(Miss::Failed(message.clone())))
            }
        }
    }

    /// The UYVY frame of a `done` reply, copied out so its buffer goes back to
    /// the worker.
    fn output(&self, done: &Incoming<Reply>) -> Result<Vec<u8>, Miss> {
        let Output { width, height, .. } = self.config.output;
        let expected = width as usize * height as usize * 2;
        match done.payload() {
            Ok(uyvy) if uyvy.len() == expected => Ok(uyvy.to_vec()),
            Ok(uyvy) => Err(Miss::Failed(format!(
                "the worker returned {} bytes of UYVY, not {expected}",
                uyvy.len()
            ))),
            Err(error) => Err(Miss::Failed(error.to_string())),
        }
    }

    fn complete(&mut self, id: u64) {
        self.outstanding
            .retain(|(outstanding, _)| *outstanding != id);
    }

    fn try_event(&self) -> Option<Event<Incoming<Reply>>> {
        match &self.state {
            State::Starting { worker, .. } | State::Ready { worker, .. } => {
                worker.channel.try_event()
            }
            State::Backoff { .. } => None,
        }
    }

    fn spawn(&mut self) {
        if self.started {
            self.status().restarts += 1;
        }
        self.started = true;
        match self.start() {
            Ok(worker) => {
                let mut status = self.status();
                status.state = WorkerState::Starting;
                status.pid = Some(worker.child.id());
                drop(status);
                self.state = State::Starting {
                    worker,
                    since: Instant::now(),
                };
            }
            Err(error) => self.backoff(format!("cannot start the worker: {error}")),
        }
    }

    fn start(&mut self) -> Result<Running<Incoming<Reply>>, Error> {
        let assets = self.config.assets.snapshot();
        self.assets_sent = assets.len();
        transport::init()?;
        let channel = Channel::open(
            &self.config.socket_dir,
            Some(&gstreamer::Caps::new_empty_simple(transport::CAPS)),
            transport::QUEUE,
            false,
        )?;
        // Waits in the socket until the worker connects.
        channel.send(
            &Request::Hello {
                version: VERSION,
                program: self.config.program.clone(),
                runtime_id: self.config.runtime_id.clone(),
                output: self.config.output,
                assets,
            },
            None,
        )?;
        let mut command = Command::new(&self.config.executable);
        command.arg("worker");
        Running::spawn(command, channel)
    }

    /// Kills the current worker, if any, and starts the backoff.
    fn reap(&mut self, reason: String) {
        self.kill();
        self.backoff(reason);
    }

    fn kill(&mut self) {
        if let State::Starting { worker, .. } | State::Ready { worker, .. } = std::mem::replace(
            &mut self.state,
            State::Backoff {
                until: Instant::now(),
            },
        ) {
            worker.kill();
        }
        // The worker is gone with its buffers, so every credit is free again.
        self.outstanding.clear();
    }

    fn backoff(&mut self, reason: String) {
        log::warn!("render worker: {reason}; restarting in {:?}", self.backoff);
        self.state = State::Backoff {
            until: Instant::now() + self.backoff,
        };
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
        let mut status = self.status();
        status.state = WorkerState::Backoff;
        status.pid = None;
        status.last_error = Some(reason);
    }

    fn status(&self) -> std::sync::MutexGuard<'_, WorkerStatus> {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A started worker process and its sockets.
pub(crate) struct Running<T> {
    pub(crate) child: Child,
    pub(crate) channel: Channel<T>,
}

impl<M: Inbound> Running<M> {
    /// Runs `command --input-socket <path> --output-socket <path>` on `channel`'s
    /// sockets. Its stdout joins its stderr, the worker's log.
    pub(crate) fn spawn(mut command: Command, channel: Channel<M>) -> Result<Self, Error> {
        let child = command
            .args(channel.worker_args())
            .stdin(Stdio::null())
            .stdout(std::io::stderr())
            .stderr(Stdio::inherit())
            .spawn()?;
        Ok(Self { child, channel })
    }

    /// Connects to the worker's output once it listens. Returns why the worker
    /// must be replaced: it exited before connecting (once connected, the end of
    /// its output reports the exit after its last messages).
    pub(crate) fn check(&mut self) -> Option<String> {
        if let Err(error) = self.channel.connect() {
            return Some(format!("cannot read the worker's output: {error}"));
        }
        match self.child.try_wait() {
            Ok(Some(status)) if !self.channel.is_connected() => {
                Some(format!("the worker exited ({status})"))
            }
            _ => None,
        }
    }
}

impl<T> Running<T> {
    /// Kills the process; its sockets close and release every buffer it held.
    pub(crate) fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Ends the worker's input, which asks it to exit; a stuck one is killed.
    pub(crate) fn stop(mut self) {
        self.channel.finish();
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.kill();
    }
}

/// Creates `dir` readable by this user only.
pub(crate) fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let State::Starting { worker, .. } | State::Ready { worker, .. } = std::mem::replace(
            &mut self.state,
            State::Backoff {
                until: Instant::now(),
            },
        ) {
            worker.stop();
        }
        let _ = std::fs::remove_dir_all(&self.config.socket_dir);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn config(dir: &std::path::Path, executable: PathBuf) -> WorkerConfig {
        WorkerConfig {
            executable,
            program: WorkerProgram::Triangle,
            runtime_id: w::Id::try_from("test-runtime".to_owned()).unwrap(),
            output: Output {
                width: 16,
                height: 16,
                fps: 50,
            },
            socket_dir: dir.join("sockets"),
            hang: Duration::from_millis(200),
            assets: Default::default(),
        }
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_millis(20)
    }

    #[test]
    fn a_missing_executable_backs_off_and_frames_are_not_ready() {
        let dir =
            std::env::temp_dir().join(format!("chr-supervisor-missing-{}", std::process::id()));
        let status = status_handle();
        let mut supervisor = Supervisor::new(
            config(&dir, dir.join("no-such-worker")),
            Arc::clone(&status),
        )
        .unwrap();
        assert_eq!(supervisor.triangle(0, 0., deadline()), Err(Miss::NotReady));
        let status = status.lock().unwrap().clone();
        assert_eq!(status.state, WorkerState::Backoff);
        assert!(status.last_error.unwrap().contains("cannot start"));
        drop(supervisor);
        assert!(
            !dir.join("sockets").exists(),
            "sockets are removed with the supervisor"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exiting_workers_are_restarted_with_growing_backoff() {
        let dir = std::env::temp_dir().join(format!("chr-supervisor-exit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("worker.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 3\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let status = status_handle();
        let mut supervisor = Supervisor::new(config(&dir, script), Arc::clone(&status)).unwrap();
        let until = Instant::now() + Duration::from_millis(1200);
        while Instant::now() < until {
            assert!(supervisor.triangle(0, 0., deadline()).is_err());
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = status.lock().unwrap().clone();
        // 250 + 500 ms of backoff fit into 1.2 s; 1 s more would not.
        assert!((2..=3).contains(&status.restarts), "{status:?}");
        assert_eq!(status.state, WorkerState::Backoff);
        assert!(status.last_error.unwrap().contains("exited"));
        drop(supervisor);
        let _ = std::fs::remove_dir_all(dir);
    }
}
