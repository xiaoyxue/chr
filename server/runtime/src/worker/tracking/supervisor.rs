//! chr-server's side of the tracking worker: starts it, hands it captured frames
//! and collects its cameras, and replaces it when it exits, fails or hangs.
//!
//! ```text
//! Starting ──ready──► Ready ──exit / fatal / hang──► Backoff ──timer──► Starting
//! ```
//!
//! Nothing here blocks: [`Tracker::track`] queues one request that carries the
//! frame's shared memory, and [`Tracker::poll`] returns the cameras answered since
//! the last call. At most [`CREDITS`] frames await an answer. How
//! long a frame waits for its camera is the caller's join policy. Each request
//! has its own ID and is answered at most once, so answers from a replaced
//! worker or for forgotten frames are never used.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use super::protocol::{CREDITS, Reply, Request, VERSION};
use crate::worker::{
    supervisor::{Running, create_private_dir},
    transport::{Event, SharedPixels},
};
use chr_common::methods::{
    render_worker::WorkerState,
    tracking::{TrackerStatus, TrackingFrame},
};

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
    /// Private directory for the worker's two sockets; removed with the tracker.
    pub socket_dir: PathBuf,
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
    state: State,
    /// The unanswered requests, one per credit.
    credits: [Option<Pending>; CREDITS],
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
    /// The frame's answer is no longer wanted; the credit stays held until it comes.
    forgotten: bool,
}

enum State {
    Starting {
        worker: Running<Reply>,
        since: Instant,
    },
    Ready {
        worker: Running<Reply>,
        since: Instant,
    },
    Backoff {
        until: Instant,
    },
}

impl Tracker {
    /// Creates the socket directory; the first worker starts on the first call.
    pub fn new(config: TrackerConfig, status: StatusHandle) -> Result<Self, Error> {
        create_private_dir(&config.socket_dir)?;
        Ok(Self {
            config,
            status,
            state: State::Backoff {
                until: Instant::now(),
            },
            credits: [None; CREDITS],
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
    /// worker: its shared memory, not a copy. Without a ready worker or a free
    /// credit the frame is not sent, so it will go without a camera.
    pub fn track(&mut self, sequence: u64, rgba: &SharedPixels) {
        self.maintain();
        let credit = match &self.state {
            State::Ready { .. } => self.credits.iter().position(Option::is_none),
            _ => None,
        };
        let Some(credit) = credit else {
            self.status().not_sent += 1;
            return;
        };
        let State::Ready { worker, .. } = &mut self.state else {
            unreachable!("a credit is only taken while ready")
        };
        let id = self.next_id;
        self.next_id += 1;
        if let Err(error) = worker
            .channel
            .send(&Request::Track { id, sequence }, Some(rgba.memory()))
        {
            self.status().not_sent += 1;
            self.reap(format!("cannot send to the tracking worker: {error}"));
            return;
        }
        self.credits[credit] = Some(Pending {
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
    /// its sequence numbers: their answers will be ignored. Their credits return
    /// only once answered, as the worker may still be reading them.
    pub fn forget(&mut self) {
        for pending in self.credits.iter_mut().flatten() {
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
        let reap = match &mut self.state {
            State::Backoff { until } => {
                spawn = now >= *until;
                None
            }
            State::Starting { worker, since } => worker.check().or_else(|| {
                (now.duration_since(*since) > STARTUP_TIMEOUT)
                    .then(|| "the tracking worker did not become ready in time".to_owned())
            }),
            State::Ready { worker, since } => {
                if now.duration_since(*since) > STABLE {
                    self.backoff = FIRST_BACKOFF;
                }
                worker.check().or_else(|| {
                    self.credits
                        .iter()
                        .flatten()
                        .any(|pending| now.duration_since(pending.sent) > self.config.hang)
                        .then(|| {
                            format!(
                                "the tracking worker left a frame unanswered for more than {:?}",
                                self.config.hang
                            )
                        })
                })
            }
        };
        if let Some(reason) = reap {
            self.reap(reason);
        } else if spawn {
            self.spawn();
        }
    }

    fn handle(&mut self, event: Event<Reply>) {
        let reply = match event {
            // The frame memory, if any, is released here.
            Event::Message(incoming) => incoming.message,
            Event::Closed(reason) => {
                self.reap(reason);
                return;
            }
        };
        match reply {
            Reply::Ready { name } => {
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
            Reply::Fatal { message } => {
                self.reap(format!("tracking worker failed: {message}"));
            }
            Reply::Camera { id, camera } => {
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
            Reply::Unavailable { id, reason } => {
                if let Some(pending) = self.answer(id).filter(|pending| !pending.forgotten) {
                    log::debug!("frame {} has no camera: {reason}", pending.sequence);
                    self.status().unavailable += 1;
                }
            }
        }
    }

    /// Returns the credit of request `id`; `None` if it is not awaited (any more).
    fn answer(&mut self, id: u64) -> Option<Pending> {
        let credit = self
            .credits
            .iter_mut()
            .find(|credit| credit.is_some_and(|p| p.id == id))?;
        credit.take()
    }

    fn try_event(&self) -> Option<Event<Reply>> {
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
            Err(error) => self.backoff(format!("cannot start the tracking worker: {error}")),
        }
    }

    fn start(&self) -> Result<Running<Reply>, Error> {
        Running::spawn(
            &self.config.executable,
            &self.config.args,
            &self.config.socket_dir,
            &Request::Hello {
                version: VERSION,
                width: self.config.width,
                height: self.config.height,
                fps: self.config.fps,
                calibration: self.config.calibration.clone(),
            },
        )
    }

    /// Kills the current worker, if any, forgets its frames and starts the backoff.
    fn reap(&mut self, reason: String) {
        if let State::Starting { worker, .. } | State::Ready { worker, .. } = std::mem::replace(
            &mut self.state,
            State::Backoff {
                until: Instant::now(),
            },
        ) {
            worker.kill();
        }
        // The worker is gone with its buffers, so every credit is free again.
        self.credits = [None; CREDITS];
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
            worker.stop();
        }
        let _ = std::fs::remove_dir_all(&self.config.socket_dir);
    }
}
