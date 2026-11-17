//! Simulated SDI program output port.
//!
//! The pipeline hands rendered frames, already in the SDI pixel format, to a bounded
//! queue. An output thread sends one frame per slot at the video mode's cadence from its
//! own monotonic clock, like an SDI card: when no new frame is ready it repeats the
//! previous one, and when the queue is full the newest render is dropped. Every slot that
//! leaves the port, repeats included, is also copied into the shared-memory program tap
//! that the separate `chr-monitor` process encodes for viewers. Neither the tap nor an
//! in-process observer can delay the port.
//!
//! This is a simulation for development without SDI hardware; a real output adapter
//! (for example a DeckLink card) replaces the output thread.

use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use crate::frame_submissions::{OutputFrame, OutputPort, Provenance};
use chr_common::methods::sdi::{AudioStatus, SdiStatus};
use chr_common::rate::Rate;
use chr_common::scene_frame as w;

pub const PIXEL_FORMAT: &str = "UYVY 4:2:2 8-bit, BT.709 limited range";
/// Rendered frames buffered ahead of the output clock.
const OUTPUT_QUEUE: usize = 3;
/// Transmitted slots buffered ahead of an in-process observer.
const OBSERVER_QUEUE: usize = 2;
/// How often blocked threads check for shutdown.
pub const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoMode {
    pub width: u32,
    pub height: u32,
    pub fps: Rate,
}

impl VideoMode {
    /// For example `1920x1080p50` or `1920x1080p59.94`.
    pub fn name(&self) -> String {
        format!("{}x{}p{}", self.width, self.height, self.fps.label())
    }

    /// One frame, rounded down to the nanosecond; schedules use
    /// [`Rate::frame_time`], which does not drift.
    pub fn frame_duration(&self) -> Duration {
        self.fps.period()
    }

    /// The input ring's metadata of a frame in this mode.
    pub fn input_meta(&self) -> chr_shm::input::SlotMeta {
        chr_shm::input::SlotMeta {
            width: self.width,
            height: self.height,
            rate_num: self.fps.num(),
            rate_den: self.fps.den(),
        }
    }

    /// The program tap's metadata of a slot in this mode.
    pub fn tap_meta(&self, source_frame: u64, program: bool) -> chr_shm::tap::SlotMeta {
        chr_shm::tap::SlotMeta {
            source_frame,
            program,
            width: self.width,
            height: self.height,
            rate_num: self.fps.num(),
            rate_den: self.fps.den(),
        }
    }
}

/// Which of an interlaced frame's two fields comes first in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldOrder {
    /// The top field (even lines from 0): 1080i, SMPTE 274M.
    TopFirst,
    /// The bottom field (odd lines): 480i/NTSC SD.
    BottomFirst,
}

/// How a frame's lines are scanned. An interlaced frame carries two fields
/// half a frame period apart, woven line by line; its rate is the **frame**
/// rate, as in GStreamer's caps: 1080i50 is 25 frames (50 fields) per second,
/// its `Rate` 25. Deadlines, the program delay and every frame's slot count
/// frames; only names use the field rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scan {
    #[default]
    Progressive,
    Interlaced(FieldOrder),
}

impl Scan {
    /// GStreamer's `interlace-mode` and `field-order` caps fields.
    pub fn caps_fields(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Progressive => ("progressive", None),
            Self::Interlaced(FieldOrder::TopFirst) => ("interleaved", Some("top-field-first")),
            Self::Interlaced(FieldOrder::BottomFirst) => {
                ("interleaved", Some("bottom-field-first"))
            }
        }
    }
}

/// A video format: extent, frame rate and scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub mode: VideoMode,
    pub scan: Scan,
}

impl Format {
    /// `1920x1080p50`, or `1920x1080i50` for 25 interlaced frames per second:
    /// interlaced formats are named by their field rate.
    pub fn name(&self) -> String {
        match self.scan {
            Scan::Progressive => self.mode.name(),
            Scan::Interlaced(_) => format!(
                "{}x{}i{}",
                self.mode.width,
                self.mode.height,
                self.field_rate()
                    .map_or_else(|| format!("{}x2", self.mode.fps.label()), Rate::label)
            ),
        }
    }

