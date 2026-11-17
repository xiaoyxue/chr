//! Per-frame camera of a tracked input, paired with captured frames by capture
//! sequence. Every frame carries its own `Ti`, `C`, lens and frame warp; which
//! lens it is rendered with is [`lens`](crate::lens)'s choice.
//!
//! [`TrackingFeed`] is the bounded backlog that `tracking.frame` or the tracking
//! worker fills; [`TrackingJoin`] holds captured frames until their camera
//! arrives or their wait ends, and paces the timer-driven output.
//! [`DeadlineJoin`] only pairs, until each frame's deadline, for chr-server's
//! GStreamer core (#154), whose selector decides each output slot. A frame is
//! never rendered with another frame's camera: a frame without its camera goes
//! out as clean feed (its own pixels, no CG), and its late camera is then
//! rejected.

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
                status: TrackingStatus::default(),
            })),
            checks,
        }
    }

    /// Accepts the camera of one frame that has not been rendered or skipped yet.
    pub fn push(&self, frame: TrackingFrame) -> Result<(), ErrorObject> {
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
                && let Err(error) = self.push(camera)
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

    pub fn count_overwritten(&self, frames: u64) {
        self.lock().status.frames_overwritten += frames;
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

/// Captured frames the join must hold so that none leaves before its `wait`
/// ends: one per output `period` of the wait, plus the frame being read and
/// one of slack, and never fewer than four.
pub fn capture_backlog(wait: Duration, period: Duration) -> usize {
    let periods = wait.as_nanos().div_ceil(period.as_nanos().max(1));
    usize::try_from(periods).map_or(usize::MAX, |n| n.saturating_add(2).max(4))
}

/// Joined frames held for the next output periods: a capture burst of two frames
/// is rendered one per period instead of being dropped.
pub const READY: usize = 2;

/// Captured frames waiting for their camera, oldest first, and joined frames
/// waiting for an output period.
pub struct TrackingJoin<P> {
    feed: TrackingFeed,
    wait: Duration,
    capacity: usize,
    /// How long after capture a joined frame leaves; zero: as soon as it is joined.
    playout: Duration,
    frames: VecDeque<CapturedFrame<P>>,
    /// Resolved frames, with their camera or `None` for clean feed.
    ready: VecDeque<(CapturedFrame<P>, Option<TrackingFrame>)>,
}

impl<P> TrackingJoin<P> {
    /// `wait` bounds how long a frame waits for its camera; `capacity` bounds the
    /// frames held, so latency cannot grow when capture runs ahead of the output.
    /// Joined frames leave as soon as they are joined.
    pub fn new(feed: TrackingFeed, wait: Duration, capacity: usize) -> Self {
        Self {
            feed,
            wait,
            capacity,
            playout: Duration::ZERO,
            frames: VecDeque::new(),
            ready: VecDeque::new(),
        }
    }

    /// Releases every resolved frame at the output period nearest to `wait`
    /// after its capture instead: a constant program delay for cameras that
    /// arrive in bursts, such as a tracking worker that holds frames until a
    /// later estimate resolves them. A wait of whole periods puts every
    /// deadline on an output tick, so tick jitter would move frames by a whole
    /// period, in pairs and gaps; deciding half a `period` early avoids that.
    pub fn with_fixed_delay(mut self, period: Duration) -> Self {
        self.wait = self.wait.saturating_sub(period / 2);
        self.playout = self.wait;
        self
    }

    /// Forgets waiting frames and the feed's samples after a capture restart.
    pub fn reset(&mut self) {
        self.frames.clear();
        self.ready.clear();
        self.feed.reset();
    }

    /// Adds a captured frame; frames arrive in increasing sequence.
    pub fn frame(&mut self, frame: CapturedFrame<P>) {
        if self.frames.len() == self.capacity
            && let Some(oldest) = self.frames.pop_front()
        {
            match self.feed.close(oldest.sequence) {
                Some(_) => self.feed.count(|status| status.frames_dropped += 1),
                None => self
                    .feed
                    .count(|status| status.frames_without_tracking += 1),
            }
        }
        self.frames.push_back(frame);
    }

    /// The next frame to output, oldest first, with its camera, or `None` for
    /// clean feed. Frames resolve in sequence order: joined with their camera,
    /// or without one once their wait ended, never with a neighbour's camera.
    /// A resolved frame is due once its playout delay has passed. Up to
    /// [`READY`] due frames wait for an output period, absorbing capture bursts;
    /// beyond that the oldest are dropped, so a stall cannot raise the latency
    /// for good.
    pub fn next(&mut self, now: Instant) -> Option<(CapturedFrame<P>, Option<TrackingFrame>)> {
        while let Some(front) = self.frames.front() {
            let waited = now.saturating_duration_since(front.arrived) >= self.wait;
            let sequence = front.sequence;
            let has_camera = self.feed.lock().samples.contains_key(&sequence);
            if !has_camera && !waited {
                break;
            }
            let frame = self.frames.pop_front().expect("front exists");
            let sample = self.feed.close(sequence);
            if sample.is_none() {
                self.feed
                    .count(|status| status.frames_without_tracking += 1);
            }
            self.ready.push_back((frame, sample));
        }
        let due = self
            .ready
            .iter()
            .take_while(|(frame, _)| now.saturating_duration_since(frame.arrived) >= self.playout)
            .count();
        let excess = due
            .saturating_sub(READY)
            .max(self.ready.len().saturating_sub(self.capacity));
        for _ in 0..excess {
            self.ready.pop_front();
            self.feed.count(|status| status.frames_dropped += 1);
        }
        if due <= excess {
            return None;
        }
        let next = self.ready.pop_front();
        match &next {
            Some((_, Some(_))) => self.feed.count(|status| status.frames_joined += 1),
            Some((_, None)) => self.feed.count(|status| status.frames_clean_feed += 1),
            None => {}
        }
        next
    }
}

/// Captured frames waiting for their camera until their own deadline, for
/// chr-server's GStreamer core (#154). Unlike [`TrackingJoin`] it does not pace
/// the output: a frame leaves as soon as its camera is in the feed, to be
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
    fn the_capture_backlog_outlasts_the_wait() {
        let period = Duration::from_millis(40);
        assert_eq!(capture_backlog(period * 3, period), 5);
        assert_eq!(capture_backlog(Duration::from_millis(201), period), 8);
        assert_eq!(capture_backlog(Duration::ZERO, period), 4);
        assert_eq!(capture_backlog(MAX_WAIT, Duration::from_millis(20)), 77);
    }

    #[test]
    fn pairs_by_sequence_regardless_of_arrival_order() {
        let feed = feed(8);
        let mut join = TrackingJoin::new(feed.clone(), Duration::from_millis(100), 4);
        let start = Instant::now();
        join.frame(captured(10, start));
        join.frame(captured(11, start));
        feed.push(replay().frame(11)).unwrap();
        assert!(
            join.next(start).is_none(),
            "frame 10 still waits for its camera"
        );
        feed.push(replay().frame(10)).unwrap();
        for sequence in [10, 11] {
            let (frame, sample) = join.next(start).unwrap();
            assert_eq!(
                (frame.sequence, sample.unwrap().sequence.get()),
                (sequence, sequence)
            );
        }
        assert!(join.next(start).is_none());
        assert_eq!(feed.status().frames_joined, 2);
    }

    #[test]
    fn a_burst_is_spread_over_output_periods_and_a_backlog_is_dropped() {
        let feed = feed(8);
        let mut join = TrackingJoin::new(feed.clone(), Duration::from_millis(100), 8);
        let start = Instant::now();
        for sequence in 0..5 {
            join.frame(captured(sequence, start));
            feed.push(replay().frame(sequence)).unwrap();
        }
        let served: Vec<_> = std::iter::from_fn(|| join.next(start))
            .map(|(frame, _)| frame.sequence)
            .collect();
        assert_eq!(served, [3, 4], "the oldest joined frames give way");
        assert_eq!(feed.status().frames_dropped, 3);
    }

    #[test]
    fn a_fixed_delay_releases_a_burst_of_cameras_one_frame_per_period() {
        let feed = feed(8);
        let wait = Duration::from_millis(100);
        let period = Duration::from_millis(20);
        let mut join = TrackingJoin::new(feed.clone(), wait, 8).with_fixed_delay(period);
        let start = Instant::now();
        for sequence in 0..4 {
            join.frame(captured(sequence, start + period * sequence as u32));
        }
        // A worker returns four frames together once a later estimate resolves them.
        for sequence in 0..4 {
            feed.push(replay().frame(sequence)).unwrap();
        }
        assert!(join.next(start + period * 4).is_none(), "joined, not due");
        let served: Vec<_> = (0..6)
            .filter_map(|tick| join.next(start + wait + period * tick))
            .map(|(frame, _)| frame.sequence)
            .collect();
        assert_eq!(served, [0, 1, 2, 3], "each leaves `wait` after its capture");
        let status = feed.status();
        assert_eq!((status.frames_joined, status.frames_dropped), (4, 0));
    }

    #[test]
    fn a_frame_without_camera_goes_out_as_clean_feed_and_its_late_camera_is_rejected() {
        let feed = feed(8);
        let wait = Duration::from_millis(100);
        let period = Duration::from_millis(20);
        let mut join = TrackingJoin::new(feed.clone(), wait, 4).with_fixed_delay(period);
        let start = Instant::now();
        join.frame(captured(1, start));
        join.frame(captured(2, start + period));
        feed.push(replay().frame(2)).unwrap();
        assert!(
            join.next(start + wait - period).is_none(),
            "frame 2 never overtakes the unresolved frame 1"
        );
        let (frame, camera) = join.next(start + wait).unwrap();
        assert_eq!(
            (frame.sequence, camera),
            (1, None),
            "clean feed at its own time"
        );
        let (frame, camera) = join.next(start + wait + period).unwrap();
        assert_eq!((frame.sequence, camera.is_some()), (2, true));
        let late = feed.push(replay().frame(1)).unwrap_err();
        assert_eq!(late.code, w::RejectionKind::InvalidState.code());
        let status = feed.status();
        assert_eq!(
            (
                status.frames_without_tracking,
                status.frames_clean_feed,
                status.frames_joined,
                status.samples_late
            ),
            (1, 1, 1, 1)
        );
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
        let mut join = TrackingJoin::new(feed.clone(), Duration::ZERO, 1);
        let start = Instant::now();
        join.frame(captured(4, start));
        join.frame(captured(5, start));
        assert_eq!(
            feed.status().frames_without_tracking,
            1,
            "an overflowing frame without camera is not counted as dropped"
        );
        let (frame, camera) = join.next(start).unwrap();
        assert_eq!((frame.sequence, camera), (5, None), "frame 5 has no camera");
        // Dropping frame 4 discarded the sample of the never-captured frame 3.
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
