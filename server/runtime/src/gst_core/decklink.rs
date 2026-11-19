//! Blackmagic DeckLink input and output (#154 phase 3): GStreamer's `decklink`
//! elements (gst-plugins-bad, on the Desktop Video driver's
//! `libDeckLinkAPI.so`).
//!
//! Verified on a DeckLink Duo 2 at 1080p30 (capture, playout on the card's
//! clock, its start, CG and audio); genlock, the other modes, timecode and
//! captions on a card are not yet (`server/docs/gst-core.md`, #161).
//!
//! - **Input** (`chr-sdi-in --source decklink`): `decklinkvideosrc` with
//!   `mode=auto` takes the format (extent, rate, scan) from the signal, and a
//!   changed signal renegotiates its caps: a new stream, as a test pattern's
//!   switch is. `drop-no-signal-frames` makes a lost signal an input loss
//!   (no frames) rather than black frames. Timecode (RP 188, any) and captions
//!   come as GStreamer metas.
//! - **Output** (`chr-server --output decklink`, with a fixed output rate):
//!   `decklinkvideosink` in the mode of the program's format, which does not
//!   change while it plays (the sink refuses a reconfiguration), and its clock
//!   (the card's, genlocked to its reference input) becomes the core's
//!   pipeline clock; the core primes it ([`super::select::prime`]), hands
//!   it each frame [`super::CARD_LEAD`] before air and copies memory of
//!   other processes for it ([`own_memory`]).
//!
//! Only HD and UHD modes are mapped: SD's caps carry a non-square pixel aspect
//! ratio and BT.601, which chr's 1:1, BT.709 frames do not negotiate with.

use gstreamer::{self as gst, prelude::*};

use super::Error;
use crate::sdi::{FieldOrder, Format, Scan, VideoMode};
use chr_common::rate::Rate;

/// A DeckLink mode: `decklinkvideosrc`/`decklinkvideosink`'s `mode` nick and
/// its format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub nick: &'static str,
    pub format: Format,
}

/// The modes chr maps: HD and UHD broadcast formats. Interlaced modes are
/// named by their field rate and run at half of it in frames (`1080i50` is 25
/// frames per second), as GStreamer's caps carry them.
const MODES: &[(&str, u32, u32, u32, u32, Scan)] = {
    use FieldOrder::TopFirst;
    use Scan::{Interlaced, Progressive};
    &[
        ("1080p2398", 1920, 1080, 24000, 1001, Progressive),
        ("1080p24", 1920, 1080, 24, 1, Progressive),
        ("1080p25", 1920, 1080, 25, 1, Progressive),
        ("1080p2997", 1920, 1080, 30000, 1001, Progressive),
        ("1080p30", 1920, 1080, 30, 1, Progressive),
        ("1080i50", 1920, 1080, 25, 1, Interlaced(TopFirst)),
        ("1080i5994", 1920, 1080, 30000, 1001, Interlaced(TopFirst)),
        ("1080i60", 1920, 1080, 30, 1, Interlaced(TopFirst)),
        ("1080p50", 1920, 1080, 50, 1, Progressive),
        ("1080p5994", 1920, 1080, 60000, 1001, Progressive),
        ("1080p60", 1920, 1080, 60, 1, Progressive),
        ("720p50", 1280, 720, 50, 1, Progressive),
        ("720p5994", 1280, 720, 60000, 1001, Progressive),
        ("720p60", 1280, 720, 60, 1, Progressive),
        ("2160p2398", 3840, 2160, 24000, 1001, Progressive),
        ("2160p24", 3840, 2160, 24, 1, Progressive),
        ("2160p25", 3840, 2160, 25, 1, Progressive),
        ("2160p2997", 3840, 2160, 30000, 1001, Progressive),
        ("2160p30", 3840, 2160, 30, 1, Progressive),
        ("2160p50", 3840, 2160, 50, 1, Progressive),
        ("2160p5994", 3840, 2160, 60000, 1001, Progressive),
        ("2160p60", 3840, 2160, 60, 1, Progressive),
    ]
};

