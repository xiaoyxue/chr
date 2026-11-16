//! Per-frame camera of a tracked input, paired with captured frames by capture
//! sequence. A simulation of the #60 tracking path, not a production tracking
//! service: the lens is fixed at startup and only `Ti`, `C` and the frame warp
//! travel per frame.
//!
//! [`TrackingFeed`] is the bounded backlog that `tracking.frame` or the tracking
//! worker fills; [`TrackingJoin`] holds captured frames until their camera
//! arrives or their wait ends. A frame is never rendered with another frame's
//! camera: a missing camera means the frame is skipped, and its late camera is
//! then rejected.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use chr_common::{
    jsonrpc::ErrorObject,
    methods::tracking::{TrackingFrame, TrackingStatus},
    scene_frame as w,
};
use chr_renderer::{
    coordinates::Ti,
    distortion::{FrameWarp, RadialDistortion},
};

use crate::{
    retained::{incompatible, reject},
    worker::{tracking::protocol::PoseStatus, transport::SharedPixels},
};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Rows of a forward row-major wire matrix.
pub fn rows3(matrix: &w::Matrix3) -> [[f64; 3]; 3] {
    std::array::from_fn(|row| std::array::from_fn(|col| matrix[row * 3 + col].get()))
}

/// The wire form of `rows`, rejecting nonfinite values.
pub fn matrix3(rows: [[f64; 3]; 3]) -> Result<w::Matrix3, Error> {
    let flat: Vec<w::Finite> = rows
        .into_iter()
        .flatten()
        .map(w::Finite::try_from)
        .collect::<Result<_, _>>()
        .map_err(|e| e.0)?;
    Ok(flat.try_into().expect("nine values"))
}

/// The startup half of a tracked program: its lens, world and native extent.
#[derive(Clone, Debug)]
pub struct Tracked {
    pub lens: RadialDistortion,
    pub world: w::Version,
    pub extent: w::Extent,
    /// A fixed reference camera and warps, replayed by `chr-renderer-cli render
    /// --frame`; a render description may omit it.
    pub replay: Option<TrackingReplay>,
}

impl Tracked {
    /// The frame geometry `render.submit` receives for one tracking sample.
    pub fn geometry(&self, frame: &TrackingFrame) -> w::FrameGeometry {
        w::FrameGeometry::TiC {
            world: self.world.clone(),
            context_id: None,
            extent: self.extent,
            ti: frame.ti,
            camera_center: frame.camera_center,
            frame_warp: frame.frame_warp.map(Box::new),
        }
    }
}

/// Tracking samples replayed from a calibrated clip: the reference `Ti` and `C` for
/// every frame and the clip's warp of `sequence % warps`, the convention of the
/// calibrated video replay. No warps means `W = S`.
#[derive(Clone, Debug)]
pub struct TrackingReplay {
    ti: w::Matrix3,
    camera_center: w::Vec3,
    frame_warps: Vec<w::Matrix3>,
}

impl TrackingReplay {
    pub fn new(
        ti: [[f64; 3]; 3],
        camera_center: [f64; 3],
        frame_warps: &[[[f64; 3]; 3]],
    ) -> Result<Self, Error> {
        Ti::new(ti)?;
        for rows in frame_warps {
            FrameWarp::new(*rows)?;
        }
        Ok(Self {
            ti: matrix3(ti)?,
            camera_center: camera_center
                .map(w::Finite::try_from)
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.0)?
                .try_into()
                .expect("three values"),
            frame_warps: frame_warps
                .iter()
                .map(|rows| matrix3(*rows))
                .collect::<Result<_, _>>()?,
        })
    }

    pub fn warp_count(&self) -> usize {
        self.frame_warps.len()
    }

    /// The reference camera's optical centre: world metres, negative Z up.
    pub fn camera_center(&self) -> [f64; 3] {
        self.camera_center.map(w::Finite::get)
    }

    /// The reference `Ti`, forward row-major ground XY to ideal image SNC.
    pub fn ti(&self) -> [f64; 9] {
        self.ti.map(w::Finite::get)
    }

    pub fn frame(&self, sequence: u64) -> TrackingFrame {
        TrackingFrame {
            sequence: sequence.into(),
            ti: self.ti,
            camera_center: self.camera_center,
            frame_warp: (!self.frame_warps.is_empty())
                .then(|| self.frame_warps[(sequence % self.frame_warps.len() as u64) as usize]),
        }
    }
}

/// Bounded backlog of tracking samples not yet paired, shared by the control plane
/// and the injection loop.
#[derive(Clone)]
pub struct TrackingFeed(Arc<Mutex<Feed>>);

struct Feed {
    capacity: usize,
    samples: BTreeMap<u64, TrackingFrame>,
    /// Highest capture sequence already rendered or skipped.
    done_through: Option<u64>,
    status: TrackingStatus,
}

impl TrackingFeed {
    pub fn new(capacity: usize) -> Self {
        Self(Arc::new(Mutex::new(Feed {
            capacity,
            samples: BTreeMap::new(),
            done_through: None,
            status: TrackingStatus::default(),
        })))
    }

