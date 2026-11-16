//! Shared-memory frame rings between chr processes.
//!
//! A ring is one file, created by its single writer and mapped by any number of
//! readers. Each slot is protected by a seqlock, so the writer never waits for a
//! reader and a reader detects and discards a slot that changed while it was
//! copied. Data leaves the mapping only through explicit copies.
//!
//! ```text
//! file header (64 bytes)
//!   magic        8 bytes  "CHRSHM01"
//!   slot_count   u32
//!   reserved     u32
//!   slot_bytes   u64      payload capacity per slot
//!   write_count  u64      atomic; incremented after each completed write
//!   reserved     …
//! slot i (64-byte header + slot_bytes payload, 64-byte aligned)
//!   sequence     u64      atomic seqlock: odd while a write is in progress
//!   index        u64      writer-defined frame index
//!   meta         [u64; 4] writer-defined metadata
//!   len          u64      valid payload bytes
//!   payload      slot_bytes
//! ```
//!
//! Integers use the host byte order: rings connect processes on one host.

pub mod input;
#[allow(unsafe_code)]
mod mapping;
pub mod tap;

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write},
    path::Path,
    sync::atomic::{Ordering, fence},
};

use mapping::Region;

/// File magic, including the layout version.
pub const MAGIC: [u8; 8] = *b"CHRSHM01";

const HEADER_BYTES: u64 = 64;
const SLOT_HEADER_BYTES: u64 = 64;
const SLOT_COUNT: usize = 8;
const SLOT_BYTES: usize = 16;
const WRITE_COUNT: usize = 24;
const SEQUENCE: usize = 0;
const INDEX: usize = 8;
const META: usize = 16;
const LEN: usize = 48;

/// Slot count and per-slot payload capacity of a ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub slot_count: u32,
    pub slot_bytes: u64,
}

impl Layout {
    fn stride(self) -> Option<u64> {
        self.slot_bytes
            .checked_next_multiple_of(64)?
            .checked_add(SLOT_HEADER_BYTES)
    }

    /// Total file size, or `None` for an empty or unaddressable layout.
    fn file_bytes(self) -> Option<u64> {
        let total = self
            .stride()?
            .checked_mul(self.slot_count.into())?
            .checked_add(HEADER_BYTES)?;
        (self.slot_count > 0 && usize::try_from(total).is_ok()).then_some(total)
    }

    /// Offset of `slot`'s header. Only called on validated layouts and slots.
    fn slot_offset(self, slot: u32) -> usize {
        (HEADER_BYTES + u64::from(slot) * self.stride().expect("validated layout")) as usize
    }
}

/// Writer-defined fields stored with each payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlotHeader {
    pub index: u64,
    pub meta: [u64; 4],
}

/// Outcome of reading one slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Read {
    /// A consistent frame; the payload buffer holds its bytes.
    Frame(SlotHeader),
    /// The slot was never written.
    Empty,
    /// A write was in progress when the read started.
    Busy,
    /// The slot changed while it was copied; the copy was discarded.
    Torn,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ring I/O: {0}")]
    Io(#[from] io::Error),
    #[error("not a chr-shm ring (magic {0:?})")]
    Magic([u8; 8]),
    #[error("invalid ring layout: {0:?}")]
    Layout(Layout),
    #[error("ring file is {actual} bytes, its layout needs {expected}")]
    Size { expected: u64, actual: u64 },
    #[error("slot {slot} is outside a {count}-slot ring")]
    Slot { slot: u32, count: u32 },
    #[error("payload of {len} bytes exceeds the slot capacity of {capacity}")]
    Payload { len: usize, capacity: u64 },
    #[error("slot {0} records a length beyond its capacity")]
    Corrupt(u32),
    #[error("frame of {len} bytes read into a buffer of {expected}")]
    Length { len: u64, expected: usize },
}

pub type Result<T> = std::result::Result<T, Error>;

/// The single writer of a ring.
pub struct Writer {
    region: Region,
    layout: Layout,
}

