//! JSON-RPC 2.0 message envelope.
//!
//! Requests are validated from raw JSON values rather than derived, so malformed input can
//! still be answered with the correct standard error and, where possible, the caller's id.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only protocol version accepted or produced.
pub const VERSION: &str = "2.0";

/// Invalid JSON was received.
pub const PARSE_ERROR: i32 = -32700;
/// The JSON sent is not a valid request object.
pub const INVALID_REQUEST: i32 = -32600;
/// The method does not exist or is not available.
pub const METHOD_NOT_FOUND: i32 = -32601;
/// Invalid method parameters.
pub const INVALID_PARAMS: i32 = -32602;
/// Internal JSON-RPC error.
pub const INTERNAL_ERROR: i32 = -32603;
/// The server cannot accept the command right now, for example because a bounded queue is
/// full. Implementation-defined server error range (-32000 to -32099).
pub const SERVER_BUSY: i32 = -32000;
/// The component that executes the command is not running.
pub const SERVER_UNAVAILABLE: i32 = -32001;

/// Request identifier. Clients should prefer strings; numbers are accepted for compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    String(String),
    Number(serde_json::Number),
    Null,
}

impl From<&str> for Id {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

/// A validated request. `id == None` marks a notification, which never gets a response.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Id>,
}

impl Request {
    /// Serializes this request as a JSON-RPC 2.0 object.
    pub fn to_value(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("jsonrpc".into(), VERSION.into());
        object.insert("method".into(), self.method.clone().into());
        if let Some(params) = &self.params {
            object.insert("params".into(), params.clone());
        }
        if let Some(id) = &self.id {
            object.insert(
                "id".into(),
                serde_json::to_value(id).expect("ids are plain JSON values"),
            );
        }
        Value::Object(object)
    }
}

/// The standard error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ErrorObject {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(METHOD_NOT_FOUND, format!("method not found: {method}"))
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(INTERNAL_ERROR, message)
    }
}

impl std::fmt::Display for ErrorObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for ErrorObject {}

/// A response carries exactly one of `result` or `error`.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub id: Id,
    pub outcome: Result<Value, ErrorObject>,
}

impl Response {
    pub fn to_value(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("jsonrpc".into(), VERSION.into());
        match &self.outcome {
            Ok(result) => {
                object.insert("result".into(), result.clone());
            }
            Err(error) => {
                object.insert(
                    "error".into(),
                    serde_json::to_value(error).expect("error objects are plain JSON values"),
                );
            }
        }
        object.insert(
            "id".into(),
            serde_json::to_value(&self.id).expect("ids are plain JSON values"),
        );
        Value::Object(object)
    }

    /// Parses a single response object, as received by a client.
    pub fn from_value(value: Value) -> Result<Self, ErrorObject> {
        let invalid = |message: &str| ErrorObject::new(INVALID_REQUEST, message);
        let Value::Object(mut object) = value else {
            return Err(invalid("response must be an object"));
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some(VERSION) {
            return Err(invalid("response must have jsonrpc \"2.0\""));
        }
        let id = serde_json::from_value(object.remove("id").unwrap_or(Value::Null))
            .map_err(|_| invalid("response id must be a string, number, or null"))?;
        let outcome = match (object.remove("result"), object.remove("error")) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) => Err(serde_json::from_value(error)
                .map_err(|_| invalid("response error must be an error object"))?),
            _ => return Err(invalid("response must have exactly one of result or error")),
        };
        Ok(Self { id, outcome })
    }
}

/// One element of an incoming message after validation.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Request(Request),
    /// Not a valid request. Answered with this error and the id, if one could be read.
    Invalid {
        id: Id,
        error: ErrorObject,
    },
}

/// A parsed message: a single element or a batch.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Single(Incoming),
    Batch(Vec<Incoming>),
}

/// Parses raw message bytes. Malformed JSON, an empty batch, or an invalid element becomes
/// an [`Incoming::Invalid`] so the caller can still answer as the specification requires.
pub fn parse_message(bytes: &[u8]) -> Message {
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(error) => {
            return Message::Single(Incoming::Invalid {
                id: Id::Null,
                error: ErrorObject::new(PARSE_ERROR, format!("parse error: {error}")),
            });
        }
    };
    match value {
        Value::Array(items) if items.is_empty() => Message::Single(Incoming::Invalid {
            id: Id::Null,
            error: ErrorObject::new(INVALID_REQUEST, "empty batch"),
        }),
        Value::Array(items) => Message::Batch(items.into_iter().map(validate_request).collect()),
        value => Message::Single(validate_request(value)),
    }
}

