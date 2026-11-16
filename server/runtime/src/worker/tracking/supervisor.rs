//! chr-server's side of the tracking worker: starts it, hands it every captured
//! frame as a VchaoFrame ([`protocol`](super::protocol)), takes each frame back
//! with its selected pose, and replaces the worker when it exits, fails or hangs.
//!
//! ```text
//! Starting ──both sockets connected──► Ready ──exit / invalid frame / hang / full──► Backoff ──timer──► Starting
//! ```
//!
//! Every started worker is a new stream: a new `stream_id`, `frame_id`s from 0.
//! While ready, every captured frame is sent, so the stream has no gaps; the
//! frames the worker holds are bounded by [`TrackerConfig::in_flight`], and a
//! worker that would need more is replaced. Nothing here blocks:
//! [`Tracker::track`] queues one frame (its shared memory, not a copy) and
//! [`Tracker::poll`] returns the frames answered since the last call. How long
//! a frame waits for its camera is the caller's join policy.
//!
//! A session's lens (chr's optional `lens` extension, `null` for vchao) is
//! fixed by its first returned frame: a later frame with another lens, an
//! invalid lens or frame warp, or a warp without a lens is a worker failure.

use std::{
    collections::VecDeque,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use super::protocol::{PoseStatus, Received, Wire, check_caps, now_ns};
use crate::calibration::{lens, tracking::matrix3};
use crate::worker::{
    supervisor::{Running, create_private_dir},
    transport::{self, Channel, Event, SharedPixels},
};
use chr_common::{
    methods::{
        render_worker::WorkerState,
        tracking::{TrackerStatus, TrackingFrame},
    },
    scene_frame as w,
};
use chr_renderer::distortion::FrameWarp;

type Error = Box<dyn std::error::Error + Send + Sync>;

const FIRST_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// Time in `Ready` after which the backoff starts again from the beginning.
const STABLE: Duration = Duration::from_secs(10);
/// Loading a tracking model and map can take a while (vchao's `pitch-analysis`
/// allows 120 s); a worker that never listens on its output is replaced.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
/// Frames in flight beyond the join's wait: transport, and replies read once
/// per output period.
const IN_FLIGHT_MARGIN: Duration = Duration::from_millis(250);
/// The fewest frames in flight, the render worker's old fixed credit count.
const MIN_IN_FLIGHT: usize = 8;
/// Placeholder in worker arguments replaced by each session's stream ID, for
/// example a new ledger path per session.
pub const STREAM_ID_PLACEHOLDER: &str = "{stream_id}";

/// The frames a worker may hold at once: every frame the join still waits for
/// (`wait` at `fps`) plus a margin. A worker holding more answers too late for
/// any of them to be rendered, so it is replaced rather than sent fewer frames:
/// a VchaoFrame stream has no gaps.
pub fn in_flight_limit(wait: Duration, fps: u32) -> usize {
    let frames = ((wait + IN_FLIGHT_MARGIN).as_nanos() * u128::from(fps)).div_ceil(1_000_000_000);
    usize::try_from(frames).map_or(usize::MAX, |n| n.max(MIN_IN_FLIGHT))
}

pub struct TrackerConfig {
    /// The worker executable, run as `<executable> <args>... --stream-id <id>
    /// --input-socket <path> --output-socket <path>`.
    pub executable: PathBuf,
    /// Worker arguments; [`STREAM_ID_PLACEHOLDER`] is replaced in each.
    pub args: Vec<String>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The station's optical centre for every camera: world metres, negative Z up.
    pub camera_center: [f64; 3],
    /// Private directory for the worker's two sockets; removed with the tracker.
    pub socket_dir: PathBuf,
    /// The oldest frame in flight may be this old before the worker is killed.
    pub hang: Duration,
    /// Frames the worker may hold at once ([`in_flight_limit`]).
    pub in_flight: usize,
}

/// Shared with the control plane for `tracking.get_worker_status`.
pub type StatusHandle = Arc<Mutex<TrackerStatus>>;

pub fn status_handle() -> StatusHandle {
    Arc::new(Mutex::new(TrackerStatus {
        state: WorkerState::Backoff,
        worker: String::new(),
        stream_id: None,
        pid: None,
        restarts: 0,
        requests: 0,
        cameras: 0,
        unavailable: 0,
        not_sent: 0,
        last_error: None,
    }))
}

/// One frame the worker returned.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    /// The frame's capture sequence.
    pub sequence: u64,
    pub pose: PoseStatus,
    pub overdue: bool,
    /// `None` for an `unavailable` pose.
    pub camera: Option<TrackingFrame>,
}