fn modes() -> impl Iterator<Item = Mode> {
    MODES
        .iter()
        .map(|&(nick, width, height, num, den, scan)| Mode {
            nick,
            format: Format {
                mode: VideoMode {
                    width,
                    height,
                    fps: Rate::new(num, den).expect("a valid mode rate"),
                },
                scan,
            },
        })
}

impl Mode {
    /// The mode that carries `format`.
    pub fn of(format: Format) -> Result<Self, Error> {
        modes().find(|mode| mode.format == format).ok_or_else(|| {
            format!(
                "no DeckLink mode carries {}; the modes: {}",
                format.name(),
                modes().map(|mode| mode.nick).collect::<Vec<_>>().join(", ")
            )
            .into()
        })
    }
}

impl std::str::FromStr for Mode {
    type Err = Error;

    /// A mode nick such as `1080i50` or `1080p5994`.
    fn from_str(nick: &str) -> Result<Self, Error> {
        modes().find(|mode| mode.nick == nick).ok_or_else(|| {
            format!(
                "unknown DeckLink mode {nick:?}; the modes: auto, {}",
                modes().map(|mode| mode.nick).collect::<Vec<_>>().join(", ")
            )
            .into()
        })
    }
}

/// The input connector of `decklinkvideosrc`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Connection {
    Auto,
    #[default]
    Sdi,
    Hdmi,
    OpticalSdi,
}

impl Connection {
    fn nick(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Sdi => "sdi",
            Self::Hdmi => "hdmi",
            Self::OpticalSdi => "optical-sdi",
        }
    }
}

impl std::str::FromStr for Connection {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Error> {
        [Self::Auto, Self::Sdi, Self::Hdmi, Self::OpticalSdi]
            .into_iter()
            .find(|connection| connection.nick() == text)
            .ok_or_else(|| {
                format!("the connection is auto, sdi, hdmi or optical-sdi, not {text:?}").into()
            })
    }
}

/// A DeckLink input: its device, mode (`None`: from the signal) and connector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input {
    pub device: u32,
    pub mode: Option<Mode>,
    pub connection: Connection,
}

/// DeckLink devices the driver reports.
pub fn devices() -> usize {
    gst::DeviceProviderFactory::by_name("decklinkdeviceprovider").map_or(0, |provider| {
        provider
            .devices()
            .iter()
            .filter(|device| device.has_classes("Video/Source") || device.has_classes("Video/Sink"))
            .count()
    })
}

fn element(factory: &str) -> Result<gst::Element, Error> {
    gst::ElementFactory::make(factory).build().map_err(|_| {
        format!(
            "GStreamer has no {factory}: the decklink plugin of gst-plugins-bad is missing \
             (the Nix shells provide it)"
        )
        .into()
    })
}

/// Opens `element` to see that its device is there: the card's driver opens
/// it on the way to READY.
fn open(element: &gst::Element, device: u32) -> Result<(), Error> {
    if element.set_state(gst::State::Ready).is_ok() {
        return Ok(());
    }
    let _ = element.set_state(gst::State::Null);
    Err(format!(
        "no DeckLink device {device} ({} found): install Blackmagic Desktop Video (its \
         driver and libDeckLinkAPI.so) and check the card with `BlackmagicFirmwareUpdater \
         status` (docs/operations.md)",
        devices()
    )
    .into())
}

/// `decklinkvideosrc` for `input`, opened. Frames of a lost signal are
/// dropped; timecode and captions come as metas.
pub fn video_source(input: &Input) -> Result<gst::Element, Error> {
    let source = element("decklinkvideosrc")?;
    source.set_property("device-number", input.device as i32);
    match input.mode {
        None => {
            source.set_property_from_str("mode", "auto");
            source.set_property_from_str("video-format", "auto");
        }
        Some(mode) => {
            source.set_property_from_str("mode", mode.nick);
            source.set_property_from_str("video-format", "8bit-yuv");
        }
    }
    source.set_property_from_str("connection", input.connection.nick());
    source.set_property("drop-no-signal-frames", true);
    source.set_property_from_str("timecode-format", "rp188any");
    source.set_property("output-cc", true);
    open(&source, input.device)?;
    Ok(source)
}

