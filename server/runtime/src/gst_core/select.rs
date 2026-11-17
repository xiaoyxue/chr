//! The per-slot selector: every captured frame owns one output slot, due at its
//! capture time plus the program delay. At the slot's deadline, a
//! [`DECISION_MARGIN`](super::DECISION_MARGIN) before it is due, the frame
//! goes to the program `appsrc`: its rendered CG if that arrived, else its own
//! clean feed. A CG frame that arrives after its slot was decided is dropped,
//! never shown in another frame's slot.
//!
//! The decision waits on the pipeline clock, so it follows the clock that
//! times the output. Neither input waits for the other, and nothing here
//! blocks the capture: the branches hand frames over and return.
//!
//! Every slot carries its frame's rate, from the capture's caps. With the
//! [`OutputRate::Follow`] policy a decided frame is one program frame at its
//! due time, and the program caps (so `livesync`'s rate) change with the first
//! frame of a new rate. With [`OutputRate::Fixed`] the program runs on its own
//! grid of slots at the fixed rate, and each decided frame fills the grid
//! slots from its due time until the next frame is due: the frame shown is the
//! latest one due (a sample and hold, as `videorate` does, but decided with the
//! frame instead of when the next one arrives, which would cost an input period
//! of latency). Faster input frames that cover no grid slot are dropped, slower
//! ones repeat.
//!
//! A change of the input's format starts a transition: from the oldest slot
//! still undecided to the first CG slot of the new format, every slot sent as
//! clean feed is counted as transition clean feed.

use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::JoinHandle,
    time::Duration,
};

use chr_common::rate::{MAX_FPS, Rate};
use gstreamer as gst;
use gstreamer_app::AppSrc;

use super::{DECISION_MARGIN, Error, OutputRate, PROGRAM_META, Timeline, uyvy_caps};
use crate::sdi::{SdiRecorder, VideoMode};

/// Program frames the `appsrc` may hold; the selector hands over one per slot.
const PROGRAM_QUEUE: u64 = 4;
/// How long an idle selector sleeps before it checks for a stop.
const IDLE: Duration = Duration::from_millis(100);
/// Slots held beyond those of the program delay, for frames of one branch
/// whose other branch is behind.
const SLACK_SLOTS: usize = 8;

/// When a captured frame's slot is due and decided, in running time: the
/// same at any rate.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub delay: gst::ClockTime,
    pub margin: gst::ClockTime,
}

impl Timing {
    pub fn new(delay: Duration) -> Self {
        let ns = |duration: Duration| gst::ClockTime::from_nseconds(duration.as_nanos() as u64);
        Self {
            delay: ns(delay),
            margin: ns(DECISION_MARGIN),
        }
    }

    /// When the slot of a frame captured at `capture` leaves the output.
    pub fn due(&self, capture: gst::ClockTime) -> gst::ClockTime {
        capture + self.delay
    }

    /// When that slot is decided.
    pub fn deadline(&self, capture: gst::ClockTime) -> gst::ClockTime {
        self.due(capture).saturating_sub(self.margin)
    }

    /// Slots in flight at once: a program delay's worth at the highest rate
    /// plus slack, whatever the input's rate is now.
    fn capacity(&self) -> usize {
        let slots = Rate::fps(MAX_FPS).frames_in(Duration::from_nanos(self.delay.nseconds()));
        usize::try_from(slots).map_or(usize::MAX, |n| n.saturating_add(SLACK_SLOTS))
    }
}

