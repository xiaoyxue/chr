//! Reads a program tap as chr-monitor would and reports its cadence and frame
//! sequence: the same measurement for chr-server's GStreamer core (#154) and a
//! generated program's internal clock.
//!
//! `cargo run --release -p chr-shm --example tap_stats -- <tap> <seconds> [<x0> <x1>]`
//!
//! With `<x0> <x1>` it also reports the field parity of every slot: the mean
//! luma of the odd rows minus the even rows in columns `x0..x1`, for CG and
//! other slots apart. On a test clip whose bottom fields show a later, brighter
//! picture than its top fields, an interlaced output that kept its fields in
//! their order reads positive (#154 phase 3).
//!
//! The expected period comes from each slot's rate; a change of rate starts a
//! new segment with its own interval statistics. It polls the tap every 200 µs, so intervals include up to that much of
//! polling jitter. A slot repeats the previous one's frame (`source_frame`) or
//! skips frames; `slot - frame` is the output's delay in slots, which stays
//! constant while the output follows the input frame for frame. Every change
//! between program (CG) and other slots is listed with its time, so a fault
//! shows as one run of clean-feed slots.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use chr_shm::{Read, Reader, tap::SlotMeta};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, seconds, columns @ ..] = args.as_slice() else {
        return Err("usage: tap_stats <tap> <seconds> [<x0> <x1>]".into());
    };
    let columns = match columns {
        [x0, x1] => Some((x0.parse::<usize>()?, x1.parse::<usize>()?)),
        _ => None,
    };
    // Field parity of (CG, other) slots: count, min, sum, max.
    let mut parity = [(0_u64, f64::MAX, 0.0_f64, f64::MIN); 2];
    let tap = Reader::open(path.as_ref())?;
    let end = Instant::now() + Duration::from_secs(seconds.parse()?);
    let (mut seen, mut payload) = (tap.write_count(), Vec::new());
    let (mut last_at, mut last_frame): (Option<Instant>, Option<u64>) = (None, None);
    let (mut slots, mut missed, mut repeats, mut skipped, mut program) = (0_u64, 0, 0, 0, 0);
    let mut segments: Vec<Segment> = Vec::new();
    let mut deltas: BTreeMap<i64, u64> = BTreeMap::new();
    let (start, mut changes, mut last_program) = (Instant::now(), Vec::new(), None);
    while Instant::now() < end {
        let count = tap.write_count();
        if count == seen {
            std::thread::sleep(Duration::from_micros(200));
            continue;
        }
        let at = Instant::now();
        missed += count - seen - 1;
        seen = count;
        let slot = tap.latest_slot().ok_or("empty tap")?;
        let Ok(Read::Frame(header)) = tap.read(slot, &mut payload) else {
            continue;
        };
        let meta = SlotMeta::from_meta(header.meta);
        if let Some((x0, x1)) = columns {
            let (width, height) = (meta.width as usize, meta.height as usize);
            let luma = |y: usize| -> f64 {
                (x0..x1.min(width))
                    .map(|x| f64::from(payload[(y * width + x) * 2 + 1]))
                    .sum::<f64>()
            };
            let (mut odd, mut even) = (0.0, 0.0);
            for y in 0..height {
                *if y % 2 == 1 { &mut odd } else { &mut even } += luma(y);
            }
            let n = ((x1.min(width) - x0) * height / 2) as f64;
            let value = (odd - even) / n;
            let entry = &mut parity[usize::from(!meta.program)];
            *entry = (
                entry.0 + 1,
                entry.1.min(value),
                entry.2 + value,
                entry.3.max(value),
            );
        }
        slots += 1;
        program += u64::from(meta.program);
        if last_program.is_some_and(|last| last != meta.program) {
            changes.push((at.duration_since(start).as_secs_f64(), slots, meta.program));
        }
        last_program = Some(meta.program);
        let rate = (meta.rate_num, meta.rate_den);
        if segments.last().is_none_or(|segment| segment.rate != rate) {
            segments.push(Segment {
                rate,
                start: at.duration_since(start).as_secs_f64(),
                slots: 0,
                intervals: Vec::new(),
                irregular: 0,
            });
            // The first interval of a new rate belongs to neither.
            last_at = None;
        }
        let segment = segments.last_mut().expect("pushed above");
        segment.slots += 1;
        if let Some(last) = last_at {
            let ms = at.duration_since(last).as_secs_f64() * 1e3;
            let period = 1e3 * f64::from(rate.1) / f64::from(rate.0);
            segment.intervals.push(ms);
            segment.irregular += u64::from((ms - period).abs() > period / 4.);
        }
        last_at = Some(at);
        match last_frame {
            Some(last) if meta.source_frame == last => repeats += 1,
            Some(last) if meta.source_frame > last + 1 => skipped += meta.source_frame - last - 1,
            _ => {}
        }
        last_frame = Some(meta.source_frame);
        *deltas
            .entry(header.index as i64 - meta.source_frame as i64)
            .or_default() += 1;
    }
    println!(
        "slots {slots} (missed by the reader {missed}), program (CG) {program}, \
         repeats {repeats}, frames skipped {skipped}"
    );
    for segment in &segments {
        let intervals = &segment.intervals;
        let n = intervals.len().max(1) as f64;
        let mean = intervals.iter().sum::<f64>() / n;
        let sd = (intervals.iter().map(|ms| (ms - mean).powi(2)).sum::<f64>() / n).sqrt();
        let (min, max) = intervals.iter().fold((f64::MAX, f64::MIN), |(lo, hi), ms| {
            (lo.min(*ms), hi.max(*ms))
        });
        let (num, den) = segment.rate;
        println!(
            "from t={:.2}s at {num}/{den} fps ({:.6} ms): {} slots, interval \
             min/mean/max {min:.3}/{mean:.6}/{max:.3} ms, sd {sd:.3} ms, {} off by more than a \
             quarter period",
            segment.start,
            1e3 * f64::from(den) / f64::from(num),
            segment.slots,
            segment.irregular
        );
    }
    println!("slot - frame: {deltas:?}");
    if columns.is_some() {
        for (kind, (count, min, sum, max)) in ["CG", "other"].iter().zip(parity) {
            if count > 0 {
                println!(
                    "field parity (odd - even rows) of {kind} slots: {count} slots, \
                     min/mean/max {min:.2}/{:.2}/{max:.2}",
                    sum / count as f64
                );
            }
        }
    }
    for pair in changes.windows(2).filter(|pair| !pair[0].2) {
        let ((at, from, _), (_, to, _)) = (pair[0], pair[1]);
        println!("t={at:.2}s: {} slots without CG", to - from);
    }
    if let Some((at, from, false)) = changes.last() {
        println!("t={at:.2}s: {} slots without CG to the end", slots - from);
    }
    Ok(())
}

/// Slots at one rate.
struct Segment {
    rate: (u32, u32),
    /// Seconds after the start of the measurement.
    start: f64,
    slots: u64,
    intervals: Vec<f64>,
    irregular: u64,
}
