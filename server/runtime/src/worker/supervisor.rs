//! The server's side: starts `chr-renderer-cli worker`, sends it frames and replaces
//! it when it exits, reports a fatal error, or hangs.
//!
//! ```text
//! Starting ──ready──► Ready ──exit / fatal / hang──► Backoff ──timer──► Starting
//! ```
//!
//! Nothing here blocks longer than the caller's deadline: a request that misses it
//! stays outstanding in the worker and its late completion only frees its slot.

use std::{
    collections::VecDeque,
    io::BufReader,
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use chr_common::{
    methods::render_worker::{WorkerState, WorkerStatus},
    scene_frame as w,
};
use chr_shm::{Layout, Read, Reader, SlotHeader, Writer};

use super::protocol::{self, Output, RING_SLOTS, Reply, Request, VERSION, WorkerProgram};

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
    /// Capacity of one input slot; zero for the triangle, which has no input.
    pub input_bytes: u64,
    /// Private directory for the two rings; removed with the supervisor.
    pub ring_dir: PathBuf,
    /// The oldest outstanding request may be this old before the worker is killed.
    pub hang: Duration,
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
    /// Every ring slot is held by an outstanding request.
    NoSlot,
    /// The worker did not finish before the deadline.
    Deadline,
    /// The worker reported or caused a failure.
    Failed(String),
}

pub struct Supervisor {
    config: WorkerConfig,
    status: StatusHandle,
    input: Option<Writer>,
    input_path: PathBuf,
    output_path: PathBuf,
    state: State,
    /// Request ID holding each slot.
    slots: [Option<u64>; RING_SLOTS as usize],
    /// Outstanding requests in send order, with their send time.
    outstanding: VecDeque<(u64, Instant)>,
    next_id: u64,
    backoff: Duration,
    started: bool,
}

enum State {
    Starting {
        worker: Running,
        since: Instant,
    },
    Ready {
        worker: Running,
        output: Reader,
        since: Instant,
    },
    Backoff {
        until: Instant,
    },
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    replies: mpsc::Receiver<Event>,
}

enum Event {
    Reply(Reply),
    Closed(String),
}

