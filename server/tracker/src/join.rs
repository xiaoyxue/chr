//! Per-frame camera of a tracked input, paired with captured frames by capture
//! sequence. Every frame carries its own `Ti`, `C`, lens and frame warp; which
//! lens it is rendered with is [`lens`](crate::lens)'s choice.
//!
//! [`TrackingFeed`] is the bounded backlog that `tracking.frame` or the tracking
//! worker fills; [`DeadlineJoin`] pairs captured frames with their camera
//! until each frame's deadline, for chr-server's GStreamer core (#154), whose
//! selector decides each output slot. A frame is never rendered with another
//! frame's camera: a frame without its camera goes out as clean feed (its own
//! pixels, no CG), and its late camera is then rejected.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use chr_common::{
    jsonrpc::ErrorObject,
    methods::tracking::{ActiveCameraModel, TrackingFrame, TrackingStatus},
    scene_frame as w,
};

use crate::{
    Checks, Error,
    pose::{Answer, rows3},
    protocol::PoseStatus,
};

fn reject(kind: w::RejectionKind, message: impl ToString) -> ErrorObject {
    w::Rejection {
        kind,
        message: message.to_string(),
        frame: None,
    }
    .into()
}

/// Bounded backlog of tracking samples not yet paired, shared by the control plane
/// and the injection loop.
#[derive(Clone)]
pub struct TrackingFeed {
    feed: Arc<Mutex<Feed>>,
    checks: Checks,
}

struct Feed {
    capacity: usize,
    samples: BTreeMap<u64, TrackingFrame>,
    /// Highest capture sequence already rendered or skipped.
    done_through: Option<u64>,
    /// What [`TrackingFeed::push`] adds to a `tracking.frame` sequence.
    base: u64,
    status: TrackingStatus,
}

impl TrackingFeed {
    /// A backlog of at most `capacity` samples, each validated by `checks`.
    pub fn new(capacity: usize, checks: Checks) -> Self {
        Self {
            feed: Arc::new(Mutex::new(Feed {
                capacity,
                samples: BTreeMap::new(),
                done_through: None,
                base: 0,
                status: TrackingStatus::default(),
            })),
            checks,
        }
    }

    /// Accepts the camera of one frame that has not been rendered or skipped
    /// yet, from `tracking.frame`: its `sequence` counts from the
    /// [`Self::rebase`] base, the program sequence of the input stream's first
    /// frame, so a camera is keyed by the frame's `frame_id` in its stream.
    pub fn push(&self, mut frame: TrackingFrame) -> Result<(), ErrorObject> {
        let base = self.lock().base;
        frame.sequence = frame.sequence.get().saturating_add(base).into();
        self.insert(frame)
    }

    /// The program sequence of the input stream's first frame: `tracking.frame`
    /// sequences count from it from now on.
    pub fn rebase(&self, base: u64) {
        self.lock().base = base;
    }

    /// Accepts the camera of program frame `frame.sequence`.
    fn insert(&self, frame: TrackingFrame) -> Result<(), ErrorObject> {
        validate(&frame, &self.checks)
            .map_err(|error| reject(w::RejectionKind::IncompatibleInput, error))?;
        let sequence = frame.sequence.get();
        let mut feed = self.lock();
        if feed.done_through.is_some_and(|done| sequence <= done) {
            feed.status.samples_late += 1;
            return Err(reject(
                w::RejectionKind::InvalidState,
                "frame was already rendered or skipped",
            ));
        }
        if feed.samples.contains_key(&sequence) {
            return Err(reject(
                w::RejectionKind::Conflict,
                "duplicate tracking frame",
            ));
        }
        if feed.samples.len() == feed.capacity {
            return Err(reject(
                w::RejectionKind::Capacity,
                "tracking backlog is full",
            ));
        }
        feed.samples.insert(sequence, frame);
        feed.status.samples_received += 1;
        Ok(())
    }

    /// Counts the tracking worker's poses and queues its cameras for the join,
    /// which counts late ones.
    pub fn accept(&self, answers: Vec<Answer>) {
        for answer in answers {
            self.count_pose(answer.pose, answer.overdue);
            if let Some(camera) = answer.camera
                && let Err(error) = self.insert(camera)
            {
                log::debug!(
                    "camera of frame {} not used: {}",
                    answer.sequence,
                    error.message
                );
            }
        }
    }

    pub fn status(&self) -> TrackingStatus {
        self.lock().status.clone()
    }

    /// Records the camera model of a frame about to be rendered with its camera.
    pub fn record_model(&self, model: ActiveCameraModel) {
        self.count(|status| status.camera_model = Some(model));
    }

