//! Shared identity, logical time and finite scalar definitions.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ValidationError(pub &'static str);

pub trait Validate {
    fn validate(&self) -> Result<(), ValidationError>;
}

/// Nonempty opaque identity, unrelated to a JSON-RPC request ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);

impl TryFrom<String> for Id {
    type Error = ValidationError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            Err(ValidationError("ID must be nonempty"))
        } else {
            Ok(Self(value))
        }
    }
}

impl From<Id> for String {
    fn from(value: Id) -> Self {
        value.0
    }
}

impl Id {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Full-range unsigned integer encoded only as canonical decimal JSON text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct U64(u64);

impl From<u64> for U64 {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl U64 {
    pub fn get(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, rhs: u64) -> Option<Self> {
        self.0.checked_add(rhs).map(Self)
    }
}

impl Serialize for U64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for U64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.is_empty()
            || (text.len() > 1 && text.starts_with('0'))
            || !text.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(D::Error::custom(
                "U64 requires canonical unsigned decimal text",
            ));
        }
        text.parse::<u64>().map(Self).map_err(D::Error::custom)
    }
}

/// Prevents serializing native NaN/infinity as JSON null.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Finite(f64);

impl TryFrom<f64> for Finite {
    type Error = ValidationError;
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(ValidationError("number must be finite"))
        }
    }
}

impl From<Finite> for f64 {
    fn from(value: Finite) -> Self {
        value.0
    }
}

impl Finite {
    pub fn get(self) -> f64 {
        self.0
    }
}

pub type Vec3 = [Finite; 3];
pub type Vec4 = [Finite; 4];
/// Forward row-major matrix; native camera validation belongs to the runtime.
pub type Matrix3 = [Finite; 9];
/// Forward row-major transform acting on column vectors (metres, negative-Z up).
pub type Matrix4 = [Vec4; 4];

/// Explicit null is allowed; omission is not. Attach to every nullable field.
pub(super) fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameKey {
    pub stream_id: Id,
    pub sequence: U64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "TimeBaseFields", into = "TimeBaseFields")]
pub struct TimeBase {
    numerator: u32,
    denominator: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeBaseFields {
    numerator: u32,
    denominator: u32,
}

impl TimeBase {
    /// Seconds per tick, already reduced; does not normalize caller input.
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, ValidationError> {
        if numerator == 0
            || denominator == 0
            || numerator > i32::MAX as u32
            || denominator > i32::MAX as u32
        {
            return Err(ValidationError(
                "time base terms must be positive 31-bit integers",
            ));
        }
        let (mut a, mut b) = (numerator, denominator);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        if a != 1 {
            return Err(ValidationError("time base must be reduced"));
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    pub fn numerator(self) -> u32 {
        self.numerator
    }

    pub fn denominator(self) -> u32 {
        self.denominator
    }
}

impl TryFrom<TimeBaseFields> for TimeBase {
    type Error = ValidationError;
    fn try_from(value: TimeBaseFields) -> Result<Self, Self::Error> {
        Self::new(value.numerator, value.denominator)
    }
}

impl From<TimeBase> for TimeBaseFields {
    fn from(value: TimeBase) -> Self {
        Self {
            numerator: value.numerator,
            denominator: value.denominator,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    pub timeline_id: Id,
    pub time_base: TimeBase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub timeline_id: Id,
    pub ticks: U64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    pub runtime_id: Id,
    pub sequence: U64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    pub id: Id,
    pub revision: U64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extent {
    pub width: u32,
    pub height: u32,
}

impl Validate for Extent {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.width == 0 || self.height == 0 {
            Err(ValidationError("extent must be positive"))
        } else {
            Ok(())
        }
    }
}
