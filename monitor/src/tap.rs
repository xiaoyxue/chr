//! Program tap -> H.264: follows `chr-server`'s shared-memory program tap and encodes
//! the newest transmitted slot while anyone watches.
//!
//! The monitor polls at half the frame period. Slots it never read because a newer one
//! was already there are counted as skipped; the server never waits for it. When the
//! server restarts it replaces the tap file, so an idle tap is reopened.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use bytes::Bytes;
use chr_shm::{Read, Reader, SlotHeader, tap::SlotMeta};
use openh264::{encoder::VuiConfig, formats::YUVSlices};

use crate::{
    BoxError, Counters, VideoMode,
    encoder::{EncodedFrame, H264Encoder, VideoFeed},
};

/// How long a tap may stay idle before the monitor checks whether it was replaced.
const REOPEN_AFTER: Duration = Duration::from_secs(1);

/// The H.264 encoder used for transmitted slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    OpenH264,
    /// GStreamer x264; needs the `gstreamer-media` feature.
    GStreamer,
}

enum SlotEncoder {
    OpenH264 {
        encoder: Box<H264Encoder>,
        planes: I420,
    },
    #[cfg(all(unix, feature = "gstreamer-media"))]
    GStreamer(crate::gstreamer::Encoder),
}

impl SlotEncoder {
    fn new(kind: EncoderKind, mode: VideoMode) -> Result<Self, BoxError> {
        if !mode.width.is_multiple_of(2) || !mode.height.is_multiple_of(2) {
            return Err("4:2:0 encoding needs an even width and height".into());
        }
        match kind {
            // The SDI frames are BT.709 limited range; signal that to decoders.
            EncoderKind::OpenH264 => Ok(Self::OpenH264 {
                encoder: Box::new(H264Encoder::new(mode.fps, VuiConfig::bt709())?),
                planes: I420::new(mode.width as usize, mode.height as usize),
            }),
            #[cfg(all(unix, feature = "gstreamer-media"))]
            EncoderKind::GStreamer => Ok(Self::GStreamer(crate::gstreamer::Encoder::new(mode)?)),
            #[cfg(not(all(unix, feature = "gstreamer-media")))]
            EncoderKind::GStreamer => {
                Err("the GStreamer encoder needs Linux/Unix and the gstreamer-media feature".into())
            }
        }
    }

    /// Annex B bytes and whether they form a keyframe, if a frame was produced.
    fn encode(
        &mut self,
        frame: u64,
        uyvy: &[u8],
        keyframe: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, BoxError> {
        match self {
            Self::OpenH264 { encoder, planes } => {
                // openh264 has no timestamps; RTP and SRT time come from `frame` later.
                let _ = frame;
                planes.fill_from_uyvy(uyvy);
                let (width, height) = (planes.width, planes.height);
                let yuv = YUVSlices::new(
                    (&planes.y, &planes.u, &planes.v),
                    (width, height),
                    (width, width / 2, width / 2),
                );
                Ok(encoder.encode_yuv(&yuv, keyframe)?)
            }
            #[cfg(all(unix, feature = "gstreamer-media"))]
            Self::GStreamer(encoder) => Ok(Some(encoder.encode(frame, uyvy, keyframe)?)),
        }
    }
}

pub struct Monitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    /// Encodes the tap at `path` into `feed` while anyone watches. The tap may appear,
    /// disappear or be replaced at any time.
    pub fn spawn(
        path: PathBuf,
        mode: VideoMode,
        encoder: EncoderKind,
        feed: Arc<VideoFeed>,
        counters: Arc<Counters>,
    ) -> Result<Self, BoxError> {
        let encoder = SlotEncoder::new(encoder, mode)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("chr-monitor".into())
                .spawn(move || {
                    run(
                        Follower::new(path, mode),
                        mode,
                        encoder,
                        &feed,
                        &counters,
                        &stop,
                    )
                })?
        };
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    mut tap: Follower,
    mode: VideoMode,
    mut encoder: SlotEncoder,
    feed: &VideoFeed,
    counters: &Counters,
    stop: &AtomicBool,
) {
    let poll = mode.frame_duration() / 2;
    let mut payload = Vec::new();
    let mut timeline = Timeline::default();
    // Last slot index encoded in the current viewing run, for counting skipped slots.
    let mut last_encoded: Option<u64> = None;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(poll);
        let Some(header) = tap.newest(&mut payload) else {
            continue;
        };
        let frame = timeline.map(header.index);
        if !feed.has_viewers() {
            last_encoded = None;
            continue;
        }
        if let Some(last) = last_encoded
            && header.index > last + 1
        {
            counters
                .frames_skipped
                .fetch_add(header.index - last - 1, Ordering::Relaxed);
        }
        last_encoded = Some(header.index);
        match encoder.encode(frame, &payload, feed.take_keyframe_request()) {
            // RTP and SRT time follow output slots, the cadence the port transmits.
            Ok(Some((data, keyframe))) => {
                counters.frames_encoded.fetch_add(1, Ordering::Relaxed);
                feed.publish(EncodedFrame {
                    frame,
                    keyframe,
                    data: Bytes::from(data),
                });
            }
            Ok(None) => {}
            Err(error) => {
                log::error!("monitor encoder failed: {error}");
                return;
            }
        }
    }
}