/// `decklinkaudiosrc` of the same card: `channels` of embedded audio.
pub fn audio_source(device: u32, channels: u32) -> Result<gst::Element, Error> {
    let source = element("decklinkaudiosrc")?;
    source.set_property("device-number", device as i32);
    source.set_property_from_str("channels", &channels.to_string());
    Ok(source)
}

/// `decklinkvideosink` on `device` in the mode of `format`, opened: its clock
/// is the card's. Timecode goes out as RP 188 and captions on line 9.
pub fn video_sink(device: u32, format: Format) -> Result<gst::Element, Error> {
    let mode = Mode::of(format)?;
    let sink = element("decklinkvideosink")?;
    sink.set_property("device-number", device as i32);
    sink.set_property_from_str("mode", mode.nick);
    sink.set_property_from_str("video-format", "8bit-yuv");
    sink.set_property_from_str("timecode-format", "rp188any");
    sink.set_property("cc-line", 9_i32);
    open(&sink, device)?;
    own_memory(&sink)?;
    Ok(sink)
}

/// `decklinkaudiosink` of the same card.
pub fn audio_sink(device: u32) -> Result<gst::Element, Error> {
    let sink = element("decklinkaudiosink")?;
    sink.set_property("device-number", device as i32);
    own_memory(&sink)?;
    Ok(sink)
}

/// Hands `sink` only memory of this process. The card's driver reads a
/// buffer's memory itself (`decklinkvideosink` wraps a frame without copying)
/// and refuses to schedule memory mapped read-only from another process's
/// memfd (`Failed to schedule frame: 0x80000008`), which a render worker's
/// frames and the input's audio are; the clean feed and the fills are the
/// core's own. Such buffers are copied on their way in.
fn own_memory(sink: &gst::Element) -> Result<(), Error> {
    sink.static_pad("sink")
        .ok_or("a DeckLink sink without a sink pad")?
        .add_probe(gst::PadProbeType::BUFFER, |_, info| {
            if let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data
                && let Some(copy) = copied(buffer)
            {
                *buffer = copy;
            }
            gst::PadProbeReturn::Ok
        });
    Ok(())
}

