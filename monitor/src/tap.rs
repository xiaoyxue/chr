//! Program tap -> H.264: follows `chr-server`'s shared-memory program tap and encodes
//! the newest transmitted slot while anyone watches.
//!
//! The monitor polls at half the frame period. Slots it never read because a newer one
//! was already there are counted as skipped; the server never waits for it. When the
//! server restarts it replaces the tap file, so an idle tap is reopened. Every slot
//! carries its mode: when it changes (chr-server's GStreamer core follows its
//! input's rate, #154), the monitor restarts its encoder in the new mode, and the
//! media time of the encoded stream runs on without a jump.

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
    BoxError, Counters, EncoderInfo, VideoMode,
    encoder::{EncodedFrame, H264Encoder, VideoFeed},
};
use chr_common::rate::Rate;

/// How long a tap may stay idle before the monitor checks whether it was replaced.
const REOPEN_AFTER: Duration = Duration::from_secs(1);

/// The H.264 encoder used for transmitted slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    OpenH264,
    /// GStreamer NVENC when it works, otherwise x264; needs `gstreamer-media`.
    GStreamer,
    /// GStreamer NVENC only; needs `gstreamer-media`.
    Nvenc,
    /// GStreamer x264 only; needs `gstreamer-media`.
    X264,
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
        #[cfg(all(unix, feature = "gstreamer-media"))]
        let gstreamer = |choice| -> Result<Self, BoxError> {
            Ok(Self::GStreamer(crate::gstreamer::Encoder::new(
                mode, choice,
            )?))
        };
        match kind {
            // The SDI frames are BT.709 limited range; signal that to decoders.
            EncoderKind::OpenH264 => Ok(Self::OpenH264 {
                encoder: Box::new(H264Encoder::new(mode.fps, VuiConfig::bt709())?),
                planes: I420::new([mode.width, mode.height], mode.preview()),
            }),
            #[cfg(all(unix, feature = "gstreamer-media"))]
            EncoderKind::GStreamer => gstreamer(crate::gstreamer::Choice::Auto),
            #[cfg(all(unix, feature = "gstreamer-media"))]
            EncoderKind::Nvenc => gstreamer(crate::gstreamer::Choice::Nvenc),
            #[cfg(all(unix, feature = "gstreamer-media"))]
            EncoderKind::X264 => gstreamer(crate::gstreamer::Choice::X264),
            #[cfg(not(all(unix, feature = "gstreamer-media")))]
            EncoderKind::GStreamer | EncoderKind::Nvenc | EncoderKind::X264 => {
                Err("the GStreamer encoders need Linux/Unix and the gstreamer-media feature".into())
            }
        }
    }

    fn info(&self) -> EncoderInfo {
        match self {
            Self::OpenH264 { planes, .. } => EncoderInfo {
                name: "openh264",
                preview: [planes.width as u32, planes.height as u32],
            },
            #[cfg(all(unix, feature = "gstreamer-media"))]
            Self::GStreamer(encoder) => EncoderInfo {
                name: encoder.h264().element(),
                preview: encoder.preview(),
            },
        }
    }

    /// Annex B bytes and whether they form a keyframe, if a frame was produced.
    fn encode(
        &mut self,
        pts: Duration,
        duration: Duration,
        uyvy: &[u8],
        source_width: usize,
        keyframe: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, BoxError> {
        match self {
            Self::OpenH264 { encoder, planes } => {
                // openh264 has no timestamps; RTP and SRT time come from `pts` later.
                let _ = (pts, duration);
                planes.fill_from_uyvy(uyvy, source_width);
                let (width, height) = (planes.width, planes.height);
                let yuv = YUVSlices::new(
                    (&planes.y, &planes.u, &planes.v),
                    (width, height),
                    (width, width / 2, width / 2),
                );
                Ok(encoder.encode_yuv(&yuv, keyframe)?)
            }
            #[cfg(all(unix, feature = "gstreamer-media"))]
            Self::GStreamer(encoder) => Ok(Some(encoder.encode(pts, duration, uyvy, keyframe)?)),
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
        let kind = encoder;
        let encoder = SlotEncoder::new(kind, mode)?;
        log_encoder(&encoder, mode);
        counters.set_encoder(encoder.info());
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("chr-monitor".into())
                .spawn(move || {
                    run(
                        Follower::new(path),
                        mode,
                        (kind, encoder),
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

fn log_encoder(encoder: &SlotEncoder, mode: VideoMode) {
    let info = encoder.info();
    log::info!(
        "monitor preview {}x{} of {} with {}",
        info.preview[0],
        info.preview[1],
        mode.name(),
        info.name
    );
}

fn run(
    mut tap: Follower,
    mut mode: VideoMode,
    (kind, mut encoder): (EncoderKind, SlotEncoder),
    feed: &VideoFeed,
    counters: &Counters,
    stop: &AtomicBool,
) {
    let mut payload = Vec::new();
    let mut timeline = Timeline::default();
    // Last slot index encoded in the current viewing run, for counting skipped slots.
    let mut last_encoded: Option<u64> = None;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(mode.frame_duration() / 2);
        let Some((header, slot_mode)) = tap.newest(&mut payload) else {
            continue;
        };
        if slot_mode != mode {
            match SlotEncoder::new(kind, slot_mode) {
                Ok(restarted) => {
                    log::info!(
                        "program mode changed from {} to {}",
                        mode.name(),
                        slot_mode.name()
                    );
                    encoder = restarted;
                    log_encoder(&encoder, slot_mode);
                    counters.set_encoder(encoder.info());
                    counters.mode_changes.fetch_add(1, Ordering::Relaxed);
                    mode = slot_mode;
                }
                Err(error) => {
                    tap.report(format!("cannot encode {}: {error}", slot_mode.name()));
                    continue;
                }
            }
        }
        let (frame, pts) = timeline.map(header.index, mode.fps);
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
        match encoder.encode(
            pts,
            mode.frame_duration(),
            &payload,
            mode.width as usize,
            feed.take_keyframe_request(),
        ) {
            // RTP and SRT time follow output slots, the cadence the port transmits.
            Ok(Some((data, keyframe))) => {
                counters.frames_encoded.fetch_add(1, Ordering::Relaxed);
                feed.publish(EncodedFrame {
                    frame,
                    pts,
                    duration: mode.frame_duration(),
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
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            reader: None,
            file: None,
            seen: 0,
            idle_since: Instant::now(),
            problem: None,
        }
    }

    /// The newest slot written since the last call and its mode, with its payload
    /// in `payload`.
    fn newest(&mut self, payload: &mut Vec<u8>) -> Option<(SlotHeader, VideoMode)> {
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
        let mode = VideoMode::of_slot(&meta).filter(|mode| {
            mode.width.is_multiple_of(2)
                && mode.height.is_multiple_of(2)
                && payload.len() as u64 == u64::from(mode.width) * u64::from(mode.height) * 2
        });
        let Some(mode) = mode else {
            self.report(format!(
                "program tap slot of {}x{} at {}/{} fps with {} bytes is not a UYVY frame",
                meta.width,
                meta.height,
                meta.rate_num,
                meta.rate_den,
                payload.len()
            ));
            return None;
        };
        Some((header, mode))
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

/// Output slot indices restart with `chr-server`, and the rate may change;
/// encoders and RTP need monotonic frames and media time.
#[derive(Default)]
struct Timeline {
    offset: u64,
    last: Option<(u64, Duration, Rate)>,
    /// Frame, media time and rate from which the current rate counts.
    epoch: Option<(u64, Duration, Rate)>,
}

impl Timeline {
    /// The monotonic frame and media time of slot `index` at `rate`.
    fn map(&mut self, index: u64, rate: Rate) -> (u64, Duration) {
        let mut frame = index.saturating_add(self.offset);
        if let Some((last, ..)) = self.last
            && frame <= last
        {
            self.offset = last + 1 - index;
            frame = last + 1;
        }
        let pts = match (self.epoch, self.last) {
            (Some((start, pts, epoch_rate)), _) if epoch_rate == rate => {
                pts + rate.frame_time(frame - start)
            }
            // A new rate starts where the old one's slots end.
            (_, Some((last, last_pts, last_rate))) => {
                let pts = last_pts + last_rate.frame_time(frame - last);
                self.epoch = Some((frame, pts, rate));
                pts
            }
            (_, None) => {
                self.epoch = Some((0, Duration::ZERO, rate));
                rate.frame_time(frame)
            }
        };
        self.last = Some((frame, pts, rate));
        (frame, pts)
    }
}

/// Planar 4:2:0, as the encoder takes it, at the preview size.
struct I420 {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    /// Source column of each preview column, and source row of each preview row.
    columns: Vec<usize>,
    rows: Vec<usize>,
}

impl I420 {
    fn new(source: [u32; 2], preview: [u32; 2]) -> Self {
        // Nearest source pixel to each preview pixel's centre; the identity when
        // the sizes match.
        let map = |source: u32, preview: u32| -> Vec<usize> {
            (0..u64::from(preview))
                .map(|i| ((2 * i + 1) * u64::from(source) / (2 * u64::from(preview))) as usize)
                .collect()
        };
        let (width, height) = (preview[0] as usize, preview[1] as usize);
        let chroma = (width / 2) * (height / 2);
        Self {
            width,
            height,
            y: vec![0; width * height],
            u: vec![0; chroma],
            v: vec![0; chroma],
            columns: map(source[0], preview[0]),
            rows: map(source[1], preview[1]),
        }
    }

    /// Unpacks UYVY 4:2:2 and halves chroma vertically by averaging row pairs,
    /// sampling the nearest source pixels when the preview is smaller.
    fn fill_from_uyvy(&mut self, uyvy: &[u8], source_width: usize) {
        let row_bytes = source_width * 2;
        let row = |index: usize| &uyvy[self.rows[index] * row_bytes..][..row_bytes];
        for (index, luma) in self.y.chunks_exact_mut(self.width).enumerate() {
            let source = row(index);
            for (value, column) in luma.iter_mut().zip(&self.columns) {
                *value = source[column * 2 + 1];
            }
        }
        let half_width = self.width / 2;
        for chroma_row in 0..self.height / 2 {
            let (top, bottom) = (row(2 * chroma_row), row(2 * chroma_row + 1));
            let base = chroma_row * half_width;
            for pair in 0..half_width {
                // The source pixel pair holding the preview pair's left pixel.
                let at = self.columns[2 * pair] / 2 * 4;
                self.u[base + pair] = average(top[at], bottom[at]);
                self.v[base + pair] = average(top[at + 2], bottom[at + 2]);
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
        let mut planes = I420::new([4, 2], [4, 2]);
        planes.fill_from_uyvy(&uyvy, 4);
        assert_eq!(planes.y, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(planes.u, [11, 32]);
        assert_eq!(planes.v, [21, 42]);
    }

    #[test]
    fn samples_the_nearest_pixels_for_a_smaller_preview() {
        // 8x4 source, Y = 10 * row + column, U = 100 + pair, V = 200 + row.
        let uyvy: Vec<u8> = (0..4u8)
            .flat_map(|row| {
                (0..4u8).flat_map(move |pair| {
                    [
                        100 + pair,
                        10 * row + 2 * pair,
                        200 + row,
                        10 * row + 2 * pair + 1,
                    ]
                })
            })
            .collect();
        let mut planes = I420::new([8, 4], [4, 2]);
        planes.fill_from_uyvy(&uyvy, 8);
        // Preview pixel (x, y) samples source (2x + 1, 2y + 1).
        assert_eq!(planes.y, [11, 13, 15, 17, 31, 33, 35, 37]);
        // Chroma of preview pairs 0 and 1 from source pairs 0 and 2, rows 1 and 3.
        assert_eq!(planes.u, [100, 102]);
        assert_eq!(planes.v, [202, 202]);
    }

    #[test]
    fn timeline_stays_monotonic_across_server_restarts_and_rate_changes() {
        let fifty = Rate::fps(50);
        let mut timeline = Timeline::default();
        let mut map = |index, rate| {
            let (frame, pts) = timeline.map(index, rate);
            (frame, pts.as_nanos() as u64)
        };
        assert_eq!(
            [5, 6, 9].map(|index| map(index, fifty)),
            [(5, 100_000_000), (6, 120_000_000), (9, 180_000_000)]
        );
        // The server restarted: its slots count from zero again.
        assert_eq!(
            [0, 1, 3].map(|index| map(index, fifty)),
            [(10, 200_000_000), (11, 220_000_000), (13, 260_000_000)]
        );
        // 29.97 from slot 14: one 50 fps slot after slot 13, then exact steps.
        let ntsc = Rate::new(30000, 1001).unwrap();
        assert_eq!(map(4, ntsc), (14, 280_000_000));
        assert_eq!(map(5, ntsc), (15, 280_000_000 + 33_366_666));
        assert_eq!(map(3004, ntsc), (3014, 280_000_000 + 100_100_000_000));
    }

    const MODE: VideoMode = VideoMode {
        width: 64,
        height: 32,
        fps: Rate::fps(50),
    };

    fn push(tap: &mut Writer, index: u64, mode: VideoMode) {
        let meta = SlotMeta {
            source_frame: index,
            program: false,
            width: mode.width,
            height: mode.height,
            rate_num: mode.fps.num(),
            rate_den: mode.fps.den(),
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
        let mut follower = Follower::new(path);
        let mut payload = Vec::new();
        assert!(follower.newest(&mut payload).is_none());
        push(&mut tap, 1, MODE);
        assert_eq!(follower.newest(&mut payload).unwrap().0.index, 1);
        // A 1 fps server: the next slot arrives just as the tap counts as idle.
        std::thread::sleep(REOPEN_AFTER + Duration::from_millis(50));
        push(&mut tap, 2, MODE);
        assert_eq!(follower.newest(&mut payload).unwrap().0.index, 2);
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
        let before = loop {
            let frame = recv(&mut viewer);
            if frame.frame == last {
                break frame;
            }
        };
        std::thread::sleep(Duration::from_millis(50));
        let encoded = counters.frames_encoded.load(Ordering::Relaxed) - encoded;
        let skipped = counters.frames_skipped.load(Ordering::Relaxed) - skipped;
        assert_eq!(
            encoded + skipped,
            5,
            "every slot is encoded or counted as skipped"
        );

        // A slot at another rate restarts the encoder: the next frame is a
        // keyframe, and media time runs on.
        let deadline = Instant::now() + Duration::from_secs(5);
        let quarter = VideoMode {
            fps: Rate::fps(25),
            ..MODE
        };
        let changed = loop {
            push(&mut tap, last + 1, quarter);
            if let Ok(frame) = viewer.try_recv() {
                break frame;
            }
            assert!(Instant::now() < deadline, "the new mode is not encoded");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(changed.keyframe && changed.pts > before.pts);
        assert_eq!(changed.duration, Duration::from_millis(40));
        assert_eq!(counters.mode_changes.load(Ordering::Relaxed), 1);

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