    /// Forgets every pending sample and closed frame: a restarted capture process
    /// numbers its frames from the start again. Counters are kept.
    pub fn reset(&self) {
        let mut feed = self.lock();
        feed.samples.clear();
        feed.done_through = None;
    }

    /// Counts the pose a tracking worker returned for one frame.
    pub fn count_pose(&self, pose: PoseStatus, overdue: bool) {
        self.count(|status| {
            *match pose {
                PoseStatus::Fresh => &mut status.poses_fresh,
                PoseStatus::Interpolated => &mut status.poses_interpolated,
                PoseStatus::Reused => &mut status.poses_reused,
                PoseStatus::Unavailable => &mut status.poses_unavailable,
            } += 1;
            status.poses_overdue += u64::from(overdue);
        });
    }

    /// Closes `sequence` and every earlier frame; returns its camera if present.
    fn close(&self, sequence: u64) -> Option<TrackingFrame> {
        let mut feed = self.lock();
        feed.done_through = Some(feed.done_through.map_or(sequence, |d| d.max(sequence)));
        let later = feed.samples.split_off(&(sequence + 1));
        let sample = feed.samples.remove(&sequence);
        feed.samples = later;
        sample
    }

    fn count(&self, update: impl FnOnce(&mut TrackingStatus)) {
        update(&mut self.lock().status);
    }

    fn lock(&self) -> MutexGuard<'_, Feed> {
        self.feed.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Checks one frame's camera at the boundary: an invertible `Ti`, a valid lens
/// and an invertible frame warp, which needs the frame's own lens.
pub fn validate(frame: &TrackingFrame, checks: &Checks) -> Result<(), Error> {
    (checks.ti)(rows3(&frame.ti))?;
    if let Some(lens) = &frame.lens {
        (checks.lens)(lens)?;
    }
    if let Some(rows) = &frame.frame_warp {
        if frame.lens.is_none() {
            return Err("a frame warp needs the frame's lens".into());
        }
        (checks.frame_warp)(rows3(rows))?;
    }
    Ok(())
}

/// One captured input frame. In chr-server, `rgba` is RGBA8 BT.709 code values
/// at the native extent, in the memory shared with the workers.
pub struct CapturedFrame<P> {
    pub sequence: u64,
    pub rgba: P,
    pub arrived: Instant,
}

/// Longest wait for a camera that `chr-server` accepts: vchao's 500 ms overload
/// rule plus transport, with room to spare. Each waiting frame holds its RGBA8
/// pixels (8 MiB at 1080p, 32 MiB at 4K), so the wait bounds the capture
/// backlog's memory: 1.5 s at 1080p50 is about 0.6 GiB.
pub const MAX_WAIT: Duration = Duration::from_millis(1500);

/// Captured frames waiting for their camera until their own deadline, for
/// chr-server's GStreamer core (#154). It does not pace the output: a frame leaves as soon as its camera is in the feed, to be
/// rendered, or without one, and the core's selector puts the rendered frame or
/// the clean feed into the frame's output slot. Frames leave in capture order.
/// A frame leaves without a camera at its deadline, or as soon as it is known
/// that none will come: its answer had none (an `unavailable` pose, or a camera
/// the feed refused), or it was never sent to the tracking worker
/// ([`Self::without_camera`]). So only a frame whose answer is still due holds
/// back later frames, and a tracking worker answers in order: their cameras
/// cannot be in yet.
pub struct DeadlineJoin<P> {
    feed: TrackingFeed,
    capacity: usize,
    frames: VecDeque<Waiting<P>>,
}

struct Waiting<P> {
    frame: CapturedFrame<P>,
    deadline: Instant,
    /// Its camera will not come.
    settled: bool,
}

impl<P> DeadlineJoin<P> {
    /// Holds at most `capacity` frames; beyond that the oldest is given up.
    pub fn new(feed: TrackingFeed, capacity: usize) -> Self {
        Self {
            feed,
            capacity,
            frames: VecDeque::new(),
        }
    }

    /// Forgets waiting frames and the feed's samples after a capture restart.
    pub fn reset(&mut self) {
        self.frames.clear();
        self.feed.reset();
    }

    /// Adds a captured frame whose camera is useful until `deadline`; frames
    /// arrive in increasing sequence.
    pub fn frame(&mut self, frame: CapturedFrame<P>, deadline: Instant) {
        if self.frames.len() == self.capacity
            && let Some(oldest) = self.frames.pop_front()
        {
            self.give_up(oldest.frame.sequence);
        }
        self.frames.push_back(Waiting {
            frame,
            deadline,
            settled: false,
        });
    }