/// What a slot got, counted since the core started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Slots with their rendered frame.
    pub cg: u64,
    /// Slots with their clean feed: no CG in time.
    pub clean: u64,
    /// Slots with neither frame at their deadline; the output repeats.
    pub empty: u64,
    /// Rendered frames that arrived after their slot was decided.
    pub late_cg: u64,
    /// Slots decided after they were due, which `livesync` drops.
    pub late_decisions: u64,
    /// Undecided slots given up because too many were held.
    pub evicted: u64,
    /// Frames the program `appsrc` refused because it was full.
    pub refused: u64,
    /// Changes of the input's format.
    pub transitions: u64,
    /// Slots sent as clean feed during a format change.
    pub transition_clean: u64,
    /// Fixed output rate: program frames repeating a decided frame on the
    /// grid, and decided frames that covered no grid slot.
    pub rate_repeats: u64,
    pub rate_drops: u64,
}

impl Counters {
    pub fn report(&self) -> String {
        format!(
            "selector: {} CG, {} clean feed, {} empty, {} late CG, {} decided late, {} evicted, \
             {} refused, {} format changes ({} slots without CG in them), rate conversion {} \
             repeats and {} drops",
            self.cg,
            self.clean,
            self.empty,
            self.late_cg,
            self.late_decisions,
            self.evicted,
            self.refused,
            self.transitions,
            self.transition_clean,
            self.rate_repeats,
            self.rate_drops
        )
    }
}

/// A slot's decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Cg,
    Clean,
    Empty,
}

/// A captured frame: its program sequence, capture time and rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub sequence: u64,
    pub capture: gst::ClockTime,
    pub rate: Rate,
}

struct Slot<B> {
    capture: gst::ClockTime,
    rate: Rate,
    clean: Option<B>,
    cg: Option<B>,
}

/// The slots waiting for their deadline, by program sequence (capture order).
struct Slots<B> {
    slots: BTreeMap<u64, Slot<B>>,
    /// Every slot up to this one is decided.
    decided_through: Option<u64>,
    capacity: usize,
    /// The format change in progress: its first slot, and its first frame in
    /// the new format.
    transition: Option<(u64, u64)>,
    counters: Counters,
}

impl<B> Slots<B> {
    fn new(capacity: usize) -> Self {
        Self {
            slots: BTreeMap::new(),
            decided_through: None,
            capacity,
            transition: None,
            counters: Counters::default(),
        }
    }

    /// Adds one branch's frame to its slot. A frame for a decided slot is
    /// dropped (and counted if it is CG).
    fn offer(&mut self, key: Frame, frame: B, cg: bool) {
        let sequence = key.sequence;
        if self.decided_through.is_some_and(|done| sequence <= done) {
            self.counters.late_cg += u64::from(cg);
            return;
        }
        let slot = self.slots.entry(sequence).or_insert(Slot {
            capture: key.capture,
            rate: key.rate,
            clean: None,
            cg: None,
        });
        if cg {
            slot.cg = Some(frame);
        } else {
            slot.clean = Some(frame);
        }
        while self.slots.len() > self.capacity {
            let (evicted, _) = self.slots.pop_first().expect("over capacity");
            self.decided_through = Some(evicted);
            self.counters.evicted += 1;
        }
    }

    /// The next slot to decide and when its frame was captured.
    fn next(&self) -> Option<(u64, gst::ClockTime)> {
        self.slots
            .first_key_value()
            .map(|(sequence, slot)| (*sequence, slot.capture))
    }

    /// A format change whose first frame is `first`: the transition covers the
    /// slots from the oldest undecided one.
    fn begin_transition(&mut self, first: u64) {
        let oldest = self
            .slots
            .first_key_value()
            .map_or(first, |(sequence, _)| (*sequence).min(first));
        let start = self
            .transition
            .map_or(oldest, |(start, _)| start.min(oldest));
        self.transition = Some((start, first));
        self.counters.transitions += 1;
    }

