//! Blackmagic DeckLink input and output (#154 phase 3): GStreamer's `decklink`
//! elements (gst-plugins-bad, on the Desktop Video driver's
//! `libDeckLinkAPI.so`).
//!
//! **Not verified on hardware.** The development host has no DeckLink card:
//! the code below builds the elements, maps formats to the cards' modes and
//! fails with a clear error without a card, and that much is tested; capture,
//! playout, genlock, the card clock, timecode, captions and audio on a real
//! card are not (`server/docs/gst-core.md`).
//!
//! - **Input** (`chr-sdi-in --source decklink`): `decklinkvideosrc` with
//!   `mode=auto` takes the format (extent, rate, scan) from the signal, and a
//!   changed signal renegotiates its caps: a new stream, as a test pattern's
//!   switch is. `drop-no-signal-frames` makes a lost signal an input loss
//!   (no frames) rather than black frames. Timecode (RP 188, any) and captions
//!   come as GStreamer metas.
//! - **Output** (`chr-server --output decklink`): `decklinkvideosink` in the
//!   mode of the program's format, switched when the program's format changes,
//!   and its clock (the card's, genlocked to its reference input) becomes the
//!   core's pipeline clock.

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

/// The modes chr maps: SD, HD and UHD broadcast formats. Interlaced modes are
/// named by their field rate and run at half of it in frames (`1080i50` is 25
/// frames per second), as GStreamer's caps carry them.
const MODES: &[(&str, u32, u32, u32, u32, Scan)] = {
    use FieldOrder::{BottomFirst, TopFirst};
    use Scan::{Interlaced, Progressive};
    &[
        ("ntsc", 720, 486, 30000, 1001, Interlaced(BottomFirst)),
        ("pal", 720, 576, 25, 1, Interlaced(TopFirst)),
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
    Ok(sink)
}

/// `decklinkaudiosink` of the same card.
pub fn audio_sink(device: u32) -> Result<gst::Element, Error> {
    let sink = element("decklinkaudiosink")?;
    sink.set_property("device-number", device as i32);
    Ok(sink)
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
        assert_eq!(
            "ntsc".parse::<Mode>().unwrap().format,
            format(720, 486, ntsc, Scan::Interlaced(FieldOrder::BottomFirst))
        );
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