    /// Frame `sequence` gets no camera, for example because it was not sent to
    /// the tracking worker: it leaves with the next [`Self::joined`] call.
    pub fn without_camera(&mut self, sequence: u64) {
        if let Some(waiting) = self
            .frames
            .iter_mut()
            .find(|waiting| waiting.frame.sequence == sequence)
        {
            waiting.settled = true;
        }
    }

    /// Takes the tracking worker's answers ([`TrackingFeed::accept`]); a frame
    /// whose answer brought no camera into the feed gets none.
    pub fn accept(&mut self, answers: Vec<Answer>) {
        let answered: Vec<u64> = answers.iter().map(|answer| answer.sequence).collect();
        self.feed.accept(answers);
        let feed = self.feed.lock();
        for waiting in &mut self.frames {
            let sequence = waiting.frame.sequence;
            if answered.contains(&sequence) && !feed.samples.contains_key(&sequence) {
                waiting.settled = true;
            }
        }
    }

    /// The frames whose camera has arrived, oldest first, each with its camera.
    /// Frames before them that will get no camera, or whose deadline has
    /// passed without one, are given up: they are clean feed.
    pub fn joined(&mut self, now: Instant) -> Vec<(CapturedFrame<P>, TrackingFrame)> {
        let mut joined = Vec::new();
        while let Some(front) = self.frames.front() {
            let sequence = front.frame.sequence;
            let has_camera = self.feed.lock().samples.contains_key(&sequence);
            if !has_camera && !front.settled && now < front.deadline {
                break;
            }
            let waiting = self.frames.pop_front().expect("front exists");
            if !has_camera {
                self.give_up(sequence);
                continue;
            }
            let camera = self
                .feed
                .close(sequence)
                .expect("the camera is in the feed");
            self.feed.count(|status| status.frames_joined += 1);
            joined.push((waiting.frame, camera));
        }
        joined
    }