/// Reads the newest slot of the tap at `path`, reopening it when it was replaced.
struct Follower {
    path: PathBuf,
    mode: VideoMode,
    reader: Option<Reader>,
    /// Identity of the file `reader` maps, if the platform reports one.
    file: Option<FileId>,
    /// Write count of the last slot handed out.
    seen: u64,
    idle_since: Instant,
    /// The last open or mode problem, logged once until it changes.
    problem: Option<String>,
}

impl Follower {
    fn new(path: PathBuf, mode: VideoMode) -> Self {
        Self {
            path,
            mode,
            reader: None,
            file: None,
            seen: 0,
            idle_since: Instant::now(),
            problem: None,
        }
    }

    /// The newest slot written since the last call, with its payload in `payload`.
    fn newest(&mut self, payload: &mut Vec<u8>) -> Option<SlotHeader> {
        if self.reader.is_none() || self.idle_since.elapsed() >= REOPEN_AFTER {
            self.reopen();
        }
        let reader = self.reader.as_ref()?;
        let count = reader.write_count();
        if count == self.seen {
            return None;
        }
        self.seen = count;
        self.idle_since = Instant::now();
        let read = reader.read(reader.latest_slot()?, payload);
        let header = match read {
            Ok(Read::Frame(header)) => header,
            // Overwritten while copying; a newer slot is there next time.
            Ok(Read::Busy | Read::Torn | Read::Empty) => return None,
            Err(error) => {
                self.report(format!("unreadable program tap slot: {error}"));
                return None;
            }
        };
        let meta = SlotMeta::from_meta(header.meta);
        let mode = VideoMode {
            width: meta.width,
            height: meta.height,
            fps: meta.fps,
        };
        if mode != self.mode {
            self.report(format!(
                "program tap carries {}, the monitor expects {}",
                mode.name(),
                self.mode.name()
            ));
            return None;
        }
        Some(header)
    }

    fn reopen(&mut self) {
        self.idle_since = Instant::now();
        // An idle but unchanged tap keeps its reader and read position, so a slow
        // or stalled server loses no slot to the check.
        let file = file_id(&self.path);
        if self.reader.is_some() && file.is_some() && file == self.file {
            return;
        }
        let reader = match Reader::open(&self.path) {
            Ok(reader) => reader,
            Err(error) => {
                if self.reader.is_none() {
                    self.report(format!(
                        "waiting for the program tap at {}: {error}",
                        self.path.display()
                    ));
                }
                return;
            }
        };
        let expected = chr_shm::tap::layout(self.mode.width, self.mode.height);
        if reader.layout() != expected {
            self.report(format!(
                "program tap layout {:?} does not match {} (expected {expected:?})",
                reader.layout(),
                self.mode.name()
            ));
            return;
        }
        // A newly opened tap is followed from its next slot.
        self.seen = reader.write_count();
        if self.reader.is_none() || self.problem.is_some() {
            log::info!("reading program tap at {}", self.path.display());
        }
        self.problem = None;
        self.reader = Some(reader);
        self.file = file;
    }

