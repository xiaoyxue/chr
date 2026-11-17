//! Reads a program tap as chr-monitor would and reports its cadence and frame
//! sequence: the same measurement for chr-server's timer-paced loop and its
//! GStreamer core (#154).
//!
//! `cargo run --release -p chr-shm --example tap_stats -- <tap> <seconds> <fps>`
//!
//! It polls the tap every 200 µs, so intervals include up to that much of
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
    let [path, seconds, fps] = args.as_slice() else {
        return Err("usage: tap_stats <tap> <seconds> <fps>".into());
    };
    let period = 1e3 / fps.parse::<f64>()?;
    let tap = Reader::open(path.as_ref())?;
    let end = Instant::now() + Duration::from_secs(seconds.parse()?);
    let (mut seen, mut payload) = (tap.write_count(), Vec::new());
    let (mut last_at, mut last_frame): (Option<Instant>, Option<u64>) = (None, None);
    let (mut slots, mut missed, mut repeats, mut skipped, mut program) = (0_u64, 0, 0, 0, 0);
    let (mut intervals, mut irregular) = (Vec::new(), 0_u64);
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
        slots += 1;
        program += u64::from(meta.program);
        if last_program.is_some_and(|last| last != meta.program) {
            changes.push((at.duration_since(start).as_secs_f64(), slots, meta.program));
        }
        last_program = Some(meta.program);
        if let Some(last) = last_at {
            let ms = at.duration_since(last).as_secs_f64() * 1e3;
            intervals.push(ms);
            irregular += u64::from((ms - period).abs() > period / 4.);
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
    let n = intervals.len().max(1) as f64;
    let mean = intervals.iter().sum::<f64>() / n;
    let sd = (intervals.iter().map(|ms| (ms - mean).powi(2)).sum::<f64>() / n).sqrt();
    let (min, max) = intervals.iter().fold((f64::MAX, f64::MIN), |(lo, hi), ms| {
        (lo.min(*ms), hi.max(*ms))
    });
    println!(
        "slots {slots} (missed by the reader {missed}), program (CG) {program}, \
         repeats {repeats}, frames skipped {skipped}"
    );
    println!(
        "interval min/mean/max {min:.3}/{mean:.3}/{max:.3} ms, sd {sd:.3} ms, \
         {irregular} off by more than a quarter period"
    );
    println!("slot - frame: {deltas:?}");
    for pair in changes.windows(2).filter(|pair| !pair[0].2) {
        let ((at, from, _), (_, to, _)) = (pair[0], pair[1]);
        println!("t={at:.2}s: {} slots without CG", to - from);
    }
    if let Some((at, from, false)) = changes.last() {
        println!("t={at:.2}s: {} slots without CG to the end", slots - from);
    }
    Ok(())
}
