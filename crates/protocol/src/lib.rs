//! Versioned JSON-RPC 2.0 control protocol and externally visible data types.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// The only control protocol version supported by this release.
pub const PROTOCOL_VERSION: u16 = 1;
/// Maximum compact JSON request size, including its newline delimiter.
pub const MAXIMUM_MESSAGE_BYTES: usize = 1024 * 1024;
/// Maximum JSON object/array nesting accepted from clients.
pub const MAXIMUM_NESTING_DEPTH: usize = 32;

/// JSON-RPC request identifier.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

/// One newline-framed JSON-RPC request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// One JSON-RPC success or error response.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    #[must_use]
    pub fn success(id: RequestId, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    #[must_use]
    pub fn failure(id: RequestId, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Structured JSON-RPC failure.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    #[must_use]
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// Required first-call parameters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HandshakeParams {
    pub protocol: u16,
    pub client_name: String,
    pub client_version: String,
}

/// Successful protocol negotiation result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HandshakeResult {
    pub protocol: u16,
    pub daemon_version: String,
    pub daemon_instance: Uuid,
}

/// Daemon status available to all compatible clients.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Status {
    pub daemon_instance: Uuid,
    pub capture_active: bool,
    pub selected_source: Option<String>,
    pub connected_clients: usize,
    pub active_profile: Option<String>,
    pub input_fps: f32,
    pub analysis_fps: f32,
    pub replaced_frames: u64,
    pub output_error: Option<String>,
}

/// Server notification, sent without a request identifier.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

/// Returns whether a JSON value respects the protocol nesting limit.
#[must_use]
pub fn nesting_within_limit(value: &Value, maximum: usize) -> bool {
    fn depth(value: &Value, current: usize, maximum: usize) -> bool {
        if current > maximum {
            return false;
        }
        match value {
            Value::Array(values) => values
                .iter()
                .all(|value| depth(value, current + 1, maximum)),
            Value::Object(values) => values
                .values()
                .all(|value| depth(value, current + 1, maximum)),
            _ => true,
        }
    }
    depth(value, 0, maximum)
}

pub mod method {
    pub const HANDSHAKE: &str = "system.handshake";
    pub const VERSION: &str = "system.version";
    pub const CAPABILITIES: &str = "system.capabilities";
    pub const STATUS: &str = "system.status";
    pub const SHUTDOWN: &str = "system.shutdown";
    pub const PROFILE_LIST: &str = "profile.list";
    pub const PROFILE_GET: &str = "profile.get";
    pub const PROFILE_CREATE: &str = "profile.create";
    pub const PROFILE_COMMIT: &str = "profile.commit";
    pub const PROFILE_DUPLICATE: &str = "profile.duplicate";
    pub const PROFILE_VALIDATE: &str = "profile.validate";
    pub const PROFILE_IMPORT: &str = "profile.import";
    pub const PROFILE_EXPORT: &str = "profile.export";
    pub const PROFILE_TRASH: &str = "profile.trash";
    pub const PROFILE_RESTORE: &str = "profile.restore";
    pub const PROFILE_ACTIVATE: &str = "profile.activate";
    pub const STATE_GET: &str = "state.get";
    pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";
    pub const STATUS_SUBSCRIBE: &str = "status.subscribe";
    pub const REPLAY_SYNTHETIC_HEALTH: &str = "replay.synthetic_health";
    pub const DETECTOR_TEST: &str = "detector.test";
    pub const REPLAY_PROFILE_DETECTOR: &str = "replay.profile_detector";
}

pub mod error_code {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
    pub const HANDSHAKE_REQUIRED: i32 = -32001;
    pub const INCOMPATIBLE_VERSION: i32 = -32002;
    pub const REVISION_CONFLICT: i32 = -32009;
    pub const SUBSCRIPTION_LAGGED: i32 = -32010;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_shape_is_stable_compact_json() {
        let response = Response::success(RequestId::Number(7), serde_json::json!({"protocol": 1}));
        assert_eq!(
            serde_json::to_string(&response).unwrap(),
            r#"{"jsonrpc":"2.0","id":7,"result":{"protocol":1}}"#
        );
    }

    #[test]
    fn nesting_limit_rejects_deep_input() {
        let value = serde_json::json!({"a": {"b": {"c": true}}});
        assert!(nesting_within_limit(&value, 3));
        assert!(!nesting_within_limit(&value, 2));
    }
}