    /// Decides the next slot: its CG, else its clean feed. The second value
    /// tells whether a slot without CG belongs to a format change.
    fn decide(&mut self) -> Option<(Frame, Choice, Option<B>, bool)> {
        let (sequence, slot) = self.slots.pop_first()?;
        self.decided_through = Some(sequence);
        let (choice, frame) = match (slot.cg, slot.clean) {
            (Some(cg), _) => (Choice::Cg, Some(cg)),
            (None, Some(clean)) => (Choice::Clean, Some(clean)),
            (None, None) => (Choice::Empty, None),
        };
        match choice {
            Choice::Cg => self.counters.cg += 1,
            Choice::Clean => self.counters.clean += 1,
            Choice::Empty => self.counters.empty += 1,
        }
        let mut transition = false;
        if let Some((start, first)) = self.transition
            && sequence >= start
        {
            match choice {
                Choice::Clean => {
                    self.counters.transition_clean += 1;
                    transition = true;
                }
                Choice::Cg if sequence >= first => self.transition = None,
                // Nothing goes out for an empty slot (the output repeats).
                Choice::Cg | Choice::Empty => {}
            }
        }
        let key = Frame {
            sequence,
            capture: slot.capture,
            rate: slot.rate,
        };
        Some((key, choice, frame, transition))
    }
}

/// The program's slots at a fixed rate: slot `k` is due at
/// `origin + rate.frame_ns(k)`, the origin being the first decided frame's due time.
#[derive(Debug)]
struct Grid {
    rate: Rate,
    origin: Option<u64>,
    /// The first slot not filled yet.
    next: u64,
}

impl Grid {
    fn new(rate: Rate) -> Self {
        Self {
            rate,
            origin: None,
            next: 0,
        }
    }

    /// The slots not filled yet that a frame due at `due` ns fills until the
    /// next one, `period` ns later, is due: each slot's due time and duration.
    /// Slots before it that were never filled stay empty (the output repeats).
    ///
    /// Capture times are exact frame times rounded down, so frames are a period
    /// or a nanosecond more apart: a frame fills the slots from 2 ns before its
    /// due time to 1 ns before the next one at the soonest, which leaves no
    /// slot between two frames and never takes the next frame's own.
    fn fill(&mut self, due: u64, period: u64) -> Vec<(u64, u64)> {
        let (start, end) = (due.saturating_sub(2), due + period - 1);
        let origin = *self.origin.get_or_insert(due);
        let rate = self.rate;
        let first = match start.checked_sub(origin) {
            None | Some(0) => 0,
            Some(offset) => {
                let at = rate.frame_at(offset);
                at + u64::from(rate.frame_ns(at) < offset)
            }
        };
        let mut slot = first.max(self.next);
        let mut filled = Vec::new();
        while origin + rate.frame_ns(slot) < end {
            let due = rate.frame_ns(slot);
            filled.push((origin + due, rate.frame_ns(slot + 1) - due));
            slot += 1;
        }
        self.next = self.next.max(slot);
        filled
    }
}

struct Shared {
    slots: Mutex<Slots<gst::Buffer>>,
    wake: Condvar,
    stop: Mutex<bool>,
}

/// Where decided frames go: the program `appsrc`, at the output's extent and
/// under the output rate policy.
pub struct Program {
    pub appsrc: AppSrc,
    /// The output's extent and its rate at start (the fixed rate, if fixed).
    pub output: VideoMode,
    pub policy: OutputRate,
    pub recorder: SdiRecorder,
}

/// The selector and its thread; clones offer frames to the same slots.
#[derive(Clone)]
pub struct Selector {
    shared: Arc<Shared>,
    timing: Timing,
    thread: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl Selector {
    /// Starts deciding slots into `program`.
    pub fn start(program: Program, timeline: Timeline, timing: Timing) -> Result<Self, Error> {
        let shared = Arc::new(Shared {
            slots: Mutex::new(Slots::new(timing.capacity())),
            wake: Condvar::new(),
            stop: Mutex::new(false),
        });
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("chr-core-select".into())
                .spawn(move || run(&shared, &program, &timeline, timing))?
        };
        Ok(Self {
            shared,
            timing,
            thread: Arc::new(Mutex::new(Some(thread))),
        })
    }