fn validate_request(value: Value) -> Incoming {
    let Value::Object(mut object) = value else {
        return Incoming::Invalid {
            id: Id::Null,
            error: ErrorObject::new(INVALID_REQUEST, "request must be an object"),
        };
    };
    let id = match object.remove("id") {
        None => None,
        Some(raw) => match serde_json::from_value::<Id>(raw) {
            Ok(id) => Some(id),
            Err(_) => {
                return Incoming::Invalid {
                    id: Id::Null,
                    error: ErrorObject::new(
                        INVALID_REQUEST,
                        "id must be a string, number, or null",
                    ),
                };
            }
        },
    };
    let invalid = |message: &str| Incoming::Invalid {
        id: id.clone().unwrap_or(Id::Null),
        error: ErrorObject::new(INVALID_REQUEST, message),
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some(VERSION) {
        return invalid("jsonrpc must be \"2.0\"");
    }
    let method = match object.remove("method") {
        Some(Value::String(method)) => method,
        _ => return invalid("method must be a string"),
    };
    if method.starts_with("rpc.") {
        return Incoming::Invalid {
            id: id.unwrap_or(Id::Null),
            error: ErrorObject::method_not_found(&method),
        };
    }
    let params = match object.remove("params") {
        None => None,
        Some(params @ (Value::Object(_) | Value::Array(_))) => Some(params),
        Some(_) => return invalid("params must be an object or array"),
    };
    Incoming::Request(Request { method, params, id })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn single(bytes: &[u8]) -> Incoming {
        match parse_message(bytes) {
            Message::Single(incoming) => incoming,
            Message::Batch(_) => panic!("expected a single message"),
        }
    }

    #[test]
    fn parses_request_and_notification() {
        let request = single(br#"{"jsonrpc":"2.0","method":"a.b","params":{"x":1},"id":"r1"}"#);
        assert_eq!(
            request,
            Incoming::Request(Request {
                method: "a.b".into(),
                params: Some(json!({"x": 1})),
                id: Some(Id::from("r1")),
            })
        );
        let notification = single(br#"{"jsonrpc":"2.0","method":"a.b"}"#);
        assert!(matches!(
            notification,
            Incoming::Request(Request { id: None, .. })
        ));
    }

    #[test]
    fn reports_standard_errors() {
        let Incoming::Invalid { id, error } = single(b"{not json") else {
            panic!("expected parse error");
        };
        assert_eq!((id, error.code), (Id::Null, PARSE_ERROR));

        let Incoming::Invalid { id, error } = single(br#"{"jsonrpc":"1.0","method":"a","id":7}"#)
        else {
            panic!("expected invalid request");
        };
        assert_eq!(id, Id::Number(7.into()));
        assert_eq!(error.code, INVALID_REQUEST);

        let Incoming::Invalid { error, .. } =
            single(br#"{"jsonrpc":"2.0","method":"a","params":3,"id":"x"}"#)
        else {
            panic!("expected invalid params shape");
        };
        assert_eq!(error.code, INVALID_REQUEST);

        let Incoming::Invalid { error, .. } =
            single(br#"{"jsonrpc":"2.0","method":"rpc.discover","id":"x"}"#)
        else {
            panic!("rpc. methods are reserved");
        };
        assert_eq!(error.code, METHOD_NOT_FOUND);

        assert!(matches!(
            single(b"[]"),
            Incoming::Invalid {
                error: ErrorObject {
                    code: INVALID_REQUEST,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn batch_keeps_invalid_elements() {
        let Message::Batch(items) = parse_message(br#"[{"jsonrpc":"2.0","method":"a","id":1},1]"#)
        else {
            panic!("expected batch");
        };
        assert!(matches!(items[0], Incoming::Request(_)));
        assert!(matches!(items[1], Incoming::Invalid { .. }));
    }

    #[test]
    fn response_round_trips() {
        let ok = Response {
            id: Id::from("a"),
            outcome: Ok(json!({"rotating": true})),
        };
        assert_eq!(
            ok.to_value(),
            json!({"jsonrpc": "2.0", "result": {"rotating": true}, "id": "a"})
        );
        assert_eq!(Response::from_value(ok.to_value()).unwrap(), ok);

        let error = Response {
            id: Id::Null,
            outcome: Err(ErrorObject::new(PARSE_ERROR, "bad")),
        };
        assert_eq!(
            error.to_value(),
            json!({"jsonrpc": "2.0", "error": {"code": -32700, "message": "bad"}, "id": null})
        );
        assert_eq!(Response::from_value(error.to_value()).unwrap(), error);
    }
}
