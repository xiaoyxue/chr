//! Frame rates as exact fractions: `50`, `25`, `30000/1001` (29.97), `60000/1001`
//! (59.94), `24000/1001` (23.976) and any other rational rate in range.
//!
//! Frame `n` of a stream starts `n * den * 1e9 / num` ns after frame 0
//! ([`Rate::frame_ns`]); timestamps computed that way never drift, unlike a sum of
//! rounded periods. [`Rate::period_ns`] is the nominal, rounded-down duration of one
//! frame.
//!
//! Text and JSON forms: an integer rate is a plain integer (`50`, as before
//! rates could be fractional), any other one the string `"num/den"`
//! (`"30000/1001"`). Parsing also takes `"N/D"` for integers and the
//! conventional names of the 1000/1001 rates: `23.976` or `23.98`, `29.97`,
//! `47.95`, `59.94` and `119.88`. Other decimals are refused: `29.97` names
//! `30000/1001`, never `2997/100`.

use std::{fmt, str::FromStr, time::Duration};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::scene_frame::TimeBase;

/// The highest rate accepted, frames per second.
pub const MAX_FPS: u32 = 120;

const NANOS: u128 = 1_000_000_000;

/// A frame rate of `num / den` frames per second, reduced, from 1 to [`MAX_FPS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rate {
    num: u32,
    den: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RateError(String);

/// The 1000/1001 rates by their conventional names.
const NAMED: [(&str, u32); 6] = [
    ("23.976", 24),
    ("23.98", 24),
    ("29.97", 30),
    ("47.95", 48),
    ("59.94", 60),
    ("119.88", 120),
];

impl Rate {
    /// `fps` frames per second; panics outside 1..=[`MAX_FPS`], so for constants.
    pub const fn fps(fps: u32) -> Self {
        assert!(fps >= 1 && fps <= MAX_FPS, "rate out of range");
        Self { num: fps, den: 1 }
    }

    /// `num / den` frames per second, reduced.
    pub fn new(num: u32, den: u32) -> Result<Self, RateError> {
        if num == 0 || den == 0 {
            return Err(RateError(format!(
                "a frame rate needs a positive numerator and denominator, not {num}/{den}"
            )));
        }
        let divisor = gcd(num, den);
        let (num, den) = (num / divisor, den / divisor);
        if num < den || u64::from(num) > u64::from(MAX_FPS) * u64::from(den) {
            return Err(RateError(format!(
                "frame rate {num}/{den} is outside 1-{MAX_FPS} fps"
            )));
        }
        // GStreamer fractions and time bases are 31-bit.
        if num > i32::MAX as u32 {
            return Err(RateError(format!(
                "frame rate {num}/{den} has terms above 2^31 - 1"
            )));
        }
        Ok(Self { num, den })
    }

    /// The rate of frames `time_base` seconds apart.
    pub fn from_time_base(time_base: TimeBase) -> Result<Self, RateError> {
        Self::new(time_base.denominator(), time_base.numerator())
    }

    /// Seconds per frame, as a timeline's time base.
    pub fn time_base(self) -> TimeBase {
        TimeBase::new(self.den, self.num).expect("a reduced 31-bit rate is a valid time base")
    }

    pub fn num(self) -> u32 {
        self.num
    }

    pub fn den(self) -> u32 {
        self.den
    }

