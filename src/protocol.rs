//! Local socket protocol: one JSON request line, one JSON response line.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(default)]
    pub protocol: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            protocol: PROTOCOL_VERSION,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, error: ApiError) -> Self {
        Self {
            id,
            protocol: PROTOCOL_VERSION,
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn responses_carry_the_protocol_version() {
        let response = Response::ok(1, json!({"ok": true}));
        assert_eq!(response.protocol, PROTOCOL_VERSION);
        let encoded = serde_json::to_string(&response).expect("encode");
        assert!(encoded.contains(&format!("\"protocol\":{PROTOCOL_VERSION}")));
    }

    #[test]
    fn old_responses_default_to_protocol_zero() {
        let response: Response =
            serde_json::from_value(json!({"id": 1, "ok": true, "result": {}})).expect("decode");
        assert_eq!(response.protocol, 0);
        assert_ne!(response.protocol, PROTOCOL_VERSION);
    }
}
