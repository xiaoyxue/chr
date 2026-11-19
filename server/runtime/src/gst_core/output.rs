//! The program's consumers after `livesync`: the OUTPUT, either simulated (an
//! `appsink` that takes every slot on the clock as an SDI card would) or a
//! DeckLink card's `decklinkvideosink` (whose slots are counted as they are
//! handed to it, ahead of air), with cadence, delay, input-loss and timecode
//! statistics; and the shared-memory program tap that chr-monitor reads. Both take each slot's
//! rate from its caps, which change with the input's rate when the output
//! follows it; the tap's slots carry it to chr-monitor, which follows it.
//! Neither can delay the other: the tap's queue drops.

use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSinkCallbacks};

use chr_common::rate::Rate;

use super::{PROGRAM_META, Timeline, capture::format_of};
use crate::sdi::{SdiRecorder, VideoMode};

/// One program slot as the output took it.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    /// Clock time the output sink took the slot.
    pub at_ns: u64,
    /// Clock time the slot was due.
    pub due_ns: u64,
    /// The frame's capture time.
    pub source_ns: u64,
    pub sequence: u64,
    pub cg: bool,
    /// A repeat of the previous slot's frame (`livesync` filled a gap).
    pub repeat: bool,
    /// Filled for a lost input (hold, black or slate).
    pub fill: bool,
    /// The frame's timecode and its frame count since midnight.
    pub timecode: Option<(String, u64)>,
    pub captions: bool,
}

impl Rendered {
    fn of(buffer: &gst::BufferRef, at: gst::ClockTime, timeline: &Timeline) -> Option<Self> {
        let meta = gst::meta::CustomMeta::from_buffer(buffer, PROGRAM_META).ok()?;
        let structure = meta.structure();
        Some(Self {
            at_ns: at.nseconds(),
            due_ns: timeline.absolute(buffer.pts()?).nseconds(),
            source_ns: structure.get("source-ns").ok()?,
            sequence: structure.get("sequence").ok()?,
            cg: structure.get("cg").ok()?,
            repeat: buffer.flags().contains(gst::BufferFlags::GAP),
            fill: structure.get("fill").unwrap_or(false),
            timecode: structure
                .get::<String>("timecode")
                .ok()
                .zip(structure.get::<u64>("timecode-frames").ok()),
            captions: structure.get("captions").unwrap_or(false),
        })
    }
}

/// Count, mean, spread and extremes of a series of nanosecond values.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Summary {
    pub count: u64,
    sum: f64,
    sum_sq: f64,
    pub min: i64,
    pub max: i64,
}

impl Summary {
    pub fn add(&mut self, value: i64) {
        if self.count == 0 {
            (self.min, self.max) = (value, value);
        }
        self.count += 1;
        self.sum += value as f64;
        self.sum_sq += (value as f64).powi(2);
        self.min = self.min.min(value);
        self.max = self.max.max(value);
    }

    pub fn mean(&self) -> f64 {
        self.sum / self.count.max(1) as f64
    }

    pub fn std_dev(&self) -> f64 {
        (self.sum_sq / self.count.max(1) as f64 - self.mean().powi(2))
            .max(0.0)
            .sqrt()
    }

    /// `min/mean/max ms (sd)`, or `-` without values; the mean to 0.1 µs, to
    /// show a cadence's exact average.
    pub fn ms(&self) -> String {
        if self.count == 0 {
            return "-".to_owned();
        }
        format!(
            "{:.3}/{:.4}/{:.3} ms (sd {:.3})",
            self.min as f64 / 1e6,
            self.mean() / 1e6,
            self.max as f64 / 1e6,
            self.std_dev() / 1e6
        )
    }
}