/// A deep copy of `buffer` (its memory is copied into system memory, its
/// times and metas kept), if it has fd-backed memory.
fn copied(buffer: &gst::BufferRef) -> Option<gst::Buffer> {
    buffer
        .iter_memories()
        .any(|memory| memory.is_memory_type::<gstreamer_allocators::FdMemory>())
        .then(|| buffer.copy_deep().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(width: u32, height: u32, fps: Rate, scan: Scan) -> Format {
        Format {
            mode: VideoMode { width, height, fps },
            scan,
        }
    }

    #[test]
    fn modes_map_formats_with_frame_rates_and_field_order() {
        let tff = Scan::Interlaced(FieldOrder::TopFirst);
        let ntsc = Rate::new(30000, 1001).unwrap();
        let i50 = format(1920, 1080, Rate::fps(25), tff);
        assert_eq!(Mode::of(i50).unwrap().nick, "1080i50");
        assert_eq!(i50.name(), "1920x1080i50");
        let i5994 = format(1920, 1080, ntsc, tff);
        assert_eq!(Mode::of(i5994).unwrap().nick, "1080i5994");
        assert_eq!(i5994.name(), "1920x1080i59.94");
        assert_eq!(
            Mode::of(format(
                1920,
                1080,
                Rate::new(60000, 1001).unwrap(),
                Scan::Progressive
            ))
            .unwrap()
            .nick,
            "1080p5994"
        );
        // SD (non-square pixels, BT.601) is not mapped.
        assert!("ntsc".parse::<Mode>().is_err() && "pal".parse::<Mode>().is_err());
        assert!(Mode::of(format(720, 576, Rate::fps(25), tff)).is_err());
        for mode in modes() {
            assert_eq!(Mode::of(mode.format).unwrap(), mode, "{}", mode.nick);
            assert_eq!(mode.nick.parse::<Mode>().unwrap(), mode);
        }
        // 1080p25 and 1080i50 have the same frame rate and differ in scan only.
        assert_eq!(
            Mode::of(format(1920, 1080, Rate::fps(25), Scan::Progressive))
                .unwrap()
                .nick,
            "1080p25"
        );
        assert!(
            Mode::of(format(1920, 1080, Rate::fps(50), tff)).is_err(),
            "no 1080i100"
        );
        assert!(Mode::of(format(1280, 720, Rate::fps(25), Scan::Progressive)).is_err());
        assert!("1080i49".parse::<Mode>().is_err());
        assert_eq!("sdi".parse::<Connection>().unwrap(), Connection::Sdi);
        assert!("dvi".parse::<Connection>().is_err());
    }

    /// The program's caps in every mapped mode are accepted by
    /// `decklinkvideosink` (its pad template, no card needed). Accepting caps
    /// takes a subset of the sink's, not just an intersection: the sink names
    /// `chroma-site` in every mode, so caps without it fail on a card.
    #[test]
    fn program_caps_negotiate_with_the_sink_in_every_mode() {
        gst::init().unwrap();
        let Some(factory) = gst::ElementFactory::find("decklinkvideosink") else {
            eprintln!("skipped: no decklink plugin");
            return;
        };
        let template = factory
            .static_pad_templates()
            .into_iter()
            .find(|template| template.direction() == gst::PadDirection::Sink)
            .unwrap()
            .caps();
        for mode in modes() {
            let caps = super::super::uyvy_caps(mode.format);
            assert!(caps.is_subset(&template), "{}: {caps}", mode.nick);
        }
        // SD would not: its pixels are not square.
        let sd = super::super::uyvy_caps(format(
            720,
            576,
            Rate::fps(25),
            Scan::Interlaced(FieldOrder::TopFirst),
        ));
        assert!(!sd.can_intersect(&template), "{sd}");
    }

    /// A worker's frame (memfd memory) reaches the card as a copy in this
    /// process's memory, with its time and video meta; the core's own frames
    /// pass as they are.
    #[test]
    fn fd_memory_is_copied_for_the_card() {
        use gstreamer_video::VideoMeta;
        gst::init().unwrap();
        let allocator = gstreamer_allocators::ShmAllocator::get().unwrap();
        let mut memory = allocator.alloc(16, None).unwrap();
        memory
            .get_mut()
            .unwrap()
            .map_writable()
            .unwrap()
            .copy_from_slice(&[7; 16]);
        let mut frame = gst::Buffer::new();
        {
            let frame = frame.get_mut().unwrap();
            frame.append_memory(memory);
            frame.set_pts(gst::ClockTime::from_mseconds(40));
            VideoMeta::add(
                frame,
                gstreamer_video::VideoFrameFlags::empty(),
                gstreamer_video::VideoFormat::Uyvy,
                4,
                2,
            )
            .unwrap();
        }
        let copy = copied(&frame).expect("fd memory is copied");
        assert!(
            !copy
                .iter_memories()
                .any(|memory| memory.is_memory_type::<gstreamer_allocators::FdMemory>())
        );
        assert_eq!(copy.map_readable().unwrap().as_slice(), &[7; 16]);
        assert_eq!(copy.pts(), Some(gst::ClockTime::from_mseconds(40)));
        assert!(copy.meta::<VideoMeta>().is_some());
        assert!(copied(&gst::Buffer::from_mut_slice(vec![0_u8; 16])).is_none());
    }

    /// The development host has no DeckLink card: the elements exist, and
    /// opening one fails with a clear error.
    #[test]
    fn without_a_card_the_elements_fail_to_open_with_a_clear_error() {
        gst::init().unwrap();
        if gst::ElementFactory::find("decklinkvideosink").is_none() || devices() > 0 {
            eprintln!("skipped: no decklink plugin, or a card is present");
            return;
        }
        let i50 = format(
            1920,
            1080,
            Rate::fps(25),
            Scan::Interlaced(FieldOrder::TopFirst),
        );
        let error = video_sink(0, i50).unwrap_err().to_string();
        assert!(error.contains("no DeckLink device 0"), "{error}");
        let error = video_source(&Input {
            device: 1,
            mode: None,
            connection: Connection::Sdi,
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("no DeckLink device 1"), "{error}");
        assert!(audio_source(0, 8).is_ok() && audio_sink(0).is_ok());
    }
}