impl Writer {
    /// Creates the ring file at `path`, replacing any previous ring there.
    ///
    /// The file is created with mode 0600 and missing parent directories with
    /// mode 0700; `path` should be in a private directory. The ring is fully
    /// initialised before it appears at `path`, so a reader never sees a
    /// partial header.
    pub fn create(path: &Path, layout: Layout) -> Result<Self> {
        let total = layout.file_bytes().ok_or(Error::Layout(layout))?;
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            create_private_dir(dir)?;
        }
        let mut name = std::ffi::OsString::from(".");
        name.push(
            path.file_name()
                .ok_or_else(|| io::Error::other("ring path has no file name"))?,
        );
        name.push(format!(".{}.tmp", std::process::id()));
        let staging = path.with_file_name(name);

        let mut header = [0; HEADER_BYTES as usize];
        header[..8].copy_from_slice(&MAGIC);
        header[SLOT_COUNT..SLOT_COUNT + 4].copy_from_slice(&layout.slot_count.to_ne_bytes());
        header[SLOT_BYTES..SLOT_BYTES + 8].copy_from_slice(&layout.slot_bytes.to_ne_bytes());
        let staged = (|| {
            let mut file = create_private_file(&staging)?;
            file.write_all(&header)?;
            // Allocate every page now: on a full tmpfs a sparse file would only
            // fail later, as SIGBUS on the first write into the mapping.
            io::copy(&mut io::repeat(0).take(total - HEADER_BYTES), &mut file)?;
            let region = Region::writable(&file)?;
            fs::rename(&staging, path)?;
            Ok::<_, io::Error>(region)
        })();
        if staged.is_err() {
            // A partly filled staging file would keep its space on tmpfs.
            let _ = fs::remove_file(&staging);
        }
        Ok(Self {
            region: staged?,
            layout,
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Number of completed writes.
    pub fn write_count(&self) -> u64 {
        self.region.atomic(WRITE_COUNT).load(Ordering::Relaxed)
    }

    /// Writes one frame into `slot`. Never waits for readers.
    pub fn write(&mut self, slot: u32, header: SlotHeader, payload: &[u8]) -> Result<()> {
        check_slot(self.layout, slot)?;
        if payload.len() as u64 > self.layout.slot_bytes {
            return Err(Error::Payload {
                len: payload.len(),
                capacity: self.layout.slot_bytes,
            });
        }
        let base = self.layout.slot_offset(slot);
        let sequence = self.region.atomic(base + SEQUENCE).load(Ordering::Relaxed);
        self.region
            .atomic(base + SEQUENCE)
            .store(sequence + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        self.region
            .atomic(base + INDEX)
            .store(header.index, Ordering::Relaxed);
        for (i, value) in header.meta.into_iter().enumerate() {
            self.region
                .atomic(base + META + 8 * i)
                .store(value, Ordering::Relaxed);
        }
        self.region
            .atomic(base + LEN)
            .store(payload.len() as u64, Ordering::Relaxed);
        self.region
            .store(base + SLOT_HEADER_BYTES as usize, payload);
        self.region
            .atomic(base + SEQUENCE)
            .store(sequence + 2, Ordering::Release);
        self.region
            .atomic(WRITE_COUNT)
            .fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Writes into the next slot in round-robin order and returns it. Readers of
    /// rings written only through `push` find the newest frame with
    /// [`Reader::latest_slot`].
    pub fn push(&mut self, header: SlotHeader, payload: &[u8]) -> Result<u32> {
        let slot = (self.write_count() % u64::from(self.layout.slot_count)) as u32;
        self.write(slot, header, payload)?;
        Ok(slot)
    }
}

/// A read-only view of a ring created by another [`Writer`].
pub struct Reader {
    region: Region,
    layout: Layout,
}

impl Reader {
    /// Opens and validates the ring at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let actual = file.metadata()?.len();
        if actual < HEADER_BYTES {
            return Err(Error::Size {
                expected: HEADER_BYTES,
                actual,
            });
        }
        let region = Region::read_only(&file)?;
        let mut header = [0; HEADER_BYTES as usize];
        region.load(0, &mut header);
        let magic: [u8; 8] = header[..8].try_into().expect("8 bytes");
        if magic != MAGIC {
            return Err(Error::Magic(magic));
        }
        let layout = Layout {
            slot_count: u32::from_ne_bytes(
                header[SLOT_COUNT..SLOT_COUNT + 4]
                    .try_into()
                    .expect("4 bytes"),
            ),
            slot_bytes: u64::from_ne_bytes(
                header[SLOT_BYTES..SLOT_BYTES + 8]
                    .try_into()
                    .expect("8 bytes"),
            ),
        };
        let expected = layout.file_bytes().ok_or(Error::Layout(layout))?;
        if expected != actual {
            return Err(Error::Size { expected, actual });
        }
        Ok(Self { region, layout })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Number of completed writes.
    pub fn write_count(&self) -> u64 {
        let count = self.region.load_word(WRITE_COUNT);
        fence(Ordering::Acquire);
        count
    }

    /// The slot of the newest frame of a ring written only with
    /// [`Writer::push`], or `None` before the first write.
    pub fn latest_slot(&self) -> Option<u32> {
        let count = self.write_count().checked_sub(1)?;
        Some((count % u64::from(self.layout.slot_count)) as u32)
    }

    /// Copies `slot` into `payload` (resized to the frame length). `payload` is
    /// meaningful only for [`Read::Frame`].
    pub fn read(&self, slot: u32, payload: &mut Vec<u8>) -> Result<Read> {
        self.read_with(slot, Target::Resize(payload), || {})
    }

    /// Copies `slot` into `payload`, which must have exactly the frame's length;
    /// a frame of another length is [`Error::Length`]. Lets a reader copy a frame
    /// straight into memory it shares onwards.
    pub fn read_into(&self, slot: u32, payload: &mut [u8]) -> Result<Read> {
        self.read_with(slot, Target::Exact(payload), || {})
    }

    /// `read`, running `during` after the copy and before the seqlock check.
    fn read_with(&self, slot: u32, payload: Target<'_>, during: impl FnOnce()) -> Result<Read> {
        check_slot(self.layout, slot)?;
        let base = self.layout.slot_offset(slot);
        let sequence = self.region.load_word(base + SEQUENCE);
        fence(Ordering::Acquire);
        if sequence == 0 {
            return Ok(Read::Empty);
        }
        if sequence % 2 == 1 {
            return Ok(Read::Busy);
        }
        let mut header = SlotHeader {
            index: self.region.load_word(base + INDEX),
            ..Default::default()
        };
        for (i, value) in header.meta.iter_mut().enumerate() {
            *value = self.region.load_word(base + META + 8 * i);
        }
        let len = self.region.load_word(base + LEN);
        // A torn length can exceed the capacity; never copy past the slot.
        let copied = len.min(self.layout.slot_bytes) as usize;
        let expected = match payload {
            Target::Resize(payload) => {
                payload.resize(copied, 0);
                self.region.load(base + SLOT_HEADER_BYTES as usize, payload);
                None
            }
            Target::Exact(payload) if payload.len() == copied => {
                self.region.load(base + SLOT_HEADER_BYTES as usize, payload);
                None
            }
            Target::Exact(payload) => Some(payload.len()),
        };
        during();
        fence(Ordering::Acquire);
        if self.region.load_word(base + SEQUENCE) != sequence {
            return Ok(Read::Torn);
        }
        if len > self.layout.slot_bytes {
            return Err(Error::Corrupt(slot));
        }
        if let Some(expected) = expected {
            return Err(Error::Length { len, expected });
        }
        Ok(Read::Frame(header))
    }
}

/// Where a read copies the payload.
enum Target<'a> {
    /// Resized to the frame length.
    Resize(&'a mut Vec<u8>),
    /// Must have the frame length.
    Exact(&'a mut [u8]),
}

fn check_slot(layout: Layout, slot: u32) -> Result<()> {
    if slot < layout.slot_count {
        Ok(())
    } else {
        Err(Error::Slot {
            slot,
            count: layout.slot_count,
        })
    }
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("chr-shm-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const LAYOUT: Layout = Layout {
        slot_count: 4,
        slot_bytes: 100,
    };

    fn frame(index: u64) -> (SlotHeader, Vec<u8>) {
        (
            SlotHeader {
                index,
                meta: [index, 1, 2, 3],
            },
            vec![index as u8; 10 + index as usize % 90],
        )
    }

    #[test]
    fn frames_round_trip_and_latest_slot_follows_push() {
        let dir = TempDir::new("round-trip");
        let path = dir.0.join("ring");
        let mut writer = Writer::create(&path, LAYOUT).unwrap();
        let reader = Reader::open(&path).unwrap();
        assert_eq!(reader.layout(), LAYOUT);
        assert_eq!(reader.latest_slot(), None);
        let mut payload = Vec::new();
        assert_eq!(reader.read(0, &mut payload).unwrap(), Read::Empty);

        for index in 0..6 {
            let (header, data) = frame(index);
            assert_eq!(writer.push(header, &data).unwrap(), (index % 4) as u32);
        }
        assert_eq!(reader.write_count(), 6);
        assert_eq!(reader.latest_slot(), Some(1));
        let (header, data) = frame(5);
        assert_eq!(reader.read(1, &mut payload).unwrap(), Read::Frame(header));
        assert_eq!(payload, data);

        writer
            .write(
                3,
                SlotHeader {
                    index: 9,
                    meta: [0; 4],
                },
                &[],
            )
            .unwrap();
        assert_eq!(
            reader.read(3, &mut payload).unwrap(),
            Read::Frame(SlotHeader {
                index: 9,
                meta: [0; 4]
            })
        );
        assert!(payload.is_empty());
    }

    #[test]
    fn writes_and_reads_are_bounds_checked() {
        let dir = TempDir::new("bounds");
        let path = dir.0.join("ring");
        let mut writer = Writer::create(&path, LAYOUT).unwrap();
        let reader = Reader::open(&path).unwrap();
        assert!(matches!(
            writer.write(4, SlotHeader::default(), &[]),
            Err(Error::Slot { slot: 4, count: 4 })
        ));
        assert!(matches!(
            writer.push(SlotHeader::default(), &[0; 101]),
            Err(Error::Payload {
                len: 101,
                capacity: 100
            })
        ));
        assert!(matches!(
            reader.read(4, &mut Vec::new()),
            Err(Error::Slot { slot: 4, count: 4 })
        ));
        assert_eq!(writer.write_count(), 0);
        assert!(matches!(
            Writer::create(
                &path,
                Layout {
                    slot_count: 0,
                    slot_bytes: 8
                }
            ),
            Err(Error::Layout(_))
        ));
        assert!(matches!(
            Writer::create(
                &path,
                Layout {
                    slot_count: 2,
                    slot_bytes: u64::MAX
                }
            ),
            Err(Error::Layout(_))
        ));
    }

    #[test]
    fn open_rejects_foreign_and_mismatched_files() {
        let dir = TempDir::new("open");
        let path = dir.0.join("ring");
        Writer::create(&path, LAYOUT).unwrap();
        let ring = fs::read(&path).unwrap();

        fs::write(&path, &ring[..10]).unwrap();
        assert!(matches!(
            Reader::open(&path),
            Err(Error::Size {
                expected: 64,
                actual: 10
            })
        ));

        let mut foreign = ring.clone();
        foreign[..8].copy_from_slice(b"NOTARING");
        fs::write(&path, &foreign).unwrap();
        assert!(matches!(Reader::open(&path), Err(Error::Magic(magic)) if &magic == b"NOTARING"));

        fs::write(&path, &ring[..ring.len() - 64]).unwrap();
        assert!(matches!(Reader::open(&path), Err(Error::Size { .. })));

        let mut empty = ring.clone();
        empty[SLOT_COUNT..SLOT_COUNT + 4].copy_from_slice(&0u32.to_ne_bytes());
        fs::write(&path, &empty).unwrap();
        assert!(matches!(Reader::open(&path), Err(Error::Layout(_))));

        assert!(matches!(
            Reader::open(&dir.0.join("missing")),
            Err(Error::Io(_))
        ));
    }

    #[test]
    fn reader_rejects_busy_and_torn_slots() {
        let dir = TempDir::new("seqlock");
        let path = dir.0.join("ring");
        let mut writer = Writer::create(&path, LAYOUT).unwrap();
        let reader = Reader::open(&path).unwrap();
        let mut payload = Vec::new();

        let (header, data) = frame(1);
        writer.write(0, header, &data).unwrap();
        let sequence = writer
            .region
            .atomic(LAYOUT.slot_offset(0))
            .load(Ordering::Relaxed);
        writer
            .region
            .atomic(LAYOUT.slot_offset(0))
            .store(sequence + 1, Ordering::Relaxed);
        assert_eq!(reader.read(0, &mut payload).unwrap(), Read::Busy);
        writer
            .region
            .atomic(LAYOUT.slot_offset(0))
            .store(sequence, Ordering::Relaxed);

        let (newer, newer_data) = frame(2);
        let read = reader.read_with(0, Target::Resize(&mut payload), || {
            writer.write(0, newer, &newer_data).unwrap()
        });
        assert_eq!(read.unwrap(), Read::Torn);
        assert_eq!(reader.read(0, &mut payload).unwrap(), Read::Frame(newer));
        assert_eq!(payload, newer_data);
    }

    #[test]
    fn read_into_needs_the_frame_length() {
        let dir = TempDir::new("into");
        let path = dir.0.join("ring");
        let mut writer = Writer::create(&path, LAYOUT).unwrap();
        let reader = Reader::open(&path).unwrap();
        let (header, data) = frame(3);
        writer.write(1, header, &data).unwrap();
        let mut payload = vec![0; data.len()];
        assert_eq!(
            reader.read_into(1, &mut payload).unwrap(),
            Read::Frame(header)
        );
        assert_eq!(payload, data);
        assert!(matches!(
            reader.read_into(1, &mut vec![0; data.len() + 1]),
            Err(Error::Length { .. })
        ));
    }

    #[test]
    fn writer_progresses_past_a_stalled_reader() {
        let dir = TempDir::new("stalled");
        let path = dir.0.join("ring");
        let mut writer = Writer::create(&path, LAYOUT).unwrap();
        let reader = Reader::open(&path).unwrap();
        let (header, data) = frame(0);
        writer.push(header, &data).unwrap();

        let mut payload = Vec::new();
        let read = reader.read_with(0, Target::Resize(&mut payload), || {
            for index in 1..=12 {
                let (header, data) = frame(index);
                writer.push(header, &data).unwrap();
            }
        });
        assert_eq!(read.unwrap(), Read::Torn);
        assert_eq!(reader.write_count(), 13);
        let (header, _) = frame(12);
        assert_eq!(
            reader
                .read(reader.latest_slot().unwrap(), &mut payload)
                .unwrap(),
            Read::Frame(header)
        );
    }

    #[test]
    fn concurrent_reads_are_consistent_or_discarded() {
        let dir = TempDir::new("concurrent");
        let path = dir.0.join("ring");
        let layout = Layout {
            slot_count: 2,
            slot_bytes: 64 * 1024,
        };
        let mut writer = Writer::create(&path, layout).unwrap();
        let reader = Reader::open(&path).unwrap();
        const FRAMES: u64 = 5_000;

        let writing = std::thread::spawn(move || {
            let mut data = vec![0; layout.slot_bytes as usize];
            for index in 0..FRAMES {
                let len = 1 + (index as usize * 7919) % data.len();
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
        });
        let (mut frames, mut payload) = (0, Vec::new());
        while reader.write_count() < FRAMES {
            let Some(slot) = reader.latest_slot() else {
                continue;
            };
            if let Read::Frame(header) = reader.read(slot, &mut payload).unwrap() {
                assert_eq!(payload.len() as u64, header.meta[0]);
                assert!(
                    payload.iter().all(|&byte| byte == header.index as u8),
                    "torn frame {}",
                    header.index
                );
                frames += 1;
            }
        }
        writing.join().unwrap();
        assert!(frames > 0);
    }

    #[test]
    fn failed_create_removes_its_staging_file() {
        let dir = TempDir::new("failed-create");
        let path = dir.0.join("ring");
        fs::create_dir_all(path.join("occupied")).unwrap();
        assert!(matches!(Writer::create(&path, LAYOUT), Err(Error::Io(_))));
        assert_eq!(
            fs::read_dir(&dir.0).unwrap().count(),
            1,
            "staging file left behind"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ring_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("private");
        let path = dir.0.join("nested").join("ring");
        Writer::create(&path, LAYOUT).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(
            fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "staging file left behind"
        );
    }
}
