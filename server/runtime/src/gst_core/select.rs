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

use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::JoinHandle,
    time::Duration,
};

use gstreamer as gst;
use gstreamer_app::AppSrc;

use super::{DECISION_MARGIN, Error, PROGRAM_META, Timeline};

/// Program frames the `appsrc` may hold; the selector hands over one per slot.
const PROGRAM_QUEUE: u64 = 4;
/// How long an idle selector sleeps before it checks for a stop.
const IDLE: Duration = Duration::from_millis(100);
/// Slots held beyond those of the program delay, for frames of one branch
/// whose other branch is behind.
const SLACK_SLOTS: usize = 8;

/// When a captured frame's slot is due and decided, in running time.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub delay: gst::ClockTime,
    pub margin: gst::ClockTime,
    pub period: gst::ClockTime,
}

impl Timing {
    pub fn new(delay: Duration, period: Duration) -> Self {
        let ns = |duration: Duration| gst::ClockTime::from_nseconds(duration.as_nanos() as u64);
        Self {
            delay: ns(delay),
            margin: ns(DECISION_MARGIN),
            period: ns(period),
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

    /// Slots in flight at once: a program delay's worth plus slack.
    fn capacity(&self) -> usize {
        let slots = self
            .delay
            .nseconds()
            .div_ceil(self.period.nseconds().max(1));
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
}

impl Counters {
    pub fn report(&self) -> String {
        format!(
            "selector: {} CG, {} clean feed, {} empty, {} late CG, {} decided late, {} evicted, \
             {} refused",
            self.cg,
            self.clean,
            self.empty,
            self.late_cg,
            self.late_decisions,
            self.evicted,
            self.refused
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

struct Slot<B> {
    capture: gst::ClockTime,
    clean: Option<B>,
    cg: Option<B>,
}

/// The slots waiting for their deadline, by program sequence (capture order).
struct Slots<B> {
    slots: BTreeMap<u64, Slot<B>>,
    /// Every slot up to this one is decided.
    decided_through: Option<u64>,
    capacity: usize,
    counters: Counters,
}

impl<B> Slots<B> {
    fn new(capacity: usize) -> Self {
        Self {
            slots: BTreeMap::new(),
            decided_through: None,
            capacity,
            counters: Counters::default(),
        }
    }

    /// Adds one branch's frame to slot `sequence`. A frame for a decided slot is
    /// dropped (and counted if it is CG).
    fn offer(&mut self, sequence: u64, capture: gst::ClockTime, frame: B, cg: bool) {
        if self.decided_through.is_some_and(|done| sequence <= done) {
            self.counters.late_cg += u64::from(cg);
            return;
        }
        let slot = self.slots.entry(sequence).or_insert(Slot {
            capture,
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

    /// Decides the next slot: its CG, else its clean feed.
    fn decide(&mut self) -> Option<(u64, gst::ClockTime, Choice, Option<B>)> {
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
        Some((sequence, slot.capture, choice, frame))
    }
}

struct Shared {
    slots: Mutex<Slots<gst::Buffer>>,
    wake: Condvar,
    stop: Mutex<bool>,
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
    pub fn start(program: AppSrc, timeline: Timeline, timing: Timing) -> Result<Self, Error> {
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

    /// The clean feed of frame `sequence`, captured at `capture`.
    pub fn clean(&self, sequence: u64, capture: gst::ClockTime, frame: gst::Buffer) {
        self.offer(sequence, capture, frame, false);
    }

    /// The rendered frame `sequence`, captured at `capture`.
    pub fn cg(&self, sequence: u64, capture: gst::ClockTime, frame: gst::Buffer) {
        self.offer(sequence, capture, frame, true);
    }

    fn offer(&self, sequence: u64, capture: gst::ClockTime, frame: gst::Buffer, cg: bool) {
        lock(&self.shared.slots).offer(sequence, capture, frame, cg);
        self.shared.wake.notify_one();
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

fn run(shared: &Shared, program: &AppSrc, timeline: &Timeline, timing: Timing) {
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
        let (sequence, capture, choice, frame) = slots.decide().expect("a next slot");
        if now >= timing.due(capture) {
            slots.counters.late_decisions += 1;
        }
        let Some(frame) = frame else {
            log::debug!("slot of frame {sequence} has no frame: the output repeats");
            continue;
        };
        if program.current_level_buffers() >= PROGRAM_QUEUE {
            slots.counters.refused += 1;
            continue;
        }
        let buffer = program_frame(&frame, sequence, capture, choice, timeline, timing);
        drop(slots);
        if let Err(error) = program.push_buffer(buffer) {
            log::warn!("program appsrc refused frame {sequence}: {error:?}");
        }
        slots = lock(&shared.slots);
    }
}

/// The frame as it goes out: its memory (not a copy), stamped with its slot's
/// time and [`PROGRAM_META`].
fn program_frame(
    frame: &gst::Buffer,
    sequence: u64,
    capture: gst::ClockTime,
    choice: Choice,
    timeline: &Timeline,
    timing: Timing,
) -> gst::Buffer {
    let mut buffer = gst::Buffer::new();
    let writable = buffer.get_mut().expect("new buffer is not shared");
    for memory in frame.iter_memories_owned() {
        writable.append_memory(memory);
    }
    writable.set_pts(timing.due(capture));
    writable.set_duration(timing.period);
    writable.set_offset(sequence);
    writable.set_offset_end(sequence + 1);
    let mut meta = gst::meta::CustomMeta::add(writable, PROGRAM_META)
        .expect("the program meta is registered in init");
    let structure = meta.mut_structure();
    structure.set("cg", choice == Choice::Cg);
    structure.set("sequence", sequence);
    structure.set("source-ns", timeline.absolute(capture).nseconds());
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

    #[test]
    fn each_slot_gets_its_own_cg_or_its_own_clean_feed() {
        let mut slots = Slots::new(8);
        for sequence in 0..3 {
            slots.offer(
                sequence,
                at(20 * sequence),
                format!("clean {sequence}"),
                false,
            );
        }
        slots.offer(1, at(20), "cg 1".to_owned(), true);
        slots.offer(2, at(40), "cg 2".to_owned(), true);
        let decided: Vec<_> = std::iter::from_fn(|| slots.decide())
            .map(|(sequence, _, choice, frame)| (sequence, choice, frame.unwrap()))
            .collect();
        assert_eq!(
            decided,
            [
                (0, Choice::Clean, "clean 0".to_owned()),
                (1, Choice::Cg, "cg 1".to_owned()),
                (2, Choice::Cg, "cg 2".to_owned()),
            ]
        );
        slots.offer(0, at(0), "cg 0".to_owned(), true);
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
        slots.offer(5, at(100), "cg 5", true);
        slots.offer(4, at(80), "clean 4", false);
        assert_eq!(
            slots.next(),
            Some((4, at(80))),
            "a CG-first slot keeps its place"
        );
        slots.offer(6, at(120), "clean 6", false);
        assert_eq!(slots.counters.evicted, 1, "4 is given up");
        slots.offer(4, at(80), "cg 4", true);
        assert_eq!(slots.counters.late_cg, 1);
        assert_eq!(
            slots.decide().map(|(s, _, c, _)| (s, c)),
            Some((5, Choice::Cg))
        );
        assert_eq!(
            slots.decide().map(|(s, _, c, _)| (s, c)),
            Some((6, Choice::Clean))
        );
    }

    #[test]
    fn deadlines_follow_capture_by_the_program_delay() {
        let timing = Timing::new(Duration::from_millis(100), Duration::from_millis(20));
        assert_eq!(timing.due(at(1000)), at(1100));
        assert_eq!(timing.deadline(at(1000)), at(1100) - timing.margin);
        assert_eq!(timing.capacity(), 5 + SLACK_SLOTS);
    }
}