    /// Fields per second: twice the frame rate if interlaced; `None` above
    /// the highest rate.
    pub fn field_rate(&self) -> Option<Rate> {
        match self.scan {
            Scan::Progressive => Some(self.mode.fps),
            Scan::Interlaced(_) => Rate::new(self.mode.fps.num() * 2, self.mode.fps.den()).ok(),
        }
    }
}

/// A rate as the command lines take it: frames per second (`50`, `59.94`,
/// `30000/1001`), or with an `i` suffix interlaced, named by its field rate as
/// broadcast does: `50i` is 25 interlaced frames per second, top field first.
pub fn scanned_rate(text: &str) -> Result<(Rate, Scan), String> {
    match text.strip_suffix('i') {
        None => Ok((text.parse().map_err(|e| format!("{e}"))?, Scan::Progressive)),
        Some(fields) => {
            let fields: Rate = fields.parse().map_err(|e| format!("{e}"))?;
            Ok((
                Rate::new(fields.num(), fields.den() * 2).map_err(|e| format!("{e}"))?,
                Scan::Interlaced(FieldOrder::TopFirst),
            ))
        }
    }
}

impl From<VideoMode> for Format {
    fn from(mode: VideoMode) -> Self {
        Self {
            mode,
            scan: Scan::Progressive,
        }
    }
}

/// One rendered frame in the SDI pixel format: packed UYVY rows, `width * 2` bytes each.
pub struct SdiFrame {
    /// Render frame index (logical time).
    pub source_frame: u64,
    pub uyvy: Arc<Vec<u8>>,
    /// Keeps the separately allocated output alive through queued/current/repeated slots.
    pub program: Option<Arc<OutputFrame>>,
}

#[derive(Clone, Debug)]
pub struct ProgramAssociation {
    pub provenance: Arc<Provenance>,
    pub output_frame_id: w::Id,
    pub output_stream_id: w::Id,
}

/// One output slot as it left the port.
pub struct SdiSlot {
    /// Output slot index, counted at the video mode's cadence.
    pub slot: u64,
    pub uyvy: Arc<Vec<u8>>,
    pub program: Option<Arc<OutputFrame>>,
    pub association: Option<ProgramAssociation>,
}

#[derive(Default)]
struct Counters {
    frames_output: AtomicU64,
    repeated: AtomicU64,
    dropped: AtomicU64,
    last_source_frame: AtomicU64,
    has_source_frame: AtomicBool,
    format_changes: AtomicU64,
    transition_clean_slots: AtomicU64,
    input_loss_slots: AtomicU64,
    input_losses: AtomicU64,
    caption_frames: AtomicU64,
    /// The latest slot's timecode and the audio output, of the GStreamer core.
    details: Mutex<Details>,
}

#[derive(Default)]
struct Details {
    last_timecode: Option<String>,
    audio: Option<AudioStatus>,
}

/// The pipeline's handle for submitting frames.
#[derive(Clone)]
pub struct SdiInput {
    frames: SyncSender<SdiFrame>,
    counters: Arc<Counters>,
    frame_bytes: usize,
    program_enabled: bool,
}

impl OutputPort for SdiInput {
    fn try_handoff(&mut self, frame: OutputFrame) -> Result<(), OutputFrame> {
        if !self.program_enabled {
            let _ = frame.observer.failed();
            return Ok(());
        }
        let Some(pixels) = frame.resource.payload::<Arc<Vec<u8>>>() else {
            let _ = frame.observer.failed();
            return Ok(());
        };
        if pixels.len() != self.frame_bytes {
            let _ = frame.observer.failed();
            return Ok(());
        }
        let sdi = SdiFrame {
            source_frame: frame.provenance.input.frame.sequence.get(),
            uyvy: Arc::clone(pixels),
            program: Some(Arc::new(frame)),
        };
        match self.frames.try_send(sdi) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(mut sdi)) => {
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                Err(Arc::try_unwrap(sdi.program.take().expect("program frame"))
                    .unwrap_or_else(|_| unreachable!("unpublished frame is uniquely owned")))
            }
            Err(TrySendError::Disconnected(_)) => Ok(()),
        }
    }
}

