//! chr-server's side of the tracking worker: starts it, hands it captured frames
//! and collects its cameras, and replaces it when it exits, fails or hangs.
//!
//! ```text
//! Starting ──ready──► Ready ──exit / fatal / hang──► Backoff ──timer──► Starting
//! ```
//!
//! Nothing here blocks: [`Tracker::track`] writes a frame and one request line,
//! and [`Tracker::poll`] returns the cameras answered since the last call. How
//! long a frame waits for its camera is the caller's join policy. Each request
//! has its own ID and is answered at most once, so answers from a replaced
//! worker or for forgotten frames are never used.

use std::{
    io::BufReader,
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc},
    time::{Duration, Instant},
};

use chr_common::methods::{
    render_worker::WorkerState,
    tracking::{TrackerStatus, TrackingFrame},
};
use chr_shm::{Layout, SlotHeader, Writer};

use crate::protocol::{self, RING_SLOTS, Reply, Request, VERSION};

type Error = Box<dyn std::error::Error + Send + Sync>;

const FIRST_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// Time in `Ready` after which the backoff starts again from the beginning.
const STABLE: Duration = Duration::from_secs(10);
/// Loading a tracking reference can take a while; a worker that never becomes
/// ready is replaced.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

pub struct TrackerConfig {
    /// The worker executable; it is run as `<executable> worker <args>...`.
    pub executable: PathBuf,
    pub args: Vec<String>,
    /// Passed to the worker in `hello`.
    pub calibration: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Private directory for the frame ring; removed with the tracker.
    pub ring_dir: PathBuf,
    /// The oldest unanswered request may be this old before the worker is killed.
    pub hang: Duration,
}

/// Shared with the control plane for `tracking.get_worker_status`.
pub type StatusHandle = Arc<Mutex<TrackerStatus>>;

pub fn status_handle() -> StatusHandle {
    Arc::new(Mutex::new(TrackerStatus {
        state: WorkerState::Backoff,
        pid: None,
        restarts: 0,
        requests: 0,
        cameras: 0,
        unavailable: 0,
        not_sent: 0,
        last_error: None,
    }))
}

pub struct Tracker {
    config: TrackerConfig,
    status: StatusHandle,
    ring: Writer,
    ring_path: PathBuf,
    state: State,
    /// The unanswered request each slot's frame belongs to.
    slots: [Option<Pending>; RING_SLOTS as usize],
    /// Cameras answered since the last [`Tracker::poll`].
    answered: Vec<TrackingFrame>,
    next_id: u64,
    backoff: Duration,
    started: bool,
}

#[derive(Clone, Copy)]
struct Pending {
    id: u64,
    sequence: u64,
    sent: Instant,
    /// The frame's answer is no longer wanted; the slot stays held until it comes.
    forgotten: bool,
}

enum State {
    Starting { worker: Running, since: Instant },
    Ready { worker: Running, since: Instant },
    Backoff { until: Instant },
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

impl Tracker {
    /// Creates the frame ring; the first worker starts on the first call.
    pub fn new(config: TrackerConfig, status: StatusHandle) -> Result<Self, Error> {
        let mut dir = std::fs::DirBuilder::new();
        dir.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
        dir.create(&config.ring_dir)?;
        let ring_path = config.ring_dir.join("frames.ring");
        let ring = Writer::create(
            &ring_path,
            Layout {
                slot_count: RING_SLOTS,
                slot_bytes: u64::from(config.width) * u64::from(config.height) * 4,
            },
        )?;
        Ok(Self {
            config,
            status,
            ring,
            ring_path,
            state: State::Backoff {
                until: Instant::now(),
            },
            slots: [None; RING_SLOTS as usize],
            answered: Vec::new(),
            next_id: 0,
            backoff: FIRST_BACKOFF,
            started: false,
        })
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.state, State::Ready { .. })
    }

    /// Hands the captured frame `sequence` (RGBA8 at the configured extent) to the
    /// worker. Without a ready worker or a free slot the frame is not sent, so it
    /// will go without a camera.
    pub fn track(&mut self, sequence: u64, rgba: &[u8]) {
        self.maintain();
        let slot = match &self.state {
            State::Ready { .. } => self.slots.iter().position(Option::is_none),
            _ => None,
        };
        let Some(slot) = slot else {
            self.status().not_sent += 1;
            return;
        };
        let id = self.next_id;
        self.next_id += 1;
        let header = SlotHeader {
            index: id,
            meta: [sequence, 0, 0, 0],
        };
        if let Err(error) = self.ring.write(slot as u32, header, rgba) {
            self.status().not_sent += 1;
            log::warn!("tracking frame {sequence} not sent: {error}");
            return;
        }
        let State::Ready { worker, .. } = &mut self.state else {
            unreachable!("a slot is only chosen while ready")
        };
        let request = Request::Track {
            id,
            sequence,
            slot: slot as u32,
        };
        if let Err(error) = protocol::write_line(&mut worker.stdin, &request) {
            self.status().not_sent += 1;
            self.reap(format!("cannot send to the tracking worker: {error}"));
            return;
        }
        self.slots[slot] = Some(Pending {
            id,
            sequence,
            sent: Instant::now(),
            forgotten: false,
        });
        self.status().requests += 1;
    }