    pub fn is_integer(self) -> bool {
        self.den == 1
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// Nanoseconds from the start of frame 0 to the start of frame `n`,
    /// rounded down: exact on average, never drifting.
    pub fn frame_ns(self, n: u64) -> u64 {
        let ns = u128::from(n) * u128::from(self.den) * NANOS / u128::from(self.num);
        u64::try_from(ns).unwrap_or(u64::MAX)
    }

    /// [`Self::frame_ns`] as a duration.
    pub fn frame_time(self, n: u64) -> Duration {
        Duration::from_nanos(self.frame_ns(n))
    }

    /// One frame, rounded down to the nanosecond.
    pub fn period_ns(self) -> u64 {
        self.frame_ns(1)
    }

    pub fn period(self) -> Duration {
        Duration::from_nanos(self.period_ns())
    }

    /// Frames that start within `span` of frame 0's start, frame 0 included
    /// when `span` is positive: `ceil(span * rate)`.
    pub fn frames_in(self, span: Duration) -> u64 {
        let frames =
            (span.as_nanos() * u128::from(self.num)).div_ceil(NANOS * u128::from(self.den));
        u64::try_from(frames).unwrap_or(u64::MAX)
    }

    /// The last frame starting at or before `ns` after frame 0: the inverse of
    /// [`Self::frame_ns`].
    pub fn frame_at(self, ns: u64) -> u64 {
        let frames = ((u128::from(ns) + 1) * u128::from(self.num))
            .div_ceil(NANOS * u128::from(self.den))
            - 1;
        u64::try_from(frames).unwrap_or(u64::MAX)
    }

    /// A short label for mode names: `50`, `29.97`, `23.98`.
    pub fn label(self) -> String {
        if self.is_integer() {
            return self.num.to_string();
        }
        let label = format!("{:.2}", self.as_f64());
        label.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_integer() {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

impl FromStr for Rate {
    type Err = RateError;

    fn from_str(text: &str) -> Result<Self, RateError> {
        let text = text.trim();
        let invalid = || {
            RateError(format!(
                "invalid frame rate {text:?}: give an integer such as 50, a fraction such as \
                 30000/1001, or 23.976, 29.97, 47.95, 59.94 or 119.88"
            ))
        };
        if let Some((_, fps)) = NAMED.iter().find(|(name, _)| *name == text) {
            return Self::new(fps * 1000, 1001);
        }
        let (num, den) = match text.split_once('/') {
            Some((num, den)) => (num, den),
            None => (text, "1"),
        };
        let digits = |part: &str| {
            (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| part.parse::<u32>().ok())
                .flatten()
        };
        Self::new(
            digits(num).ok_or_else(invalid)?,
            digits(den).ok_or_else(invalid)?,
        )
    }
}

impl Serialize for Rate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.is_integer() {
            serializer.serialize_u32(self.num)
        } else {
            serializer.collect_str(self)
        }
    }
}

impl<'de> Deserialize<'de> for Rate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = Rate;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a frame rate: an integer or a string such as \"30000/1001\"")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Rate, E> {
                let fps = u32::try_from(value).map_err(E::custom)?;
                Rate::new(fps, 1).map_err(E::custom)
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Rate, E> {
                let fps = u32::try_from(value).map_err(E::custom)?;
                Rate::new(fps, 1).map_err(E::custom)
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Rate, E> {
                value.parse().map_err(E::custom)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_integers_fractions_and_the_ntsc_names() {
        let ntsc = |fps: u32| Rate::new(fps * 1000, 1001).unwrap();
        for (text, rate) in [
            ("50", Rate::fps(50)),
            ("25/1", Rate::fps(25)),
            ("60000/1001", ntsc(60)),
            ("29.97", ntsc(30)),
            ("59.94", ntsc(60)),
            ("23.976", ntsc(24)),
            ("23.98", ntsc(24)),
            ("120000/2002", ntsc(60)),
            ("50/2", Rate::fps(25)),
            ("1", Rate::fps(1)),
            ("120", Rate::fps(120)),
        ] {
            assert_eq!(text.parse::<Rate>().unwrap(), rate, "{text}");
        }
        for bad in [
            "0", "121", "", "29.9", "30.0", "-25", "25/0", "0/1", "1/2", "a/b", "25 fps", "1/",
            "+5",
        ] {
            assert!(bad.parse::<Rate>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn formats_and_labels() {
        let rate: Rate = "59.94".parse().unwrap();
        assert_eq!((rate.num(), rate.den()), (60000, 1001));
        assert_eq!(rate.to_string(), "60000/1001");
        assert_eq!(rate.label(), "59.94");
        assert_eq!(Rate::new(24000, 1001).unwrap().label(), "23.98");
        assert_eq!(Rate::fps(50).to_string(), "50");
        assert_eq!(Rate::fps(50).label(), "50");
        assert_eq!(Rate::new(25, 2).unwrap().label(), "12.5");
        assert_eq!(Rate::from_time_base(rate.time_base()).unwrap(), rate);
        assert_eq!(
            (rate.time_base().numerator(), rate.time_base().denominator()),
            (1001, 60000)
        );
    }

    #[test]
    fn json_keeps_integers_and_takes_strings() {
        assert_eq!(serde_json::to_string(&Rate::fps(25)).unwrap(), "25");
        let ntsc = Rate::new(30000, 1001).unwrap();
        assert_eq!(serde_json::to_string(&ntsc).unwrap(), "\"30000/1001\"");
        assert_eq!(serde_json::from_str::<Rate>("25").unwrap(), Rate::fps(25));
        assert_eq!(serde_json::from_str::<Rate>("\"29.97\"").unwrap(), ntsc);
        assert_eq!(
            serde_json::from_str::<Rate>("\"30000/1001\"").unwrap(),
            ntsc
        );
        for bad in ["0", "-1", "25.0", "\"x\"", "null", "4294967296"] {
            assert!(serde_json::from_str::<Rate>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn frame_times_are_exact_and_never_drift() {
        let ntsc = Rate::new(30000, 1001).unwrap();
        assert_eq!(ntsc.period_ns(), 33_366_666);
        assert_eq!(ntsc.frame_ns(3), 100_100_000);
        // One hour at 29.97 holds 107 893 frame starts; the next one is
        // 107893 * 1001 / 30000 s after the first, to the nanosecond.
        let hour = ntsc.frames_in(Duration::from_secs(3600));
        assert_eq!(hour, 107_893);
        assert_eq!(ntsc.frame_ns(hour), 3_600_029_766_666);
        assert_eq!(ntsc.frame_ns(30_000), 1001 * 1_000_000_000);
        // A sum of rounded periods would be 72 µs early by then, and drift on.
        assert_eq!(ntsc.frame_ns(hour) - hour * ntsc.period_ns(), 71_928);
        // Each frame lasts its period or one nanosecond more.
        for n in 0..hour {
            let length = ntsc.frame_ns(n + 1) - ntsc.frame_ns(n);
            assert!(length == 33_366_666 || length == 33_366_667, "{n}");
        }
        assert_eq!(ntsc.frame_at(ntsc.frame_ns(1234)), 1234);
        assert_eq!(ntsc.frame_at(ntsc.frame_ns(1234) - 1), 1233);
        assert_eq!(ntsc.frame_at(0), 0);
        assert_eq!(Rate::fps(50).frame_at(19_999_999), 0);
        assert_eq!(Rate::fps(50).frame_at(20_000_000), 1);
        assert_eq!(Rate::fps(50).frames_in(Duration::from_millis(100)), 5);
        assert_eq!(Rate::fps(50).frames_in(Duration::from_millis(101)), 6);
        assert_eq!(Rate::fps(50).period(), Duration::from_millis(20));
    }
}