pub struct Tracker {
    config: TrackerConfig,
    camera_center: w::Vec3,
    status: StatusHandle,
    state: State,
    /// Frames answered since the last [`Tracker::poll`].
    answered: Vec<Answer>,
    /// Workers started so far; numbers the streams.
    sessions: u64,
    backoff: Duration,
}

/// A frame sent and not yet returned.
struct Pending {
    frame_id: u64,
    sequence: u64,
    sent: Instant,
    /// The frame's answer is no longer wanted (the capture restarted).
    forgotten: bool,
}

/// One worker's stream.
struct Session {
    wire: Wire,
    /// Sent frames in `frame_id` order: the order they come back in.
    in_flight: VecDeque<Pending>,
    next_frame_id: u64,
    last_source_ns: u64,
    /// The lens of every frame of this stream, once the first one came back.
    lens: Option<Option<w::RadialLens>>,
}

enum State {
    Starting {
        worker: Running<Received>,
        session: Session,
        since: Instant,
    },
    Ready {
        worker: Running<Received>,
        session: Session,
        since: Instant,
    },
    Backoff {
        until: Instant,
    },
}

impl Tracker {
    /// Creates the socket directory; the first worker starts on the first call.
    pub fn new(config: TrackerConfig, status: StatusHandle) -> Result<Self, Error> {
        let camera_center = config
            .camera_center
            .map(w::Finite::try_from)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "the station centre must be finite")?
            .try_into()
            .expect("three values");
        create_private_dir(&config.socket_dir)?;
        status.lock().unwrap_or_else(PoisonError::into_inner).worker = config
            .executable
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        Ok(Self {
            config,
            camera_center,
            status,
            state: State::Backoff {
                until: Instant::now(),
            },
            answered: Vec::new(),
            sessions: 0,
            backoff: FIRST_BACKOFF,
        })
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.state, State::Ready { .. })
    }

    /// Hands the captured frame `sequence` (RGBA8 at the configured extent) to the
    /// worker: its shared memory, not a copy. Without a ready worker the frame is
    /// not sent, so it goes without a camera; a worker that already holds
    /// [`TrackerConfig::in_flight`] frames is replaced.
    pub fn track(&mut self, sequence: u64, rgba: &SharedPixels) {
        self.maintain();
        let sent = match &mut self.state {
            State::Ready {
                worker, session, ..
            } => send(worker, session, self.config.in_flight, sequence, rgba),
            _ => Err(None),
        };
        match sent {
            Ok(()) => self.status().requests += 1,
            Err(failure) => {
                self.status().not_sent += 1;
                if let Some(reason) = failure {
                    self.reap(reason);
                }
            }
        }
    }

    /// Advances supervision and returns the frames answered since the last call.
    pub fn poll(&mut self) -> Vec<Answer> {
        self.maintain();
        std::mem::take(&mut self.answered)
    }

    /// Forgets every frame in flight, for example after the capture restarted
    /// its sequence numbers: their answers will be ignored. They still come back,
    /// in order, and keep their place in the stream.
    pub fn forget(&mut self) {
        if let State::Starting { session, .. } | State::Ready { session, .. } = &mut self.state {
            for pending in &mut session.in_flight {
                pending.forgotten = true;
            }
        }
        self.answered.clear();
    }

    /// Handles returned frames, then replaces an exited, failed, hung or
    /// never-ready worker and starts one when the backoff ends.
    fn maintain(&mut self) {
        while let Some(event) = self.try_event() {
            if let Err(reason) = self.handle(event) {
                self.reap(reason);
            }
        }
        let now = Instant::now();
        let mut spawn = false;
        let mut ready = false;
        let reap = match &mut self.state {
            State::Backoff { until } => {
                spawn = now >= *until;
                None
            }
            State::Starting { worker, since, .. } => worker.check().or_else(|| {
                ready = worker.channel.is_connected() && worker.channel.worker_connected();
                (!ready && now.duration_since(*since) > STARTUP_TIMEOUT)
                    .then(|| "the tracking worker did not connect both sockets in time".to_owned())
            }),
            State::Ready {
                worker,
                session,
                since,
            } => {
                if now.duration_since(*since) > STABLE {
                    self.backoff = FIRST_BACKOFF;
                }
                worker.check().or_else(|| {
                    if !worker.channel.worker_connected() {
                        return Some("the tracking worker left its input socket".to_owned());
                    }
                    session
                        .in_flight
                        .front()
                        .is_some_and(|pending| now.duration_since(pending.sent) > self.config.hang)
                        .then(|| {
                            format!(
                                "the tracking worker held a frame for more than {:?}",
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
        } else if ready {
            self.become_ready();
        }
    }

    /// chr-server reads the worker's output socket and the worker reads its
    /// input: there is no `ready` message in the VchaoFrame contract.
    fn become_ready(&mut self) {
        let State::Starting {
            worker, session, ..
        } = std::mem::replace(
            &mut self.state,
            State::Backoff {
                until: Instant::now(),
            },
        )
        else {
            unreachable!("only a starting worker becomes ready")
        };
        log::info!(
            "tracking worker {} ready: stream {}",
            worker.child.id(),
            session.wire.stream_id
        );
        self.state = State::Ready {
            worker,
            session,
            since: Instant::now(),
        };
        self.status().state = WorkerState::Ready;
    }

    /// Validates one returned frame against the oldest frame in flight; any
    /// mismatch is a worker failure, not a frame without a camera.
    fn handle(&mut self, event: Event<Received>) -> Result<(), String> {
        let received = match event {
            Event::Message(received) => received,
            Event::Closed(reason) => return Err(reason),
        };
        let (State::Starting { session, .. } | State::Ready { session, .. }) = &mut self.state
        else {
            return Ok(());
        };
        let meta = &received.meta;
        let stream_id = &session.wire.stream_id;
        if &meta.stream_id != stream_id {
            return Err(format!(
                "the tracking worker returned a frame of stream {:?}, not {stream_id:?}",
                meta.stream_id
            ));
        }
        if let Some(caps) = &received.caps {
            check_caps(caps, stream_id).map_err(|error| error.to_string())?;
        }
        let Some(pending) = session.in_flight.pop_front() else {
            return Err(format!(
                "the tracking worker returned frame {} that was never sent",
                meta.frame_id
            ));
        };
        if meta.frame_id != pending.frame_id {
            return Err(format!(
                "the tracking worker returned frame {} where frame {} was due",
                meta.frame_id, pending.frame_id
            ));
        }
        if meta.duration_ns != session.wire.duration_ns()
            || received.buffer.size() != session.wire.frame_bytes()
        {
            return Err(format!(
                "the tracking worker returned frame {} with another duration or size",
                meta.frame_id
            ));
        }
        let Some(selection) = &meta.selection else {
            return Err(format!(
                "the tracking worker returned frame {} without a selection",
                meta.frame_id
            ));
        };
        match &session.lens {
            None => {
                if let Some(lens) = &meta.lens {
                    lens::radial(lens).map_err(|error| {
                        format!("the tracking worker returned an invalid lens: {error}")
                    })?;
                }
                log::info!(
                    "tracking worker stream {stream_id}: {}",
                    meta.lens
                        .as_ref()
                        .map_or_else(|| "no lens".to_owned(), lens::describe)
                );
                session.lens = Some(meta.lens.clone());
            }
            Some(lens) if *lens != meta.lens => {
                return Err(format!(
                    "the tracking worker changed its lens within stream {stream_id} at frame {}",
                    meta.frame_id
                ));
            }
            Some(_) => {}
        }
        let frame_warp = selection
            .pose
            .frame_warp()
            .map(|warp| {
                if meta.lens.is_none() {
                    return Err("a frame warp without a lens".to_owned());
                }
                let rows = std::array::from_fn(|r| std::array::from_fn(|c| warp[r * 3 + c]));
                FrameWarp::new(rows).map_err(|error| error.to_string())?;
                matrix3(rows).map_err(|error| error.to_string())
            })
            .transpose()
            .map_err(|error| {
                format!(
                    "the tracking worker returned frame {} with an invalid frame warp: {error}",
                    meta.frame_id
                )
            })?;
        if pending.forgotten {
            return Ok(());
        }
        let camera = selection.pose.t().map(|t| TrackingFrame {
            sequence: pending.sequence.into(),
            // Finite: checked when the meta was read.
            ti: t.map(|v| w::Finite::try_from(v).expect("finite Ti")),
            camera_center: self.camera_center,
            frame_warp,
            lens: meta.lens.clone(),
        });
        let mut status = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        match camera {
            Some(_) => status.cameras += 1,
            None => status.unavailable += 1,
        }
        drop(status);
        self.answered.push(Answer {
            sequence: pending.sequence,
            pose: selection.pose.status(),
            overdue: selection.overdue,
            camera,
        });
        Ok(())
    }

    fn try_event(&self) -> Option<Event<Received>> {
        match &self.state {
            State::Starting { worker, .. } | State::Ready { worker, .. } => {
                worker.channel.try_event()
            }
            State::Backoff { .. } => None,
        }
    }

    fn spawn(&mut self) {
        if self.sessions > 0 {
            self.status().restarts += 1;
        }
        self.sessions += 1;
        let wire = Wire {
            stream_id: format!("chr-{}-{}", std::process::id(), self.sessions),
            width: self.config.width,
            height: self.config.height,
            fps: self.config.fps,
        };
        match self.start(&wire) {
            Ok(worker) => {
                let mut status = self.status();
                status.state = WorkerState::Starting;
                status.pid = Some(worker.child.id());
                status.stream_id = Some(wire.stream_id.clone());
                drop(status);
                self.state = State::Starting {
                    worker,
                    session: Session {
                        wire,
                        in_flight: VecDeque::new(),
                        next_frame_id: 0,
                        last_source_ns: 0,
                        lens: None,
                    },
                    since: Instant::now(),
                };
            }
            Err(error) => self.backoff(format!("cannot start the tracking worker: {error}")),
        }
    }

    fn start(&self, wire: &Wire) -> Result<Running<Received>, Error> {
        transport::init()?;
        let channel = Channel::open(
            &self.config.socket_dir,
            Some(&wire.caps()),
            self.config.in_flight as u64 + 1,
            true,
        )?;
        let mut command = Command::new(&self.config.executable);
        command
            .args(
                self.config
                    .args
                    .iter()
                    .map(|arg| arg.replace(STREAM_ID_PLACEHOLDER, &wire.stream_id)),
            )
            .arg("--stream-id")
            .arg(&wire.stream_id);
        Running::spawn(command, channel)
    }

    /// Kills the current worker, if any, forgets its frames and starts the backoff.
    fn reap(&mut self, reason: String) {
        if let State::Starting { worker, .. } | State::Ready { worker, .. } = std::mem::replace(
            &mut self.state,
            State::Backoff {
                until: Instant::now(),
            },
        ) {
            // Its frames are released with its sockets.
            worker.kill();
        }
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
        status.stream_id = None;
        status.last_error = Some(reason);
    }

    fn status(&self) -> MutexGuard<'_, TrackerStatus> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Sends one frame on the session's stream. Fails with `None` if it was merely
/// not sent and with the reason if the worker must be replaced.
fn send(
    worker: &Running<Received>,
    session: &mut Session,
    limit: usize,
    sequence: u64,
    rgba: &SharedPixels,
) -> Result<(), Option<String>> {
    if session.in_flight.len() >= limit {
        return Err(Some(format!(
            "the tracking worker holds {} frames, the most chr-server keeps in flight \
             (--tracking-wait-ms plus {IN_FLIGHT_MARGIN:?})",
            session.in_flight.len()
        )));
    }
    let frame_id = session.next_frame_id;
    // Strictly increasing, as the stream requires, even for frames read together.
    let source_ns = now_ns().max(session.last_source_ns + 1);
    session
        .wire
        .frame(frame_id, source_ns, rgba.memory())
        .and_then(|buffer| worker.channel.push(buffer))
        .map_err(|error| Some(format!("cannot send to the tracking worker: {error}")))?;
    session.next_frame_id += 1;
    session.last_source_ns = source_ns;
    session.in_flight.push_back(Pending {
        frame_id,
        sequence,
        sent: Instant::now(),
        forgotten: false,
    });
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_in_flight_limit_covers_the_wait() {
        let ms = Duration::from_millis;
        // 1.5 s + 0.25 s at 50 fps.
        assert_eq!(in_flight_limit(ms(1500), 50), 88);
        // pitch-analysis holds up to ceil(500 ms / frame) + 8 + 2 frames: 25 at 30 fps.
        assert!(in_flight_limit(ms(600), 30) >= 25);
        assert_eq!(in_flight_limit(ms(1000), 30), 38);
        assert_eq!(in_flight_limit(ms(60), 50), 16);
        assert_eq!(in_flight_limit(Duration::ZERO, 1), MIN_IN_FLIGHT);
    }
}