    /// Accepts the camera of one frame that has not been rendered or skipped yet.
    pub fn push(&self, frame: TrackingFrame) -> Result<(), ErrorObject> {
        Ti::new(rows3(&frame.ti)).map_err(incompatible)?;
        if let Some(rows) = &frame.frame_warp {
            FrameWarp::new(rows3(rows)).map_err(incompatible)?;
        }
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

    pub fn status(&self) -> TrackingStatus {
        self.lock().status.clone()
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
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One captured input frame: RGBA8 BT.709 code values at the native extent, in
/// the memory shared with the workers.
pub struct CapturedFrame {
    pub sequence: u64,
    pub rgba: SharedPixels,
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
pub struct TrackingJoin {
    feed: TrackingFeed,
    wait: Duration,
    capacity: usize,
    /// How long after capture a joined frame leaves; zero: as soon as it is joined.
    playout: Duration,
    frames: VecDeque<CapturedFrame>,
    ready: VecDeque<(CapturedFrame, TrackingFrame)>,
}

impl TrackingJoin {
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

    /// Releases every joined frame exactly `wait` after its capture instead: a
    /// constant program delay for cameras that arrive in bursts, such as a
    /// tracking worker that holds frames until a later estimate resolves them.
    pub fn with_fixed_delay(mut self) -> Self {
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
    pub fn frame(&mut self, frame: CapturedFrame) {
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

    /// The next frame to render, oldest first. Frames resolve in sequence order:
    /// joined with their camera, or skipped once their wait ended, never
    /// rendered with a neighbour's camera. A joined frame is due once its
    /// playout delay has passed. Up to [`READY`] due frames wait for an output
    /// period, absorbing capture bursts; beyond that the oldest are dropped, so
    /// a stall cannot raise the latency for good.
    pub fn next(&mut self, now: Instant) -> Option<(CapturedFrame, TrackingFrame)> {
        while let Some(front) = self.frames.front() {
            let waited = now.saturating_duration_since(front.arrived) >= self.wait;
            let sequence = front.sequence;
            let has_camera = self.feed.lock().samples.contains_key(&sequence);
            if !has_camera && !waited {
                break;
            }
            let frame = self.frames.pop_front().expect("front exists");
            match self.feed.close(sequence) {
                Some(sample) => self.ready.push_back((frame, sample)),
                None => self
                    .feed
                    .count(|status| status.frames_without_tracking += 1),
            }
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
        if next.is_some() {
            self.feed.count(|status| status.frames_joined += 1);
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: f64 = 0.001;

    fn replay() -> TrackingReplay {
        TrackingReplay::new(
            [[0.02, 0., 0.], [0., 0.02, 0.], [0., 0., 1.]],
            [0., 0., -10.],
            &[
                [[S, 0., -1.], [0., S, -0.5], [0., 0., 1.]],
                [[S, 0., -0.9], [0., S, -0.5], [0., 0., 1.]],
            ],
        )
        .unwrap()
    }

    fn captured(sequence: u64, arrived: Instant) -> CapturedFrame {
        CapturedFrame {
            sequence,
            rgba: SharedPixels::copy_from(&[sequence as u8; 4]).unwrap(),
            arrived,
        }
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
    fn replay_follows_the_clip_loop_and_rejects_singular_input() {
        let replay = replay();
        assert_eq!(replay.frame(2).frame_warp, replay.frame(0).frame_warp);
        assert_ne!(replay.frame(1).frame_warp, replay.frame(0).frame_warp);
        assert_eq!(replay.frame(5).sequence.get(), 5);
        let unwarped = TrackingReplay::new(
            [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            [0., 0., -1.],
            &[],
        )
        .unwrap();
        assert_eq!(unwarped.frame(3).frame_warp, None);
        assert!(TrackingReplay::new([[0.; 3]; 3], [0., 0., -1.], &[]).is_err());
        assert!(
            TrackingReplay::new(
                [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
                [0., 0., -1.],
                &[[[0.; 3]; 3]]
            )
            .is_err()
        );
    }

    #[test]
    fn pairs_by_sequence_regardless_of_arrival_order() {
        let feed = TrackingFeed::new(8);
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
                (frame.sequence, sample.sequence.get()),
                (sequence, sequence)
            );
        }
        assert!(join.next(start).is_none());
        assert_eq!(feed.status().frames_joined, 2);
    }

    #[test]
    fn a_burst_is_spread_over_output_periods_and_a_backlog_is_dropped() {
        let feed = TrackingFeed::new(8);
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
        let feed = TrackingFeed::new(8);
        let wait = Duration::from_millis(100);
        let period = Duration::from_millis(20);
        let mut join = TrackingJoin::new(feed.clone(), wait, 8).with_fixed_delay();
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
    fn a_frame_without_camera_is_skipped_and_its_late_camera_rejected() {
        let feed = TrackingFeed::new(8);
        let mut join = TrackingJoin::new(feed.clone(), Duration::from_millis(100), 4);
        let start = Instant::now();
        join.frame(captured(1, start));
        join.frame(captured(2, start));
        feed.push(replay().frame(2)).unwrap();
        assert!(
            join.next(start + Duration::from_millis(99)).is_none(),
            "frame 2 never overtakes the unresolved frame 1"
        );
        let (frame, _) = join.next(start + Duration::from_millis(100)).unwrap();
        assert_eq!(
            frame.sequence, 2,
            "frame 1 timed out, frame 2 is not delayed by it"
        );
        let late = feed.push(replay().frame(1)).unwrap_err();
        assert_eq!(late.code, w::RejectionKind::InvalidState.code());
        let status = feed.status();
        assert_eq!(
            (
                status.frames_without_tracking,
                status.frames_joined,
                status.samples_late
            ),
            (1, 1, 1)
        );
    }

    #[test]
    fn bounds_the_backlog_and_drops_superseded_samples() {
        let feed = TrackingFeed::new(2);
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
        assert!(join.next(start).is_none(), "frame 5 has no camera");
        // Dropping frame 4 discarded the sample of the never-captured frame 3.
        feed.push(replay().frame(10)).unwrap();
        let mut bad = replay().frame(11);
        bad.frame_warp = Some(matrix3([[0.; 3]; 3]).unwrap());
        assert!(feed.push(bad).is_err());
    }
}
