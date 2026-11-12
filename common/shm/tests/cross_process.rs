//! A ring written by one process and read by another.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use chr_shm::{Layout, Read, Reader, SlotHeader, Writer};

const RING_ENV: &str = "CHR_SHM_TEST_RING";
const FRAMES: u64 = 2_000;
const LAYOUT: Layout = Layout {
    slot_count: 8,
    slot_bytes: 256 * 1024,
};

fn payload_len(index: u64) -> usize {
    1 + (index as usize * 7919) % LAYOUT.slot_bytes as usize
}

/// The writer half; does nothing unless started by `reader_sees_frames_from_another_process`.
#[test]
fn child_writer() {
    let Some(path) = std::env::var_os(RING_ENV) else {
        return;
    };
    let mut writer = Writer::create(Path::new(&path), LAYOUT).unwrap();
    let mut data = vec![0; LAYOUT.slot_bytes as usize];
    for index in 0..FRAMES {
        let len = payload_len(index);
        data[..len].fill(index as u8);
        writer
            .push(
                SlotHeader {
                    index,
                    meta: [len as u64, 0, 0, 0],
                },
                &data[..len],
            )
            .unwrap();
    }
}

#[test]
fn reader_sees_frames_from_another_process() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("chr-shm-cross-process-{}", std::process::id()));
    let path = dir.join("ring");
    let _ = std::fs::remove_dir_all(&dir);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["child_writer", "--exact", "--nocapture"])
        .env(RING_ENV, &path)
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    let reader = loop {
        match Reader::open(&path) {
            Ok(reader) => break reader,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
            Err(error) => panic!("ring never appeared: {error}"),
        }
    };
    assert_eq!(reader.layout(), LAYOUT);

    let mut payload = Vec::new();
    let check = |header: SlotHeader, payload: &[u8]| {
        assert_eq!(payload.len(), payload_len(header.index));
        assert!(
            payload.iter().all(|&byte| byte == header.index as u8),
            "torn frame {}",
            header.index
        );
    };
    while child.try_wait().unwrap().is_none() {
        if let Some(slot) = reader.latest_slot()
            && let Read::Frame(header) = reader.read(slot, &mut payload).unwrap()
        {
            check(header, &payload);
        }
    }
    assert!(child.wait().unwrap().success());

    assert_eq!(reader.write_count(), FRAMES);
    for index in FRAMES - 8..FRAMES {
        let Read::Frame(header) = reader.read((index % 8) as u32, &mut payload).unwrap() else {
            panic!("slot of frame {index} is not readable");
        };
        assert_eq!(header.index, index);
        check(header, &payload);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