    /// Advances supervision and returns the cameras answered since the last call.
    pub fn poll(&mut self) -> Vec<TrackingFrame> {
        self.maintain();
        std::mem::take(&mut self.answered)
    }

    /// Forgets every unanswered frame, for example after the capture restarted
    /// its sequence numbers: their answers will be ignored. Their slots are
    /// reused only once answered, as the worker may still be reading them.
    pub fn forget(&mut self) {
        for pending in self.slots.iter_mut().flatten() {
            pending.forgotten = true;
        }
        self.answered.clear();
    }

    /// Handles replies, then replaces an exited, failed, hung or never-ready
    /// worker and starts one when the backoff ends.
    fn maintain(&mut self) {
        while let Some(event) = self.try_event() {
            self.handle(event);
        }
        let now = Instant::now();
        let mut spawn = false;
        let reap = match &self.state {
            State::Backoff { until } => {
                spawn = now >= *until;
                None
            }
            State::Starting { since, .. } => (now.duration_since(*since) > STARTUP_TIMEOUT)
                .then(|| "the tracking worker did not become ready in time".to_owned()),
            State::Ready { since, .. } => {
                if now.duration_since(*since) > STABLE {
                    self.backoff = FIRST_BACKOFF;
                }
                self.slots
                    .iter()
                    .flatten()
                    .any(|pending| now.duration_since(pending.sent) > self.config.hang)
                    .then(|| {
                        format!(
                            "the tracking worker left a frame unanswered for more than {:?}",
                            self.config.hang
                        )
                    })
            }
        };
        if let Some(reason) = reap {
            self.reap(reason);
        } else if spawn {
            self.spawn();
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Reply(Reply::Ready { name }) => {
                let State::Starting { .. } = self.state else {
                    self.reap("unexpected ready from the tracking worker".into());
                    return;
                };
                let State::Starting { worker, .. } = std::mem::replace(
                    &mut self.state,
                    State::Backoff {
                        until: Instant::now(),
                    },
                ) else {
                    unreachable!()
                };
                log::info!("tracking worker {} ready: {name}", worker.child.id());
                self.state = State::Ready {
                    worker,
                    since: Instant::now(),
                };
                self.status().state = WorkerState::Ready;
            }
            Event::Reply(Reply::Fatal { message }) => {
                self.reap(format!("tracking worker failed: {message}"));
            }
            Event::Closed(reason) => self.reap(reason),
            Event::Reply(Reply::Camera { id, camera }) => {
                let Some(pending) = self.answer(id) else {
                    return;
                };
                if pending.forgotten {
                    return;
                }
                if camera.sequence.get() != pending.sequence {
                    self.reap(format!(
                        "the tracking worker answered request {id} for frame {} with frame {}",
                        pending.sequence,
                        camera.sequence.get()
                    ));
                    return;
                }
                self.status().cameras += 1;
                self.answered.push(camera);
            }
            Event::Reply(Reply::Unavailable { id, reason }) => {
                if let Some(pending) = self.answer(id).filter(|pending| !pending.forgotten) {
                    log::debug!("frame {} has no camera: {reason}", pending.sequence);
                    self.status().unavailable += 1;
                }
            }
        }
    }

    /// Frees the slot of request `id`; `None` if it is not awaited (any more).
    fn answer(&mut self, id: u64) -> Option<Pending> {
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.is_some_and(|p| p.id == id))?;
        slot.take()
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
            Err(error) => self.backoff(format!("cannot start the tracking worker: {error}")),
        }
    }

    fn start(&mut self) -> Result<Running, Error> {
        let mut child = Command::new(&self.config.executable)
            .arg("worker")
            .args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (sender, replies) = mpsc::channel();
        std::thread::Builder::new()
            .name("chr-tracking-replies".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let event = match protocol::read_line::<Reply>(&mut reader) {
                        Ok(Some(reply)) => Event::Reply(reply),
                        Ok(None) => Event::Closed("the tracking worker exited".into()),
                        Err(error) => {
                            Event::Closed(format!("invalid tracking worker output: {error}"))
                        }
                    };
                    let closed = matches!(event, Event::Closed(_));
                    if sender.send(event).is_err() || closed {
                        return;
                    }
                }
            })?;
        let hello = Request::Hello {
            version: VERSION,
            input_ring: self.ring_path.clone(),
            width: self.config.width,
            height: self.config.height,
            fps: self.config.fps,
            calibration: self.config.calibration.clone(),
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

    /// Kills the current worker, if any, forgets its frames and starts the backoff.
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
        self.slots = [None; RING_SLOTS as usize];
        self.backoff(reason);
    }

    fn backoff(&mut self, reason: String) {
        log::warn!(
            "tracking worker: {reason}; restarting in {:?}",
            self.backoff
        );
        self.state = State::Backoff {
            until: Instant::now() + self.backoff,
        };
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
        let mut status = self.status();
        status.state = WorkerState::Backoff;
        status.pid = None;
        status.last_error = Some(reason);
    }

    fn status(&self) -> MutexGuard<'_, TrackerStatus> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Tracker {
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
