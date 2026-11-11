use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::{core::required_nullable, *};
use crate::jsonrpc::ErrorObject;

/// Decodes exact payloads and checks cross-field metadata, never native math or resources.
pub fn parse_payload<T: DeserializeOwned + Validate>(value: Value) -> Result<T, ErrorObject> {
    let value: T = serde_json::from_value(value)
        .map_err(|error| ErrorObject::invalid_params(error.to_string()))?;
    value
        .validate()
        .map_err(|error| ErrorObject::invalid_params(error.to_string()))?;
    Ok(value)
}

macro_rules! methods {
    ($($constant:ident, $variant:ident, $method:literal, $params:ty, $result:ty);* $(;)?) => {
        $(pub const $constant: &str = $method;)*

        /// A parsed method invocation, not another wire envelope.
        #[derive(Debug, Clone, PartialEq, Serialize)]
        #[serde(untagged)]
        pub enum Params { $($variant(Box<$params>)),* }

        /// Both IPC transports should call this from the same handler.
        /// Unknown methods remain available to the handler's other namespaces.
        pub fn parse_params(method: &str, params: Option<Value>) -> Result<Params, ErrorObject> {
            match method {
                $($method => parse_payload(params.ok_or_else(|| ErrorObject::invalid_params("params are required"))?)
                    .map(Box::new).map(Params::$variant),)*
                _ => Err(ErrorObject::method_not_found(method)),
            }
        }

        /// Validates method-specific results without adding a success wrapper.
        pub fn validate_result(method: &str, result: Value) -> Result<(), ErrorObject> {
            match method {
                $($method => parse_payload::<$result>(result).map(|_| ()),)*
                _ => Err(ErrorObject::method_not_found(method)),
            }
        }

        pub const METHODS: &[&str] = &[$($method),*];
    };
}

methods! {
    GET_CONFIGURATION, GetConfiguration, "render.get_configuration", RuntimeTarget, Configuration;
    SCENE_CREATE, SceneCreate, "scene.create", SceneCreate, SceneApplied;
    SCENE_GET, SceneGet, "scene.get", Target, SceneSnapshot;
    SCENE_MUTATE, SceneMutate, "scene.mutate", SceneMutate, SceneApplied;
    SCENE_RELEASE, SceneRelease, "scene.release", Target, ReleaseResult;
    FRAME_PUBLISH, FramePublish, "frame.publish", FramePublish, PublishedFrame;
    FRAME_RELEASE, FrameRelease, "frame.release", ImageTarget, ReleaseResult;
    RENDER_SUBMIT, RenderSubmit, "render.submit", Submit, SubmissionAccepted;
    RENDER_GET, RenderGet, "render.get", SubmissionTarget, SubmissionSnapshot;
    RENDER_CANCEL, RenderCancel, "render.cancel", SubmissionTarget, CancelResult;
    RENDER_RELEASE, RenderRelease, "render.release", SubmissionTarget, ReleaseResult;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionKind {
    RuntimeChanged,
    NotFound,
    Conflict,
    IncompatibleInput,
    Capacity,
    InvalidState,
    UnsupportedProfile,
}

impl RejectionKind {
    pub fn code(self) -> i32 {
        match self {
            Self::RuntimeChanged => 2001,
            Self::NotFound => 2002,
            Self::Conflict => 2003,
            Self::IncompatibleInput => 2004,
            Self::Capacity => 2005,
            Self::InvalidState => 2006,
            Self::UnsupportedProfile => 2007,
        }
    }
}

/// Code and kind cannot disagree; serialized shape is the existing RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RejectionFields", into = "RejectionFields")]
pub struct Rejection {
    pub kind: RejectionKind,
    pub message: String,
    pub frame: Option<FrameKey>,
}

object!(@private RejectionFields {
    #[serde(deserialize_with = "rejection_code")]
    code: i32,
    message: String,
    data: RejectionData,
});

fn rejection_code<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i32, D::Error> {
    let number = super::core::integer(deserializer)?;
    i32::try_from(number).map_err(serde::de::Error::custom)
}

object!(@private RejectionData {
    kind: RejectionKind,
    #[serde(deserialize_with = "required_nullable")]
    frame: Option<FrameKey>,
});

impl TryFrom<RejectionFields> for Rejection {
    type Error = ValidationError;
    fn try_from(value: RejectionFields) -> Result<Self, Self::Error> {
        if value.code != value.data.kind.code() {
            return Err(ValidationError("rejection code/kind mismatch"));
        }
        Ok(Self {
            kind: value.data.kind,
            message: value.message,
            frame: value.data.frame,
        })
    }
}

impl From<Rejection> for RejectionFields {
    fn from(value: Rejection) -> Self {
        Self {
            code: value.kind.code(),
            message: value.message,
            data: RejectionData {
                kind: value.kind,
                frame: value.frame,
            },
        }
    }
}

impl From<Rejection> for ErrorObject {
    fn from(value: Rejection) -> Self {
        Self {
            code: value.kind.code(),
            message: value.message,
            data: Some(serde_json::json!({ "kind": value.kind, "frame": value.frame })),
        }
    }
}