impl SdiInput {
    /// Queues a frame without blocking the render loop. A full queue drops this frame.
    pub fn submit(&self, frame: SdiFrame) {
        match self.frames.try_send(frame) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// Read-only view of the output counters.
#[derive(Clone)]
pub struct SdiStats {
    format: Arc<Mutex<Format>>,
    counters: Arc<Counters>,
}

/// Records the slots of an output driven elsewhere: chr-server's GStreamer
/// core (`--pipeline gst`, #154), whose output sink takes every slot.
#[derive(Clone)]
pub struct SdiRecorder {
    format: Arc<Mutex<Format>>,
    counters: Arc<Counters>,
}

impl SdiRecorder {
    /// One transmitted slot carrying capture frame `source_frame`; a repeat
    /// of the previous slot's frame if `repeated`.
    pub fn output(&self, source_frame: u64, repeated: bool) {
        let counters = &self.counters;
        counters.frames_output.fetch_add(1, Ordering::Relaxed);
        if repeated {
            counters.repeated.fetch_add(1, Ordering::Relaxed);
        }
        counters
            .last_source_frame
            .store(source_frame, Ordering::Relaxed);
        counters.has_source_frame.store(true, Ordering::Release);
    }

    /// The output's format from its next slot on.
    pub fn set_format(&self, output: Format) {
        *self.format.lock().unwrap_or_else(PoisonError::into_inner) = output;
    }

    /// One change of the input's format, whether or not the output's mode
    /// follows it.
    pub fn format_changed(&self) {
        self.counters.format_changes.fetch_add(1, Ordering::Relaxed);
    }

    /// One more slot of clean feed during a format change.
    pub fn transition_clean(&self) {
        self.counters
            .transition_clean_slots
            .fetch_add(1, Ordering::Relaxed);
    }

    /// One slot filled for a lost input; `first` if the loss begins with it.
    pub fn input_loss(&self, first: bool) {
        self.counters
            .input_loss_slots
            .fetch_add(1, Ordering::Relaxed);
        if first {
            self.counters.input_losses.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The timecode of the slot just output and whether it carried captions.
    pub fn ancillary(&self, timecode: Option<String>, captions: bool) {
        if captions {
            self.counters.caption_frames.fetch_add(1, Ordering::Relaxed);
        }
        if timecode.is_some() {
            lock(&self.counters.details).last_timecode = timecode;
        }
    }

    /// The audio output as it is now.
    pub fn audio(&self, audio: AudioStatus) {
        lock(&self.counters.details).audio = Some(audio);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SdiStats {
    fn new(format: Format, counters: Arc<Counters>) -> Self {
        Self {
            format: Arc::new(Mutex::new(format)),
            counters,
        }
    }

    /// Stats of an output that `SdiRecorder` records.
    pub fn recorded(format: impl Into<Format>) -> (Self, SdiRecorder) {
        let stats = Self::new(format.into(), Arc::default());
        let recorder = SdiRecorder {
            format: Arc::clone(&stats.format),
            counters: Arc::clone(&stats.counters),
        };
        (stats, recorder)
    }

    /// Stats for an output that never runs, for tests of status reporting.
    #[cfg(test)]
    pub fn idle(mode: VideoMode) -> Self {
        Self::new(mode.into(), Arc::default())
    }

    pub fn status(&self) -> SdiStatus {
        let counters = &self.counters;
        let format = *lock(&self.format);
        let details = lock(&counters.details);
        SdiStatus {
            video_mode: format.name(),
            rate: format.mode.fps,
            pixel_format: PIXEL_FORMAT.to_owned(),
            frames_output: counters.frames_output.load(Ordering::Relaxed),
            repeated_frames: counters.repeated.load(Ordering::Relaxed),
            dropped_frames: counters.dropped.load(Ordering::Relaxed),
            last_source_frame: counters
                .has_source_frame
                .load(Ordering::Acquire)
                .then(|| counters.last_source_frame.load(Ordering::Relaxed)),
            format_changes: counters.format_changes.load(Ordering::Relaxed),
            transition_clean_slots: counters.transition_clean_slots.load(Ordering::Relaxed),
            input_loss_slots: counters.input_loss_slots.load(Ordering::Relaxed),
            input_losses: counters.input_losses.load(Ordering::Relaxed),
            last_timecode: details.last_timecode.clone(),
            caption_frames: counters.caption_frames.load(Ordering::Relaxed),
            audio: details.audio.clone(),
        }
    }
}

/// Where transmitted slots are copied besides the port itself.
#[derive(Default)]
pub struct Taps {
    /// Shared-memory program tap for `chr-monitor`; written for every slot.
    pub ring: Option<chr_shm::Writer>,
    /// In-process observer (tests); slots it cannot take are skipped for it.
    pub observer: Option<SyncSender<SdiSlot>>,
}

impl Taps {
    /// Creates the program tap ring for `mode` at `path`.
    pub fn ring(mode: VideoMode, path: &std::path::Path) -> Result<Self, chr_shm::Error> {
        let ring = chr_shm::Writer::create(path, chr_shm::tap::layout(mode.width, mode.height))?;
        Ok(Self {
            ring: Some(ring),
            observer: None,
        })
    }

    /// Only an in-process observer, whose receiver yields transmitted slots in order.
    pub fn observed() -> (Self, Receiver<SdiSlot>) {
        let (observer, slots) = mpsc::sync_channel(OBSERVER_QUEUE);
        let taps = Self {
            ring: None,
            observer: Some(observer),
        };
        (taps, slots)
    }
}

pub struct SdiOutput {
    input: SdiInput,
    stats: SdiStats,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SdiOutput {
    /// Starts the output clock.
    pub fn spawn(mode: VideoMode, taps: Taps) -> std::io::Result<Self> {
        Self::spawn_inner(mode, None, taps)
    }

    /// A new stream ID is required whenever this clock is restarted.
    pub fn spawn_program(mode: VideoMode, stream: w::Id, taps: Taps) -> std::io::Result<Self> {
        Self::spawn_inner(mode, Some(stream), taps)
    }

    fn spawn_inner(mode: VideoMode, stream: Option<w::Id>, taps: Taps) -> std::io::Result<Self> {
        let program_enabled = stream.is_some();
        let counters = Arc::new(Counters::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (frames_tx, frames_rx) = mpsc::sync_channel(OUTPUT_QUEUE);
        let thread = {
            let (counters, stop) = (Arc::clone(&counters), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("chr-sdi-out".into())
                .spawn(move || {
                    run_output(mode, stream.as_ref(), &frames_rx, taps, &counters, &stop)
                })?
        };
        Ok(Self {
            input: SdiInput {
                frames: frames_tx,
                counters: Arc::clone(&counters),
                frame_bytes: mode.width as usize * mode.height as usize * 2,
                program_enabled,
            },
            stats: SdiStats::new(mode.into(), counters),
            stop,
            thread: Some(thread),
        })
    }

    pub fn input(&self) -> SdiInput {
        self.input.clone()
    }

    pub fn stats(&self) -> SdiStats {
        self.stats.clone()
    }
}

impl Drop for SdiOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_output(
    mode: VideoMode,
    stream: Option<&w::Id>,
    frames: &Receiver<SdiFrame>,
    mut taps: Taps,
    counters: &Counters,
    stop: &AtomicBool,
) {
    // The port starts once program video exists.
    let mut current = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match frames.recv_timeout(POLL) {
            Ok(frame) => break frame,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        }
    };
    let period = mode.frame_duration();
    // Two slots of preroll let the queue absorb jitter between the two clocks.
    // Slot deadlines count from `epoch`, so the rational cadence never drifts.
    let (mut epoch, mut epoch_slot) = (Instant::now() + period * 2, 0u64);
    let mut deadline = epoch;
    let mut slot = 0u64;
    while !stop.load(Ordering::Relaxed) {
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if slot > 0 {
            match frames.try_recv() {
                Ok(frame) => current = frame,
                Err(TryRecvError::Empty) => {
                    counters.repeated.fetch_add(1, Ordering::Relaxed);
                }
                Err(TryRecvError::Disconnected) => return,
            }
        }
        counters.frames_output.fetch_add(1, Ordering::Relaxed);
        counters
            .last_source_frame
            .store(current.source_frame, Ordering::Relaxed);
        counters.has_source_frame.store(true, Ordering::Release);
        let association = current.program.as_ref().zip(stream).map(|(frame, stream)| {
            let _ = frame.observer.transmitted(stream.clone(), slot.into());
            ProgramAssociation {
                provenance: Arc::clone(&frame.provenance),
                output_frame_id: frame.resource.id().clone(),
                output_stream_id: stream.clone(),
            }
        });
        if let Some(ring) = &mut taps.ring {
            let meta = mode.tap_meta(current.source_frame, current.program.is_some());
            let header = chr_shm::SlotHeader {
                index: slot,
                meta: meta.to_meta(),
            };
            // The ring never waits for its readers; it can only fail on a frame size
            // it was not created for, and then the port continues without it.
            if let Err(error) = ring.push(header, &current.uyvy) {
                log::error!("program tap disabled: {error}");
                taps.ring = None;
            }
        }
        if let Some(observer) = &taps.observer {
            // A full or closed observer only misses slots; the port keeps ticking.
            let _ = observer.try_send(SdiSlot {
                slot,
                uyvy: Arc::clone(&current.uyvy),
                program: current.program.clone(),
                association,
            });
        }
        let Some(next) = slot.checked_add(1) else {
            return;
        };
        slot = next;
        deadline = epoch + mode.fps.frame_time(slot - epoch_slot);
        let now = Instant::now();
        if now > deadline + period {
            // A stalled process cannot send late slots in a burst; real hardware would
            // have kept ticking, so the missed slots are simply gone.
            log::debug!("SDI output stalled for {:?}", now - deadline);
            (epoch, epoch_slot, deadline) = (now, slot, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn program_handoff_is_bounded_and_never_invents_transmission() {
        use crate::{
            program::synthetic::{Synthetic, runtime_id},
            retained::SceneControl,
        };
        use serde_json::json;
        let mode = VideoMode {
            width: 16,
            height: 16,
            fps: Rate::fps(50),
        };
        let (mut synthetic, adapter) = Synthetic::install(mode, runtime_id().unwrap()).unwrap();
        let (control, owner) = SceneControl::spawn(adapter, 8).unwrap();
        synthetic.create_scene(&control).await.unwrap();
        let (frames, receiver) = mpsc::sync_channel(1);
        let mut port = SdiInput {
            frames,
            counters: Arc::default(),
            frame_bytes: 512,
            program_enabled: true,
        };
        let first = synthetic.inject(&control).await.unwrap().unwrap();
        synthetic
            .scheduler
            .next_ready()
            .unwrap()
            .unwrap()
            .rendered(Arc::new(vec![16u8; 512]))
            .unwrap();
        synthetic.scheduler.handoff(&mut port).unwrap();
        let second = synthetic.inject(&control).await.unwrap().unwrap();
        synthetic
            .scheduler
            .next_ready()
            .unwrap()
            .unwrap()
            .rendered(Arc::new(vec![17u8; 512]))
            .unwrap();
        synthetic.scheduler.handoff(&mut port).unwrap();
        let get = |submission| json!({"runtime_id":synthetic.configuration.runtime_id,"submission_id":submission});
        let first_result = control
            .call(w::RENDER_GET, Some(get(&first)))
            .await
            .unwrap();
        assert_eq!(first_result["frames"][0]["delivery"]["state"], "queued");
        let second_result = control
            .call(w::RENDER_GET, Some(get(&second)))
            .await
            .unwrap();
        assert_eq!(
            second_result["frames"][0]["delivery"]["reason"],
            "queue_full"
        );
        assert_eq!(port.counters.dropped.load(Ordering::Relaxed), 1);
        let frame = receiver.try_recv().unwrap();
        let weak = Arc::downgrade(&frame.uyvy);
        drop(frame);
        assert!(weak.upgrade().is_none());
        let first_result = control
            .call(w::RENDER_GET, Some(get(&first)))
            .await
            .unwrap();
        assert_eq!(
            first_result["frames"][0]["delivery"]["reason"],
            "output_failed"
        );
        // Native shutdown drains even if live transports retain endpoint clones.
        let live_connection = control.clone();
        control.shutdown().await;
        owner.await.unwrap();
        assert!(
            live_connection
                .call(
                    w::GET_CONFIGURATION,
                    Some(json!({"runtime_id":synthetic.configuration.runtime_id}))
                )
                .await
                .is_err()
        );
    }

    fn uyvy_frame(mode: VideoMode, luma: u8) -> Arc<Vec<u8>> {
        let pixels = (mode.width * mode.height) as usize;
        Arc::new([128, luma, 128, luma].repeat(pixels / 2))
    }

    #[test]
    fn output_repeats_on_underrun_and_drops_when_full() {
        let mode = VideoMode {
            width: 64,
            height: 32,
            fps: Rate::fps(100),
        };
        let (taps, observer) = Taps::observed();
        let output = SdiOutput::spawn(mode, taps).unwrap();
        let input = output.input();
        let stats = output.stats();
        assert_eq!(stats.status().last_source_frame, None);

        // More frames than the queue holds, faster than the output clock.
        for source_frame in 0..10 {
            input.submit(SdiFrame {
                source_frame,
                uyvy: uyvy_frame(mode, 100),
                program: None,
            });
        }
        // Then nothing: the port must keep ticking by repeating the last frame.
        std::thread::sleep(Duration::from_millis(300));
        let status = stats.status();
        assert_eq!(status.video_mode, "64x32p100");
        // The queue holds OUTPUT_QUEUE frames, plus one if the port already took the first.
        let kept = 10 - status.dropped_frames;
        assert!(
            (OUTPUT_QUEUE as u64..=OUTPUT_QUEUE as u64 + 1).contains(&kept),
            "{status:?}"
        );
        assert!(status.last_source_frame.is_some_and(|frame| frame < 10));
        assert!(status.frames_output > 10, "{status:?}");
        assert!(status.repeated_frames > 5, "{status:?}");

        // Nobody drained the observer: it kept the first slots, in order, and the
        // port skipped the rest for it but kept going.
        let first = observer.try_recv().expect("a transmitted slot");
        assert_eq!(first.slot, 0);
        let second = observer.try_recv().expect("a second slot");
        assert_eq!(second.slot, 1);
    }

    #[test]
    fn program_tap_receives_every_slot_without_a_reader() {
        let mode = VideoMode {
            width: 16,
            height: 16,
            fps: Rate::fps(100),
        };
        let dir = std::env::temp_dir().join(format!("chr-sdi-tap-{}", std::process::id()));
        let path = dir.join("program.tap");
        let output = SdiOutput::spawn(mode, Taps::ring(mode, &path).unwrap()).unwrap();
        let stats = output.stats();
        let frame = uyvy_frame(mode, 16);
        output.input().submit(SdiFrame {
            source_frame: 3,
            uyvy: Arc::clone(&frame),
            program: None,
        });
        std::thread::sleep(Duration::from_millis(100));
        let tap = chr_shm::Reader::open(&path).unwrap();
        drop(output);

        let written = tap.write_count();
        assert!(written > 5, "{written}");
        assert_eq!(written, stats.status().frames_output);
        let mut payload = Vec::new();
        let chr_shm::Read::Frame(header) =
            tap.read(tap.latest_slot().unwrap(), &mut payload).unwrap()
        else {
            panic!("the newest tap slot is not readable");
        };
        assert_eq!(
            header.index,
            written - 1,
            "repeats are written as their own slots"
        );
        assert_eq!(
            chr_shm::tap::SlotMeta::from_meta(header.meta),
            mode.tap_meta(3, false)
        );
        assert_eq!(payload, *frame);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