    fn report(&mut self, problem: String) {
        if self.problem.as_ref() != Some(&problem) {
            log::warn!("{problem}");
            self.problem = Some(problem);
        }
    }
}

type FileId = (u64, u64);

/// Device and inode of `path`: a restarted server replaces the tap with a new file.
#[cfg(unix)]
fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

/// Without file identity every idle check reopens the tap.
#[cfg(not(unix))]
fn file_id(_path: &Path) -> Option<FileId> {
    None
}

/// Output slot indices restart with `chr-server`; encoders and RTP need monotonic time.
#[derive(Default)]
struct Timeline {
    offset: u64,
    last: Option<u64>,
}

impl Timeline {
    fn map(&mut self, index: u64) -> u64 {
        let mut frame = index.saturating_add(self.offset);
        if let Some(last) = self.last
            && frame <= last
        {
            self.offset = last + 1 - index;
            frame = last + 1;
        }
        self.last = Some(frame);
        frame
    }
}

/// Planar 4:2:0, as the encoder takes it.
struct I420 {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl I420 {
    fn new(width: usize, height: usize) -> Self {
        let chroma = (width / 2) * (height / 2);
        Self {
            width,
            height,
            y: vec![0; width * height],
            u: vec![0; chroma],
            v: vec![0; chroma],
        }
    }