/// Output cadence and capture-to-output delay over a series of slots.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub slots: u64,
    pub cg: u64,
    pub clean: u64,
    /// Slots `livesync` filled with the previous frame: no frame came.
    pub repeats: u64,
    /// Slots showing the previous slot's frame again by design: the fixed
    /// output rate is above the input's.
    pub duplicates: u64,
    /// Captured frames that never reached the output between two that did.
    pub skipped: u64,
    /// Slots filled for a lost input.
    pub fills: u64,
    /// Consecutive frames whose timecodes are not one frame apart.
    pub timecode_breaks: u64,
    pub captioned: u64,
    /// Time between consecutive slots at each rate, the current one last; the
    /// interval across a change of rate is in none.
    pub intervals: Vec<(Rate, Summary)>,
    /// Slots more than a quarter period off their rate's period.
    pub irregular: u64,
    /// Output time minus due time.
    pub lateness: Summary,
    /// Output time minus capture time, of slots that are not repeats.
    pub delay: Summary,
    last_at: Option<u64>,
    last_sequence: Option<u64>,
    last_timecode: Option<u64>,
}

impl Totals {
    /// The intervals at `rate`.
    pub fn interval(&self, rate: Rate) -> Option<&Summary> {
        self.intervals
            .iter()
            .find(|(at, _)| *at == rate)
            .map(|(_, summary)| summary)
    }

    /// One slot of the output at `rate`.
    pub fn record(&mut self, slot: &Rendered, rate: Rate) {
        self.slots += 1;
        if self.intervals.last().is_none_or(|(last, _)| *last != rate) {
            self.last_at = None;
            if self.interval(rate).is_none() {
                self.intervals.push((rate, Summary::default()));
            }
        }
        if let Some(last) = self.last_at {
            let interval = slot.at_ns as i64 - last as i64;
            let period = rate.frame_ns(1) as i64;
            let (_, summary) = self
                .intervals
                .iter_mut()
                .find(|(at, _)| *at == rate)
                .expect("added above");
            summary.add(interval);
            if (interval - period).unsigned_abs() > period.unsigned_abs() / 4 {
                self.irregular += 1;
            }
        }
        // Keep the rate's place last, so the next slot at it counts on.
        if let Some(index) = self.intervals.iter().position(|(at, _)| *at == rate) {
            let entry = self.intervals.remove(index);
            self.intervals.push(entry);
        }
        self.last_at = Some(slot.at_ns);
        self.lateness.add(slot.at_ns as i64 - slot.due_ns as i64);
        if slot.repeat {
            self.repeats += 1;
            return;
        }
        if slot.fill {
            self.fills += 1;
            return;
        }
        if self.last_sequence == Some(slot.sequence) {
            self.duplicates += 1;
            return;
        }
        let timecode = slot.timecode.as_ref().map(|(_, frames)| *frames);
        if let (Some(last), Some(sequence), Some(now), Some(before)) = (
            self.last_sequence,
            Some(slot.sequence),
            timecode,
            self.last_timecode,
        ) && sequence == last + 1
            && now != before + 1
        {
            self.timecode_breaks += 1;
        }
        self.last_timecode = timecode;
        self.captioned += u64::from(slot.captions);
        if slot.cg {
            self.cg += 1;
        } else {
            self.clean += 1;
        }
        self.delay.add(slot.at_ns as i64 - slot.source_ns as i64);
        if let Some(last) = self.last_sequence
            && slot.sequence > last + 1
        {
            self.skipped += slot.sequence - last - 1;
        }
        self.last_sequence = Some(slot.sequence);
    }

    pub fn report(&self) -> String {
        let intervals: Vec<String> = self
            .intervals
            .iter()
            .map(|(rate, summary)| format!("at {} fps {}", rate.label(), summary.ms()))
            .collect();
        format!(
            "output: {} slots ({} CG, {} clean feed, {} repeats, {} duplicates, {} input-loss \
             fills, {} frames skipped; {} timecode breaks, {} captioned), interval \
             min/mean/max {}, {} irregular, delay {}, lateness {}",
            self.slots,
            self.cg,
            self.clean,
            self.repeats,
            self.duplicates,
            self.fills,
            self.skipped,
            self.timecode_breaks,
            self.captioned,
            if intervals.is_empty() {
                "-".to_owned()
            } else {
                intervals.join(", ")
            },
            self.irregular,
            self.delay.ms(),
            self.lateness.ms()
        )
    }
}

/// The program tap writer: one ring slot per output slot.
struct Tap {
    ring: chr_shm::Writer,
    slot: u64,
    /// Output slot minus frame sequence, the program delay in slots, and how
    /// often it changed.
    delta: Option<i64>,
    delta_changes: u64,
}

