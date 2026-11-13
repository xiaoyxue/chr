//! Copy cost of one frame through a ring, for the live-process measurements.
//! Run with `cargo test --release -p chr-shm --test copy_cost -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use chr_shm::{Layout, Read, Reader, SlotHeader, Writer};

fn measure(name: &str, bytes: usize) {
    let dir = std::env::temp_dir().join(format!("chr-shm-cost-{}-{name}", std::process::id()));
    let path = dir.join("ring");
    let layout = Layout {
        slot_count: 4,
        slot_bytes: bytes as u64,
    };
    let mut writer = Writer::create(&path, layout).unwrap();
    let reader = Reader::open(&path).unwrap();
    let frame = vec![0x5a; bytes];
    let mut payload = Vec::with_capacity(bytes);
    let (mut writes, mut reads) = (Vec::new(), Vec::new());
    for index in 0..200 {
        let start = Instant::now();
        let slot = writer.push(SlotHeader { index, meta: [0; 4] }, &frame).unwrap();
        writes.push(start.elapsed());
        let start = Instant::now();
        assert!(matches!(reader.read(slot, &mut payload).unwrap(), Read::Frame(_)));
        reads.push(start.elapsed());
    }
    let median = |times: &mut Vec<Duration>| {
        times.sort();
        times[times.len() / 2]
    };
    println!(
        "{name}: {:.1} MB, write {:?}, read {:?} (median of 200)",
        bytes as f64 / 1e6,
        median(&mut writes),
        median(&mut reads)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "measurement; run in release"]
fn ring_copy_cost_per_frame() {
    measure("1920x1080 RGBA8 input", 1920 * 1080 * 4);
    measure("1280x720 RGBA8 input", 1280 * 720 * 4);
    measure("1920x1080 UYVY output", 1920 * 1080 * 2);
    measure("1280x720 UYVY output", 1280 * 720 * 2);
}