    /// Unpacks UYVY 4:2:2 and halves chroma vertically by averaging row pairs.
    fn fill_from_uyvy(&mut self, uyvy: &[u8]) {
        let row_bytes = self.width * 2;
        let half_width = self.width / 2;
        for (row_index, row) in uyvy.chunks_exact(row_bytes).take(self.height).enumerate() {
            let luma = &mut self.y[row_index * self.width..][..self.width];
            for (pair, group) in row.as_chunks::<4>().0.iter().enumerate() {
                luma[pair * 2] = group[1];
                luma[pair * 2 + 1] = group[3];
            }
        }
        for chroma_row in 0..self.height / 2 {
            let top = &uyvy[2 * chroma_row * row_bytes..][..row_bytes];
            let bottom = &uyvy[(2 * chroma_row + 1) * row_bytes..][..row_bytes];
            let base = chroma_row * half_width;
            for pair in 0..half_width {
                let (t, b) = (
                    &top[pair * 4..pair * 4 + 4],
                    &bottom[pair * 4..pair * 4 + 4],
                );
                self.u[base + pair] = average(t[0], b[0]);
                self.v[base + pair] = average(t[2], b[2]);
            }
        }
    }
}

fn average(a: u8, b: u8) -> u8 {
    ((u16::from(a) + u16::from(b)).div_ceil(2)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use chr_shm::Writer;

    #[test]
    fn unpacks_uyvy_into_i420() {
        // 4x2 image: two pixel pairs per row, rows differ in chroma.
        let uyvy = [
            10, 1, 20, 2, 30, 3, 40, 4, // row 0: (Cb, Y0, Cr, Y1) x 2
            12, 5, 22, 6, 33, 7, 44, 8, // row 1
        ];
        let mut planes = I420::new(4, 2);
        planes.fill_from_uyvy(&uyvy);
        assert_eq!(planes.y, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(planes.u, [11, 32]);
        assert_eq!(planes.v, [21, 42]);
    }

    #[test]
    fn timeline_stays_monotonic_across_server_restarts() {
        let mut timeline = Timeline::default();
        assert_eq!([5, 6, 9].map(|index| timeline.map(index)), [5, 6, 9]);
        // The server restarted: its slots count from zero again.
        assert_eq!([0, 1, 3].map(|index| timeline.map(index)), [10, 11, 13]);
    }

    const MODE: VideoMode = VideoMode {
        width: 64,
        height: 32,
        fps: 50,
    };

    fn push(tap: &mut Writer, index: u64, mode: VideoMode) {
        let meta = SlotMeta {
            source_frame: index,
            program: false,
            width: mode.width,
            height: mode.height,
            fps: mode.fps,
        };
        let uyvy = [128, 100, 128, 100].repeat((mode.width * mode.height / 2) as usize);
        tap.push(
            SlotHeader {
                index,
                meta: meta.to_meta(),
            },
            &uyvy,
        )
        .unwrap();
    }

    fn recv(viewer: &mut tokio::sync::broadcast::Receiver<Arc<EncodedFrame>>) -> Arc<EncodedFrame> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match viewer.try_recv() {
                Ok(frame) => return frame,
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                    if Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("no encoded slot: {error}"),
            }
        }
    }

    #[test]
    fn a_slot_after_an_idle_period_is_not_lost() {
        let dir = std::env::temp_dir().join(format!("chr-monitor-idle-{}", std::process::id()));
        let path = dir.join("program.tap");
        let mut tap = Writer::create(&path, chr_shm::tap::layout(MODE.width, MODE.height)).unwrap();
        let mut follower = Follower::new(path, MODE);
        let mut payload = Vec::new();
        assert!(follower.newest(&mut payload).is_none());
        push(&mut tap, 1, MODE);
        assert_eq!(follower.newest(&mut payload).unwrap().index, 1);
        // A 1 fps server: the next slot arrives just as the tap counts as idle.
        std::thread::sleep(REOPEN_AFTER + Duration::from_millis(50));
        push(&mut tap, 2, MODE);
        assert_eq!(follower.newest(&mut payload).unwrap().index, 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn encodes_the_newest_tap_slot_and_follows_a_restarted_server() {
        let dir = std::env::temp_dir().join(format!("chr-monitor-tap-{}", std::process::id()));
        let path = dir.join("program.tap");
        let feed = Arc::new(VideoFeed::new());
        let counters = Arc::new(Counters::default());
        let mut viewer = feed.subscribe();
        // The monitor may start before the server.
        let monitor = Monitor::spawn(
            path.clone(),
            MODE,
            EncoderKind::OpenH264,
            Arc::clone(&feed),
            Arc::clone(&counters),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(50));

        let mut tap = Writer::create(&path, chr_shm::tap::layout(MODE.width, MODE.height)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut index = 7;
        let first = loop {
            push(&mut tap, index, MODE);
            if let Ok(frame) = viewer.try_recv() {
                break frame;
            }
            assert!(Instant::now() < deadline, "no encoded slot");
            index += 1;
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(
            (7..=index).contains(&first.frame),
            "RTP timing follows output slots"
        );
        assert!(first.keyframe);

        // Several slots between two polls: the monitor encodes the newest and counts
        // the rest as skipped.
        std::thread::sleep(Duration::from_millis(50));
        while viewer.try_recv().is_ok() {}
        let (encoded, skipped) = (
            counters.frames_encoded.load(Ordering::Relaxed),
            counters.frames_skipped.load(Ordering::Relaxed),
        );
        let last = index + 5;
        for index in index + 1..=last {
            push(&mut tap, index, MODE);
        }
        while recv(&mut viewer).frame != last {}
        std::thread::sleep(Duration::from_millis(50));
        let encoded = counters.frames_encoded.load(Ordering::Relaxed) - encoded;
        let skipped = counters.frames_skipped.load(Ordering::Relaxed) - skipped;
        assert_eq!(
            encoded + skipped,
            5,
            "every slot is encoded or counted as skipped"
        );

        // A slot in another mode is not encoded.
        push(&mut tap, last + 1, VideoMode { fps: 25, ..MODE });
        std::thread::sleep(Duration::from_millis(100));
        assert!(viewer.try_recv().is_err());

        // A restarted server replaces the tap and counts from zero.
        drop(tap);
        let mut tap = Writer::create(&path, chr_shm::tap::layout(MODE.width, MODE.height)).unwrap();
        let restarted = loop {
            push(&mut tap, 0, MODE);
            if let Ok(frame) = viewer.try_recv() {
                break frame;
            }
            assert!(
                Instant::now() < deadline + REOPEN_AFTER * 3,
                "restarted tap not followed"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(restarted.frame > last, "encoder time stays monotonic");
        drop(monitor);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