    pub fn timing(&self) -> Timing {
        self.timing
    }

    /// The clean feed of `key`.
    pub fn clean(&self, key: Frame, frame: gst::Buffer) {
        self.offer(key, frame, false);
    }

    /// The rendered frame of `key`.
    pub fn cg(&self, key: Frame, frame: gst::Buffer) {
        self.offer(key, frame, true);
    }

    fn offer(&self, key: Frame, frame: gst::Buffer, cg: bool) {
        lock(&self.shared.slots).offer(key, frame, cg);
        self.shared.wake.notify_one();
    }

    /// The input's format changed with frame `first`.
    pub fn begin_transition(&self, first: u64) {
        lock(&self.shared.slots).begin_transition(first);
    }

    pub fn counters(&self) -> Counters {
        lock(&self.shared.slots).counters
    }

    /// Stops the thread; frames still held are released.
    pub fn stop(&self) {
        *lock(&self.shared.stop) = true;
        self.shared.wake.notify_one();
        if let Some(thread) = lock(&self.thread).take() {
            let _ = thread.join();
        }
        lock(&self.shared.slots).slots.clear();
    }
}

fn run(shared: &Shared, program: &Program, timeline: &Timeline, timing: Timing) {
    let mut program_rate = program.output.fps;
    let mut grid = match program.policy {
        OutputRate::Follow => None,
        OutputRate::Fixed(rate) => Some(Grid::new(rate)),
    };
    let mut slots = lock(&shared.slots);
    while !*lock(&shared.stop) {
        let Some((_, capture)) = slots.next() else {
            slots = wait(&shared.wake, slots, IDLE);
            continue;
        };
        let now = timeline.now();
        let deadline = timing.deadline(capture);
        if now < deadline {
            let left = Duration::from_nanos((deadline - now).nseconds());
            slots = wait(&shared.wake, slots, left.min(IDLE));
            continue;
        }
        let (key, choice, frame, transition) = slots.decide().expect("a next slot");
        if transition {
            program.recorder.transition_clean();
        }
        let due = timing.due(key.capture);
        if now >= due {
            slots.counters.late_decisions += 1;
        }
        let Some(frame) = frame else {
            log::debug!(
                "slot of frame {} has no frame: the output repeats",
                key.sequence
            );
            continue;
        };
        if program.appsrc.current_level_buffers() >= PROGRAM_QUEUE {
            slots.counters.refused += 1;
            continue;
        }
        // Each program frame: its due time and duration, in ns.
        let times = match &mut grid {
            None => vec![(due.nseconds(), key.rate.period_ns())],
            Some(grid) => {
                let times = grid.fill(due.nseconds(), key.rate.period_ns());
                match times.len() {
                    0 => slots.counters.rate_drops += 1,
                    n => slots.counters.rate_repeats += n as u64 - 1,
                }
                times
            }
        };
        drop(slots);
        if grid.is_none() && key.rate != program_rate {
            // livesync and the output take the new rate from these caps.
            program_rate = key.rate;
            let caps = uyvy_caps(VideoMode {
                fps: program_rate,
                ..program.output
            });
            program.appsrc.set_caps(Some(&caps));
            program.recorder.set_mode(VideoMode {
                fps: program_rate,
                ..program.output
            });
            log::info!("program output now at {} fps", program_rate.label());
        }
        for (pts, duration) in times {
            let buffer = program_frame(&frame, key, choice, timeline, pts, duration);
            if let Err(error) = program.appsrc.push_buffer(buffer) {
                log::warn!("program appsrc refused frame {}: {error:?}", key.sequence);
            }
        }
        slots = lock(&shared.slots);
    }
}