impl Supervisor {
    /// Creates the rings; the first worker starts on the first [`Self::maintain`].
    pub fn new(config: WorkerConfig, status: StatusHandle) -> Result<Self, Error> {
        let mut dir = std::fs::DirBuilder::new();
        dir.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
        dir.create(&config.ring_dir)?;
        let input_path = config.ring_dir.join("input.ring");
        let output_path = config.ring_dir.join("output.ring");
        let input = (config.input_bytes > 0)
            .then(|| {
                Writer::create(
                    &input_path,
                    Layout {
                        slot_count: RING_SLOTS,
                        slot_bytes: config.input_bytes,
                    },
                )
            })
            .transpose()?;
        Ok(Self {
            config,
            status,
            input,
            input_path,
            output_path,
            state: State::Backoff {
                until: Instant::now(),
            },
            slots: [None; RING_SLOTS as usize],
            outstanding: VecDeque::new(),
            next_id: 0,
            backoff: FIRST_BACKOFF,
            started: false,
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
        let reap = match &self.state {
            State::Backoff { until } => {
                spawn = now >= *until;
                None
            }
            State::Starting { since, .. } => (now.duration_since(*since) > STARTUP_TIMEOUT)
                .then(|| "the worker did not become ready in time".to_owned()),
            State::Ready { since, .. } => {
                if now.duration_since(*since) > STABLE {
                    self.backoff = FIRST_BACKOFF;
                }
                self.outstanding
                    .front()
                    .is_some_and(|(_, sent)| now.duration_since(*sent) > self.config.hang)
                    .then(|| format!("the worker hung for more than {:?}", self.config.hang))
            }
        };
        if let Some(reason) = reap {
            self.reap(reason);
        } else if spawn {
            self.spawn();
        }
    }

    /// Renders one program frame; `pixels` is copied into the input ring before the
    /// request is sent, so the caller may release its input afterwards.
    pub fn render(
        &mut self,
        image: &w::ImageDescription,
        pixels: &[u8],
        frame: &w::FrameInput,
        scene: &w::SceneSnapshot,
        deadline: Instant,
    ) -> Result<Vec<u8>, Miss> {
        let (id, slot) = self.reserve()?;
        let input = self
            .input
            .as_mut()
            .ok_or(Miss::Failed("no input ring".into()))?;
        let header = SlotHeader {
            index: id,
            meta: [0; 4],
        };
        if let Err(error) = input.write(slot, header, pixels) {
            self.slots[slot as usize] = None;
            return Err(Miss::Failed(error.to_string()));
        }
        self.send(
            id,
            slot,
            &Request::Render {
                id,
                input_slot: slot,
                output_slot: slot,
                image: image.clone(),
                frame: frame.clone(),
                scene: Box::new(scene.clone()),
            },
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
        let (id, slot) = self.reserve()?;
        self.send(
            id,
            slot,
            &Request::Triangle {
                id,
                output_slot: slot,
                frame,
                angle_rad,
            },
            deadline,
        )
    }

    fn reserve(&mut self) -> Result<(u64, u32), Miss> {
        self.maintain();
        if !self.is_ready() {
            return Err(Miss::NotReady);
        }
        let slot = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(Miss::NoSlot)?;
        let id = self.next_id;
        self.next_id += 1;
        self.slots[slot] = Some(id);
        Ok((id, slot as u32))
    }

    fn send(
        &mut self,
        id: u64,
        slot: u32,
        request: &Request,
        deadline: Instant,
    ) -> Result<Vec<u8>, Miss> {
        let State::Ready { worker, .. } = &mut self.state else {
            unreachable!("reserved while ready");
        };
        if let Err(error) = protocol::write_line(&mut worker.stdin, request) {
            self.reap(format!("cannot send to the worker: {error}"));
            return Err(Miss::Failed(error.to_string()));
        }
        self.outstanding.push_back((id, Instant::now()));
        loop {
            let event = match &self.state {
                State::Ready { worker, .. } => {
                    match worker
                        .replies
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    {
                        Ok(event) => event,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            self.status().deadline_misses += 1;
                            return Err(Miss::Deadline);
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            Event::Closed("the worker closed its output".into())
                        }
                    }
                }
                _ => return Err(Miss::Failed("the worker was replaced".into())),
            };
            if let Some(result) = self.handle(event, Some((id, slot))) {
                return result;
            }
        }
    }

    /// Applies one event. Returns the outcome when it completes `awaited`.
    fn handle(
        &mut self,
        event: Event,
        awaited: Option<(u64, u32)>,
    ) -> Option<Result<Vec<u8>, Miss>> {
        match event {
            Event::Reply(Reply::Ready { adapter }) => {
                let State::Starting { .. } = self.state else {
                    self.reap("unexpected ready".into());
                    return None;
                };
                match Reader::open(&self.output_path) {
                    Ok(output) => {
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
                            output,
                            since: Instant::now(),
                        };
                        self.status().state = WorkerState::Ready;
                    }
                    Err(error) => self.reap(format!("cannot open the worker output ring: {error}")),
                }
                None
            }
            Event::Reply(Reply::Fatal { message }) => {
                self.reap(format!("worker failed: {message}"));
                None
            }
            Event::Closed(reason) => {
                self.reap(reason);
                awaited.map(|_| Err(Miss::Failed("the worker exited".into())))
            }
            Event::Reply(Reply::Done { id }) => {
                let result = (awaited.map(|(awaited, _)| awaited) == Some(id))
                    .then(|| self.read_output(id, awaited.expect("awaited").1));
                self.complete(id);
                result
            }
            Event::Reply(Reply::Failed { id, message }) => {
                log::warn!("render request {id} failed: {message}");
                self.status().last_error = Some(message.clone());
                self.complete(id);
                (awaited.map(|(awaited, _)| awaited) == Some(id))
                    .then_some(Err(Miss::Failed(message)))
            }
        }
    }

    fn read_output(&mut self, id: u64, slot: u32) -> Result<Vec<u8>, Miss> {
        let State::Ready { output, .. } = &self.state else {
            return Err(Miss::Failed("no output ring".into()));
        };
        let mut uyvy = Vec::new();
        match output.read(slot, &mut uyvy) {
            Ok(Read::Frame(header)) if header.index == id => Ok(uyvy),
            other => Err(Miss::Failed(format!(
                "output slot {slot} does not hold request {id}: {other:?}"
            ))),
        }
    }

    fn complete(&mut self, id: u64) {
        self.outstanding
            .retain(|(outstanding, _)| *outstanding != id);
        for slot in &mut self.slots {
            if *slot == Some(id) {
                *slot = None;
            }
        }
    }

    fn try_event(&self) -> Option<Event> {
        match &self.state {
            State::Starting { worker, .. } | State::Ready { worker, .. } => {
                worker.replies.try_recv().ok()
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

    fn start(&mut self) -> Result<Running, Error> {
        let _ = std::fs::remove_file(&self.output_path);
        let mut child = Command::new(&self.config.executable)
            .arg("worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (sender, replies) = mpsc::channel();
        std::thread::Builder::new()
            .name("chr-worker-replies".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let event = match protocol::read_line::<Reply>(&mut reader) {
                        Ok(Some(reply)) => Event::Reply(reply),
                        Ok(None) => Event::Closed("the worker exited".into()),
                        Err(error) => Event::Closed(format!("invalid worker output: {error}")),
                    };
                    let closed = matches!(event, Event::Closed(_));
                    if sender.send(event).is_err() || closed {
                        return;
                    }
                }
            })?;
        let hello = Request::Hello {
            version: VERSION,
            program: self.config.program.clone(),
            runtime_id: self.config.runtime_id.clone(),
            output: self.config.output,
            input_ring: self.input.as_ref().map(|_| self.input_path.clone()),
            output_ring: self.output_path.clone(),
        };
        if let Err(error) = protocol::write_line(&mut stdin, &hello) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
        Ok(Running {
            child,
            stdin,
            replies,
        })
    }

    /// Kills the current worker, if any, and starts the backoff.
    fn reap(&mut self, reason: String) {
        if let State::Starting { worker, .. } | State::Ready { worker, .. } = std::mem::replace(
            &mut self.state,
            State::Backoff {
                until: Instant::now(),
            },
        ) {
            let mut child = worker.child;
            let _ = child.kill();
            let _ = child.wait();
        }
        // The worker is gone, so every slot is free again.
        self.slots = [None; RING_SLOTS as usize];
        self.outstanding.clear();
        self.backoff(reason);
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

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let State::Starting { worker, .. } | State::Ready { worker, .. } = std::mem::replace(
            &mut self.state,
            State::Backoff {
                until: Instant::now(),
            },
        ) {
            // EOF on stdin asks the worker to exit; a stuck one is killed.
            let Running {
                mut child, stdin, ..
            } = worker;
            drop(stdin);
            let deadline = Instant::now() + Duration::from_secs(2);
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.config.ring_dir);
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
            input_bytes: 0,
            ring_dir: dir.join("rings"),
            hang: Duration::from_millis(200),
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
            !dir.join("rings").exists(),
            "rings are removed with the supervisor"
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
