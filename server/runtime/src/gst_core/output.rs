//! The program's consumers after `livesync`: the simulated OUTPUT, an `appsink`
//! that takes every slot on the clock as an SDI card would (phase 3 puts
//! `decklinkvideosink` here), with cadence and delay statistics; and the
//! shared-memory program tap that chr-monitor reads, so the monitor works
//! unchanged. Neither can delay the other: the tap's queue drops.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSinkCallbacks};

use super::{PROGRAM_META, Timeline};
use crate::sdi::{SdiRecorder, VideoMode};

/// One program slot as the output took it.
#[derive(Clone, Copy, Debug, PartialEq)]
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

    /// `min/mean/max ms (sd)`, or `-` without values.
    pub fn ms(&self) -> String {
        if self.count == 0 {
            return "-".to_owned();
        }
        format!(
            "{:.3}/{:.3}/{:.3} ms (sd {:.3})",
            self.min as f64 / 1e6,
            self.mean() / 1e6,
            self.max as f64 / 1e6,
            self.std_dev() / 1e6
        )
    }
}

/// Output cadence and capture-to-output delay over a series of slots.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    pub period_ns: u64,
    pub slots: u64,
    pub cg: u64,
    pub clean: u64,
    pub repeats: u64,
    /// Captured frames that never reached the output between two that did.
    pub skipped: u64,
    /// Time between consecutive slots.
    pub interval: Summary,
    /// Slots more than a quarter period off the period.
    pub irregular: u64,
    /// Output time minus due time.
    pub lateness: Summary,
    /// Output time minus capture time, of slots that are not repeats.
    pub delay: Summary,
    last_at: Option<u64>,
    last_sequence: Option<u64>,
}

impl Totals {
    pub fn new(period_ns: u64) -> Self {
        Self {
            period_ns,
            ..Self::default()
        }
    }

    pub fn record(&mut self, slot: &Rendered) {
        self.slots += 1;
        if let Some(last) = self.last_at {
            let interval = slot.at_ns as i64 - last as i64;
            self.interval.add(interval);
            if (interval - self.period_ns as i64).unsigned_abs() > self.period_ns / 4 {
                self.irregular += 1;
            }
        }
        self.last_at = Some(slot.at_ns);
        self.lateness.add(slot.at_ns as i64 - slot.due_ns as i64);
        if slot.repeat {
            self.repeats += 1;
            return;
        }
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
        format!(
            "output: {} slots ({} CG, {} clean feed, {} repeats, {} frames skipped), interval \
             min/mean/max {}, {} irregular, delay {}, lateness {}",
            self.slots,
            self.cg,
            self.clean,
            self.repeats,
            self.skipped,
            self.interval.ms(),
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
    mode: VideoMode,
    timeline: Timeline,
    window: Mutex<Totals>,
    totals: Mutex<Totals>,
    tap: Mutex<Option<Tap>>,
    recorder: SdiRecorder,
}

impl Output {
    pub fn new(
        mode: VideoMode,
        timeline: Timeline,
        tap: Option<chr_shm::Writer>,
        recorder: SdiRecorder,
    ) -> Self {
        let period = mode.frame_duration().as_nanos() as u64;
        Self {
            mode,
            timeline,
            window: Mutex::new(Totals::new(period)),
            totals: Mutex::new(Totals::new(period)),
            tap: Mutex::new(tap.map(|ring| Tap {
                ring,
                slot: 0,
                delta: None,
                delta_changes: 0,
            })),
            recorder,
        }
    }

    fn rendered(&self, buffer: &gst::BufferRef) {
        let at = self.timeline.clock.time();
        let Some(slot) = Rendered::of(buffer, at, &self.timeline) else {
            log::warn!("an output slot without its program meta");
            return;
        };
        lock(&self.window).record(&slot);
        lock(&self.totals).record(&slot);
        self.recorder.output(slot.sequence, slot.repeat);
    }

    fn tapped(&self, buffer: &gst::BufferRef) {
        let mut tap = lock(&self.tap);
        let Some(writer) = tap.as_mut() else {
            return;
        };
        let Some(slot) = Rendered::of(buffer, gst::ClockTime::ZERO, &self.timeline) else {
            return;
        };
        let meta = chr_shm::tap::SlotMeta {
            source_frame: slot.sequence,
            program: slot.cg,
            width: self.mode.width,
            height: self.mode.height,
            fps: self.mode.fps,
        };
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
    pub fn report(&self) -> String {
        let period = self.mode.frame_duration().as_nanos() as u64;
        let window = std::mem::replace(&mut *lock(&self.window), Totals::new(period));
        let tap = lock(&self.tap);
        match tap.as_ref() {
            Some(tap) => format!(
                "{}; tap slot - frame {:?} ({} changes)",
                window.report(),
                tap.delta,
                tap.delta_changes
            ),
            None => window.report(),
        }
    }

    pub fn totals(&self) -> Totals {
        *lock(&self.totals)
    }
}

/// Feeds the output and tap sinks' slots to `output`.
pub fn attach(output: &Arc<Output>, output_sink: &AppSink, tap_sink: &AppSink) {
    let rendering = Arc::clone(output);
    output_sink.set_callbacks(
        AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                if let Some(buffer) = sample.buffer() {
                    rendering.rendered(buffer);
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    let tapping = Arc::clone(output);
    tap_sink.set_callbacks(
        AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                if let Some(buffer) = sample.buffer() {
                    tapping.tapped(buffer);
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
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
        }
    }

    #[test]
    fn totals_count_cadence_repeats_and_skips() {
        let mut totals = Totals::new(20_000_000);
        totals.record(&slot(1000, 0, true, false));
        totals.record(&slot(1020, 1, false, false));
        totals.record(&slot(1040, 1, false, true));
        totals.record(&slot(1070, 3, true, false));
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
        assert_eq!(
            (totals.interval.min, totals.interval.max),
            (20_000_000, 30_000_000)
        );
        assert_eq!(totals.irregular, 1);
        assert_eq!(totals.delay.count, 3, "repeats have no delay of their own");
        assert_eq!(
            (totals.delay.min, totals.delay.max),
            (100_000_000, 100_000_000)
        );
    }
}
