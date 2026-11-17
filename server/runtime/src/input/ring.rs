//! Frames from a capture process through the shared-memory input ring
//! ([`chr_shm::input`]), standing in for an SDI input card. No physical SDI.
//!
//! Each frame is copied once, straight from the ring into memfd-backed memory
//! ([`SharedPixels`]) that the tracking and render workers then receive
//! without another copy.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use chr_shm::{Read, Reader, input::SlotMeta};

use crate::{sdi::VideoMode, worker::transport::SharedPixels};

/// How long the ring may stay idle before the reader checks whether it was replaced.
const REOPEN_AFTER: Duration = Duration::from_secs(1);

/// What one [`RingInput::poll`] found.
#[derive(Debug, Default)]
pub struct Polled {
    /// `(capture sequence, RGBA8 pixels)`, oldest first.
    pub frames: Vec<(u64, SharedPixels)>,
    /// Frames the writer overwrote before they were read.
    pub overwritten: u64,
    /// A new ring replaced the previous one: capture sequences restart.
    pub restarted: bool,
}

/// Reads every frame of the input ring at `path` in write order.
pub struct RingInput {
    path: PathBuf,
    mode: VideoMode,
    reader: Option<Reader>,
    file: Option<(u64, u64)>,
    /// Write count of the next frame to read.
    next_write: u64,
    idle_since: Instant,
    problem: Option<String>,
}

impl RingInput {
    /// `mode` is the native extent and rate the capture process must write.
    pub fn new(path: PathBuf, mode: VideoMode) -> Self {
        Self {
            path,
            mode,
            reader: None,
            file: None,
            next_write: 0,
            idle_since: Instant::now(),
            problem: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn poll(&mut self) -> Polled {
        let mut polled = Polled::default();
        if self.reader.is_none() || self.idle_since.elapsed() >= REOPEN_AFTER {
            polled.restarted = self.reopen();
        }
        let Some(reader) = &self.reader else {
            return polled;
        };
        let count = reader.write_count();
        if count == self.next_write {
            return polled;
        }
        self.idle_since = Instant::now();
        let slots = u64::from(reader.layout().slot_count);
        if count - self.next_write > slots {
            polled.overwritten += count - self.next_write - slots;
            self.next_write = count - slots;
        }
        let mut problem = None;
        let frame_bytes = self.mode.width as usize * self.mode.height as usize * 4;
        for write in self.next_write..count {
            let slot = (write % slots) as u32;
            let (payload, read) = match SharedPixels::filled(frame_bytes, |payload| {
                reader.read_into(slot, payload)
            }) {
                Ok(frame) => frame,
                Err(error) => {
                    problem = Some(format!("no shared memory for an input frame: {error}"));
                    continue;
                }
            };
            // A later write that completed before the copy started passes the
            // seqlock; only the write count shows that the slot was reused.
            if reader.write_count() > write + slots {
                polled.overwritten += 1;
                continue;
            }
            match read {
                Ok(Read::Frame(header)) => {
                    let meta = SlotMeta::from_meta(header.meta);
                    if meta == self.mode.input_meta() {
                        polled.frames.push((header.index, payload));
                    } else {
                        problem = Some(format!(
                            "input ring carries {}x{} at {}/{} fps, the program expects {}",
                            meta.width,
                            meta.height,
                            meta.rate_num,
                            meta.rate_den,
                            self.mode.name()
                        ));
                    }
                }
                // A later write reused the slot while it was read.
                Ok(Read::Busy | Read::Torn | Read::Empty) => polled.overwritten += 1,
                Err(error) => problem = Some(format!("unreadable input ring slot: {error}")),
            }
        }
        self.next_write = count;
        if let Some(problem) = problem {
            self.report(problem);
        }
        polled
    }

    /// Opens the ring if it is new; returns whether it replaced an earlier one.
    fn reopen(&mut self) -> bool {
        self.idle_since = Instant::now();
        let file = file_id(&self.path);
        if self.reader.is_some() && file.is_some() && file == self.file {
            return false;
        }
        let reader = match Reader::open(&self.path) {
            Ok(reader) => reader,
            Err(error) => {
                if self.reader.is_none() {
                    self.report(format!(
                        "waiting for the input ring at {}: {error}",
                        self.path.display()
                    ));
                }
                return false;
            }
        };
        let expected = chr_shm::input::layout(self.mode.width, self.mode.height);
        if reader.layout() != expected {
            self.report(format!(
                "input ring layout {:?} does not match {} (expected {expected:?})",
                reader.layout(),
                self.mode.name()
            ));
            return false;
        }
        let restarted = self.reader.is_some();
        // A newly opened ring is read from its next frame.
        self.next_write = reader.write_count();
        log::info!("reading input ring at {}", self.path.display());
        self.problem = None;
        self.reader = Some(reader);
        self.file = file;
        restarted
    }

    fn report(&mut self, problem: String) {
        if self.problem.as_ref() != Some(&problem) {
            log::warn!("{problem}");
            self.problem = Some(problem);
        }
    }
}

/// Device and inode of `path`: a restarted capture process replaces the ring.
#[cfg(unix)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_path: &Path) -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chr_common::rate::Rate;
    use chr_shm::{SlotHeader, Writer};

    const MODE: VideoMode = VideoMode {
        width: 4,
        height: 2,
        fps: Rate::fps(30),
    };

    fn push(writer: &mut Writer, sequence: u64, mode: VideoMode) {
        let meta = mode.input_meta();
        writer
            .push(
                SlotHeader {
                    index: sequence,
                    meta: meta.to_meta(),
                },
                &[sequence as u8; 32],
            )
            .unwrap();
    }

    #[test]
    fn reads_every_frame_in_order_and_counts_overwritten_ones() {
        let dir = std::env::temp_dir().join(format!("chr-input-ring-{}", std::process::id()));
        let path = dir.join("input.ring");
        let mut writer = Writer::create(&path, chr_shm::input::layout(4, 2)).unwrap();
        let mut input = RingInput::new(path.clone(), MODE);
        push(&mut writer, 99, MODE);
        let first = input.poll();
        assert!(
            first.frames.is_empty(),
            "frames before opening are not replayed"
        );
        assert!(!first.restarted);

        for sequence in [0, 1, 3] {
            push(&mut writer, sequence, MODE);
        }
        let polled = input.poll();
        let sequences: Vec<_> = polled.frames.iter().map(|(s, _)| *s).collect();
        assert_eq!(sequences, [0, 1, 3], "capture gaps keep their sequence");
        assert_eq!(*polled.frames[2].1, [3; 32]);

        for sequence in 10..20 {
            push(&mut writer, sequence, MODE);
        }
        push(
            &mut writer,
            20,
            VideoMode {
                fps: Rate::fps(25),
                ..MODE
            },
        );
        let polled = input.poll();
        assert_eq!(polled.overwritten, 3, "eleven writes into eight slots");
        let sequences: Vec<_> = polled.frames.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            sequences,
            (13..20).collect::<Vec<_>>(),
            "the wrong mode is skipped"
        );

        drop(writer);
        let mut writer = Writer::create(&path, chr_shm::input::layout(4, 2)).unwrap();
        input.idle_since -= REOPEN_AFTER;
        assert!(input.poll().restarted, "a replaced ring restarts sequences");
        push(&mut writer, 0, MODE);
        assert_eq!(input.poll().frames.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