    fn give_up(&self, sequence: u64) {
        let had_camera = self.feed.close(sequence).is_some();
        self.feed.count(|status| {
            status.frames_clean_feed += 1;
            if !had_camera {
                status.frames_without_tracking += 1;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{pose::matrix3, replay::tests::replay, tests::CHECKS};

    #[test]
    fn a_deadline_join_releases_each_frame_with_its_own_camera_or_gives_it_up() {
        let feed = feed(8);
        let mut join = DeadlineJoin::new(feed.clone(), 3);
        let start = Instant::now();
        let deadline =
            |sequence: u32| start + Duration::from_millis(100 + 20 * u64::from(sequence));
        for sequence in 0..3 {
            join.frame(captured(u64::from(sequence), start), deadline(sequence));
        }
        feed.push(replay().frame(1)).unwrap();
        assert!(join.joined(start).is_empty(), "frame 0 still waits");
        feed.push(replay().frame(0)).unwrap();
        let joined: Vec<_> = join
            .joined(start)
            .into_iter()
            .map(|(frame, camera)| (frame.sequence, camera.sequence.get()))
            .collect();
        assert_eq!(joined, [(0, 0), (1, 1)], "as soon as the cameras arrive");
        assert!(
            join.joined(deadline(2) - Duration::from_millis(1))
                .is_empty()
        );
        assert!(join.joined(deadline(2)).is_empty(), "frame 2 is given up");
        assert!(
            feed.push(replay().frame(2)).is_err(),
            "its camera is too late"
        );
        // A full join gives up its oldest frame.
        for sequence in 3..7 {
            join.frame(captured(u64::from(sequence), start), deadline(sequence));
        }
        feed.push(replay().frame(4)).unwrap();
        let joined = join.joined(deadline(3));
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].0.sequence, 4, "3 was given up when 6 arrived");
        let status = feed.status();
        assert_eq!(
            (
                status.frames_joined,
                status.frames_clean_feed,
                status.frames_without_tracking
            ),
            (3, 2, 2)
        );
    }

    fn answer(sequence: u64, camera: bool) -> Answer {
        Answer {
            sequence,
            pose: if camera {
                PoseStatus::Fresh
            } else {
                PoseStatus::Unavailable
            },
            overdue: false,
            camera: camera.then(|| replay().frame(sequence)),
        }
    }

    #[test]
    fn a_frame_answered_without_a_camera_holds_back_no_later_frame() {
        let feed = feed(8);
        let mut join = DeadlineJoin::new(feed.clone(), 8);
        let start = Instant::now();
        let deadline = start + Duration::from_millis(100);
        for sequence in 0..4 {
            join.frame(captured(sequence, start), deadline);
        }
        // One poll: 0 has a camera, 1 is unavailable, 2 has a camera; 3 is due.
        join.accept(vec![answer(0, true), answer(1, false), answer(2, true)]);
        let joined: Vec<_> = join
            .joined(start)
            .into_iter()
            .map(|(frame, camera)| (frame.sequence, camera.sequence.get()))
            .collect();
        assert_eq!(
            joined,
            [(0, 0), (2, 2)],
            "1 leaves at once, and giving it up drops no earlier camera"
        );
        assert!(join.joined(start).is_empty(), "3 waits for its answer");
        let status = feed.status();
        assert_eq!(
            (status.frames_clean_feed, status.frames_without_tracking),
            (1, 1)
        );
    }

    #[test]
    fn a_frame_never_sent_to_the_tracker_holds_back_no_later_frame() {
        let feed = feed(8);
        let mut join = DeadlineJoin::new(feed.clone(), 8);
        let start = Instant::now();
        let deadline = start + Duration::from_millis(100);
        join.frame(captured(5, start), deadline);
        join.without_camera(5);
        join.frame(captured(6, start), deadline);
        join.accept(vec![answer(6, true)]);
        let joined: Vec<_> = join
            .joined(start)
            .into_iter()
            .map(|(frame, _)| frame.sequence)
            .collect();
        assert_eq!(joined, [6], "long before 5's deadline");
        assert_eq!(feed.status().frames_clean_feed, 1);
    }

    #[test]
    fn external_cameras_count_from_the_input_streams_first_frame() {
        let feed = feed(8);
        feed.rebase(100);
        feed.push(replay().frame(2)).unwrap();
        let mut join = DeadlineJoin::new(feed.clone(), 4);
        let start = Instant::now();
        join.frame(captured(102, start), start + Duration::from_secs(1));
        let joined = join.joined(start);
        assert_eq!(
            (joined[0].0.sequence, joined[0].1.sequence.get()),
            (102, 102),
            "frame_id 2 of the stream whose first frame is 100"
        );
        let late = feed.push(replay().frame(1)).unwrap_err();
        assert_eq!(late.code, w::RejectionKind::InvalidState.code());
        assert_eq!(feed.status().samples_late, 1);
    }

    fn captured(sequence: u64, arrived: Instant) -> CapturedFrame<u64> {
        CapturedFrame {
            sequence,
            rgba: sequence,
            arrived,
        }
    }

    fn feed(capacity: usize) -> TrackingFeed {
        TrackingFeed::new(capacity, CHECKS)
    }

    #[test]
    fn bounds_the_backlog_and_drops_superseded_samples() {
        let feed = feed(2);
        feed.push(replay().frame(3)).unwrap();
        assert_eq!(
            feed.push(replay().frame(3)).unwrap_err().code,
            w::RejectionKind::Conflict.code()
        );
        feed.push(replay().frame(9)).unwrap();
        assert_eq!(
            feed.push(replay().frame(10)).unwrap_err().code,
            w::RejectionKind::Capacity.code()
        );
        let mut join = DeadlineJoin::new(feed.clone(), 1);
        let start = Instant::now();
        join.frame(captured(4, start), start);
        join.frame(captured(5, start), start);
        assert_eq!(
            feed.status().frames_without_tracking,
            1,
            "frame 4 overflowed without a camera"
        );
        assert!(join.joined(start).is_empty(), "frame 5 has no camera");
        // Giving up frame 4 discarded the sample of the never-captured frame 3.
        feed.push(replay().frame(10)).unwrap();
        let mut bad = replay().frame(11);
        bad.frame_warp = Some(matrix3([[0.; 3]; 3]).unwrap());
        assert_eq!(
            feed.push(bad).unwrap_err().code,
            w::RejectionKind::IncompatibleInput.code()
        );
        let mut warp_without_lens = replay().frame(11);
        warp_without_lens.lens = None;
        assert!(feed.push(warp_without_lens).is_err());
        let mut bad_lens = replay().frame(11);
        bad_lens.lens.as_mut().unwrap().scale_px = w::Finite::try_from(0.).unwrap();
        assert!(feed.push(bad_lens).is_err());
        let mut pinhole = replay().frame(11);
        (pinhole.lens, pinhole.frame_warp) = (None, None);
        validate(&pinhole, &CHECKS).unwrap();
    }
}