pub struct Output {
    /// The output's extent; each slot's rate comes from its caps.
    mode: VideoMode,
    timeline: Timeline,
    window: Mutex<Totals>,
    totals: Mutex<Totals>,
    tap: Mutex<Option<Tap>>,
    recorder: SdiRecorder,
    /// How long before air a card gets its slots ([`super::CARD_LEAD`]); zero
    /// for the simulated output, which takes them on air.
    lead: Duration,
}

impl Output {
    pub fn new(
        mode: VideoMode,
        timeline: Timeline,
        tap: Option<chr_shm::Writer>,
        recorder: SdiRecorder,
        lead: Duration,
    ) -> Self {
        Self {
            mode,
            timeline,
            window: Mutex::default(),
            totals: Mutex::default(),
            tap: Mutex::new(tap.map(|ring| Tap {
                ring,
                slot: 0,
                delta: None,
                delta_changes: 0,
            })),
            recorder,
            lead,
        }
    }

    /// The rate of a sample's caps, else the output's at start.
    fn rate(&self, sample: &gst::Sample) -> Rate {
        sample
            .caps()
            .and_then(|caps| format_of(caps).ok())
            .map_or(self.mode.fps, |format| format.mode.fps)
    }

    fn rendered(&self, sample: &gst::Sample) {
        let at = self.timeline.clock.time();
        let Some(slot) = sample
            .buffer()
            .and_then(|buffer| Rendered::of(buffer, at, &self.timeline))
        else {
            log::warn!("an output slot without its program meta");
            return;
        };
        let rate = self.rate(sample);
        self.record(slot, rate);
    }

    fn record(&self, slot: Rendered, rate: Rate) {
        lock(&self.window).record(&slot, rate);
        lock(&self.totals).record(&slot, rate);
        self.recorder.output(slot.sequence, slot.repeat);
        self.recorder
            .ancillary(slot.timecode.map(|(text, _)| text), slot.captions);
    }