/// The frame as it goes out: its memory (not a copy), stamped with its slot's
/// time (`pts` and `duration` ns of running time) and [`PROGRAM_META`].
fn program_frame(
    frame: &gst::Buffer,
    key: Frame,
    choice: Choice,
    timeline: &Timeline,
    pts: u64,
    duration: u64,
) -> gst::Buffer {
    let mut buffer = gst::Buffer::new();
    let writable = buffer.get_mut().expect("new buffer is not shared");
    for memory in frame.iter_memories_owned() {
        writable.append_memory(memory);
    }
    writable.set_pts(gst::ClockTime::from_nseconds(pts));
    writable.set_duration(gst::ClockTime::from_nseconds(duration));
    writable.set_offset(key.sequence);
    writable.set_offset_end(key.sequence + 1);
    let mut meta = gst::meta::CustomMeta::add(writable, PROGRAM_META)
        .expect("the program meta is registered in init");
    let structure = meta.mut_structure();
    structure.set("cg", choice == Choice::Cg);
    structure.set("sequence", key.sequence);
    structure.set("source-ns", timeline.absolute(key.capture).nseconds());
    buffer
}

fn wait<'a, T>(wake: &Condvar, guard: MutexGuard<'a, T>, timeout: Duration) -> MutexGuard<'a, T> {
    wake.wait_timeout(guard, timeout)
        .unwrap_or_else(PoisonError::into_inner)
        .0
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> gst::ClockTime {
        gst::ClockTime::from_mseconds(ms)
    }

    fn key(sequence: u64, ms: u64) -> Frame {
        Frame {
            sequence,
            capture: at(ms),
            rate: Rate::fps(50),
        }
    }

    fn decided<B>(slots: &mut Slots<B>) -> Vec<(u64, Choice, Option<B>)> {
        std::iter::from_fn(|| slots.decide())
            .map(|(key, choice, frame, _)| (key.sequence, choice, frame))
            .collect()
    }

    #[test]
    fn each_slot_gets_its_own_cg_or_its_own_clean_feed() {
        let mut slots = Slots::new(8);
        for sequence in 0..3 {
            slots.offer(
                key(sequence, 20 * sequence),
                format!("clean {sequence}"),
                false,
            );
        }
        slots.offer(key(1, 20), "cg 1".to_owned(), true);
        slots.offer(key(2, 40), "cg 2".to_owned(), true);
        assert_eq!(
            decided(&mut slots),
            [
                (0, Choice::Clean, Some("clean 0".to_owned())),
                (1, Choice::Cg, Some("cg 1".to_owned())),
                (2, Choice::Cg, Some("cg 2".to_owned())),
            ]
        );
        slots.offer(key(0, 0), "cg 0".to_owned(), true);
        assert!(slots.next().is_none(), "a late CG frame takes no slot");
        assert_eq!(
            slots.counters,
            Counters {
                cg: 2,
                clean: 1,
                late_cg: 1,
                ..Counters::default()
            }
        );
    }

    #[test]
    fn slots_are_decided_in_capture_order_and_bounded() {
        let mut slots = Slots::new(2);
        slots.offer(key(5, 100), "cg 5", true);
        slots.offer(key(4, 80), "clean 4", false);
        assert_eq!(
            slots.next(),
            Some((4, at(80))),
            "a CG-first slot keeps its place"
        );
        slots.offer(key(6, 120), "clean 6", false);
        assert_eq!(slots.counters.evicted, 1, "4 is given up");
        slots.offer(key(4, 80), "cg 4", true);
        assert_eq!(slots.counters.late_cg, 1);
        let choices: Vec<_> = decided(&mut slots)
            .into_iter()
            .map(|(sequence, choice, _)| (sequence, choice))
            .collect();
        assert_eq!(choices, [(5, Choice::Cg), (6, Choice::Clean)]);
    }

    #[test]
    fn a_format_change_counts_its_slots_without_cg_until_the_first_cg() {
        let mut slots = Slots::new(16);
        for sequence in 0..8 {
            slots.offer(key(sequence, 20 * sequence), "clean", false);
        }
        slots.offer(key(0, 0), "cg", true);
        assert_eq!(
            slots.decide().map(|(k, c, ..)| (k.sequence, c)),
            Some((0, Choice::Cg))
        );
        // Frame 3 is the first of the new format; 1 and 2 are still undecided:
        // 1 lost CG with the old tracking session, 2 still got it.
        slots.begin_transition(3);
        slots.offer(key(2, 40), "cg", true);
        slots.offer(key(6, 120), "cg", true);
        let transition: Vec<_> = std::iter::from_fn(|| slots.decide())
            .map(|(key, choice, _, transition)| (key.sequence, choice, transition))
            .collect();
        assert_eq!(
            transition,
            [
                (1, Choice::Clean, true),
                (2, Choice::Cg, false),
                (3, Choice::Clean, true),
                (4, Choice::Clean, true),
                (5, Choice::Clean, true),
                (6, Choice::Cg, false),
                (7, Choice::Clean, false),
            ]
        );
        assert_eq!(
            (slots.counters.transitions, slots.counters.transition_clean),
            (1, 4)
        );
    }

    #[test]
    fn deadlines_follow_capture_by_the_program_delay_at_any_rate() {
        let timing = Timing::new(Duration::from_millis(100));
        assert_eq!(timing.due(at(1000)), at(1100));
        assert_eq!(timing.deadline(at(1000)), at(1100) - timing.margin);
        assert_eq!(timing.capacity(), 12 + SLACK_SLOTS);
    }

    /// Grid slots that frames due every `input` ns fill, as `(input frame, grid slot)`.
    fn convert(output: Rate, input: Rate, frames: u64) -> Vec<(u64, u64)> {
        let mut grid = Grid::new(output);
        let start = 1_000_000_000;
        (0..frames)
            .flat_map(|n| {
                let due = start + input.frame_ns(n);
                grid.fill(due, input.period_ns())
                    .into_iter()
                    .map(move |(at, _)| (n, output.frame_at(at - start)))
            })
            .collect()
    }

    #[test]
    fn a_fixed_rate_shows_the_latest_frame_due_in_every_grid_slot() {
        let fps = Rate::fps;
        // 25 to 50: every frame twice.
        assert_eq!(
            convert(fps(50), fps(25), 3),
            [(0, 0), (0, 1), (1, 2), (1, 3), (2, 4), (2, 5)]
        );
        // 50 to 25: every other frame.
        assert_eq!(convert(fps(25), fps(50), 6), [(0, 0), (2, 1), (4, 2)]);
        // The same rate: one slot each.
        let ntsc = Rate::new(30000, 1001).unwrap();
        let same = convert(ntsc, ntsc, 30_000);
        assert!(
            same.iter()
                .enumerate()
                .all(|(i, &(n, k))| n == i as u64 && k == i as u64)
        );
        // 29.97 to 50 for 1001 s: each grid slot exactly once, from the latest
        // frame due, never a gap or a slot twice.
        let slots = convert(fps(50), ntsc, 30_000);
        assert_eq!(slots.len(), 50_050);
        for (k, &(n, slot)) in slots.iter().enumerate() {
            assert_eq!(slot, k as u64);
            let at = fps(50).frame_ns(slot);
            assert!(ntsc.frame_ns(n) <= at && at < ntsc.frame_ns(n + 1), "{k}");
        }
    }

    #[test]
    fn a_fixed_grid_leaves_the_slots_of_a_missing_frame_empty() {
        let mut grid = Grid::new(Rate::fps(50));
        assert_eq!(grid.fill(0, 40_000_000).len(), 2);
        // The frame due at 40 ms never came: its slots stay empty.
        let filled = grid.fill(80_000_000, 40_000_000);
        assert_eq!(
            filled,
            [(80_000_000, 20_000_000), (100_000_000, 20_000_000)]
        );
        // A frame due before the slots already filled adds none.
        assert!(grid.fill(90_000_000, 20_000_000).is_empty());
    }
}