    fn tapped(&self, sample: &gst::Sample) {
        let mut tap = lock(&self.tap);
        let Some(writer) = tap.as_mut() else {
            return;
        };
        let Some(buffer) = sample.buffer() else {
            return;
        };
        let Some(slot) = Rendered::of(buffer, gst::ClockTime::ZERO, &self.timeline) else {
            return;
        };
        let meta = VideoMode {
            fps: self.rate(sample),
            ..self.mode
        }
        .tap_meta(slot.sequence, slot.cg);
        let header = chr_shm::SlotHeader {
            index: writer.slot,
            meta: meta.to_meta(),
        };
        let written = buffer
            .map_readable()
            .map_err(|error| error.to_string())
            .and_then(|map| {
                writer
                    .ring
                    .push(header, &map)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = written {
            log::error!("program tap disabled: {error}");
            *tap = None;
            return;
        }
        let delta = writer.slot as i64 - slot.sequence as i64;
        if writer.delta.is_some_and(|last| last != delta) {
            writer.delta_changes += 1;
        }
        writer.delta = Some(delta);
        writer.slot += 1;
    }

    /// The output since the last report, and the tap's slot delay.
    pub fn report(&self) -> (String, Totals) {
        let window = std::mem::take(&mut *lock(&self.window));
        let tap = lock(&self.tap);
        let text = match tap.as_ref() {
            Some(tap) => format!(
                "{}; tap slot - frame {:?} ({} changes)",
                window.report(),
                tap.delta,
                tap.delta_changes
            ),
            None => window.report(),
        };
        (text, window)
    }

    pub fn totals(&self) -> Totals {
        lock(&self.totals).clone()
    }
}

/// Feeds the output and tap sinks' slots to `output`: a simulated output's
/// as it renders them on the clock, a card's as they are handed to it.
pub fn attach(
    output: &Arc<Output>,
    output_sink: &gst::Element,
    tap_sink: &AppSink,
) -> Result<(), super::Error> {
    let rendering = Arc::clone(output);
    match output_sink.downcast_ref::<AppSink>() {
        Some(simulated) => simulated.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                    rendering.rendered(&sample);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        ),
        None => {
            output_sink
                .static_pad("sink")
                .ok_or("the output sink has no sink pad")?
                .add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
                    if let Some(buffer) = info.buffer() {
                        let at = rendering.timeline.clock.time();
                        // Its slots go on air the lead after they are handed over.
                        let lead = rendering.lead.as_nanos() as u64;
                        let at = at + gst::ClockTime::from_nseconds(lead);
                        if let Some(mut slot) = Rendered::of(buffer, at, &rendering.timeline) {
                            slot.due_ns += lead;
                            let rate = pad
                                .current_caps()
                                .and_then(|caps| format_of(&caps).ok())
                                .map_or(rendering.mode.fps, |format| format.mode.fps);
                            rendering.record(slot, rate);
                        }
                    }
                    gst::PadProbeReturn::Ok
                });
        }
    }
    let tapping = Arc::clone(output);
    tap_sink.set_callbacks(
        AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                tapping.tapped(&sample);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(at_ms: u64, sequence: u64, cg: bool, repeat: bool) -> Rendered {
        Rendered {
            at_ns: at_ms * 1_000_000,
            due_ns: at_ms * 1_000_000,
            source_ns: (at_ms - 100) * 1_000_000,
            sequence,
            cg,
            repeat,
            fill: false,
            timecode: Some((String::new(), 1000 + sequence)),
            captions: false,
        }
    }

    #[test]
    fn totals_count_cadence_repeats_and_skips() {
        let fifty = Rate::fps(50);
        let mut totals = Totals::default();
        totals.record(&slot(1000, 0, true, false), fifty);
        totals.record(&slot(1020, 1, false, false), fifty);
        totals.record(&slot(1040, 1, false, true), fifty);
        totals.record(&slot(1070, 3, true, false), fifty);
        assert_eq!(
            (
                totals.slots,
                totals.cg,
                totals.clean,
                totals.repeats,
                totals.skipped
            ),
            (4, 2, 1, 1, 1)
        );
        let interval = totals.interval(fifty).unwrap();
        assert_eq!((interval.min, interval.max), (20_000_000, 30_000_000));
        assert_eq!(totals.irregular, 1);
        assert_eq!(totals.delay.count, 3, "repeats have no delay of their own");
        assert_eq!(
            (totals.delay.min, totals.delay.max),
            (100_000_000, 100_000_000)
        );
    }

    #[test]
    fn totals_keep_intervals_per_rate_and_count_duplicates() {
        let (fifty, quarter) = (Rate::fps(50), Rate::fps(25));
        let mut totals = Totals::default();
        totals.record(&slot(1000, 0, true, false), quarter);
        totals.record(&slot(1040, 1, true, false), quarter);
        // The input switched to 50 fps: the first interval at it is in neither.
        totals.record(&slot(1080, 2, true, false), fifty);
        totals.record(&slot(1100, 3, true, false), fifty);
        // A fixed output above the input's rate shows a frame twice.
        totals.record(&slot(1120, 3, true, false), fifty);
        assert_eq!(totals.interval(quarter).unwrap().count, 1);
        assert_eq!(totals.interval(fifty).unwrap().count, 2);
        assert_eq!(totals.interval(fifty).unwrap().mean(), 20_000_000.0);
        assert_eq!((totals.irregular, totals.duplicates, totals.cg), (0, 1, 4));
    }

    #[test]
    fn totals_count_input_loss_fills_and_timecode_breaks() {
        let fifty = Rate::fps(50);
        let mut totals = Totals::default();
        totals.record(&slot(1000, 0, true, false), fifty);
        // The input is lost: two fills on the cadence, no delay of their own.
        for at in [1020, 1040] {
            totals.record(
                &Rendered {
                    fill: true,
                    timecode: None,
                    ..slot(at, 0, false, false)
                },
                fifty,
            );
        }
        // Back: frame 1's timecode follows frame 0's; frame 2's jumps.
        totals.record(&slot(1060, 1, true, false), fifty);
        totals.record(
            &Rendered {
                timecode: Some((String::new(), 5000)),
                ..slot(1080, 2, true, false)
            },
            fifty,
        );
        assert_eq!((totals.fills, totals.cg, totals.skipped), (2, 3, 0));
        assert_eq!(totals.timecode_breaks, 1);
        assert_eq!(totals.delay.count, 3);
        assert_eq!(totals.irregular, 0, "fills keep the cadence");
    }
}
