//! JSON-RPC 2.0 messages used on the bus.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
pub const PROVIDER_UNAVAILABLE: i64 = -32001;
pub const VERSION_MISMATCH: i64 = -32002;
pub const FORBIDDEN: i64 = -32003;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Request {
    #[must_use]
    pub fn new(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id: Some(Value::from(id)),
            method: method.into(),
            params,
        }
    }
}

impl Response {
    #[must_use]
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    #[must_use]
    pub fn err(id: Option<Value>, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

pub fn parse_request(text: &str) -> Result<Request, RpcError> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        RpcError::new(INVALID_REQUEST, format!("request is not JSON: {error}"))
    })?;
    let object = value.as_object().ok_or_else(|| {
        RpcError::new(INVALID_REQUEST, "request is not a JSON object")
    })?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(RpcError::new(INVALID_REQUEST, "jsonrpc must be \"2.0\""));
    }
    if object.get("method").and_then(Value::as_str).is_none() {
        return Err(RpcError::new(INVALID_REQUEST, "method is required"));
    }
    serde_json::from_value(value)
        .map_err(|error| RpcError::new(INVALID_REQUEST, format!("request is invalid: {error}")))
}

pub fn encode(value: &impl Serialize) -> Result<String, RpcError> {
    serde_json::to_string(value)
        .map_err(|error| RpcError::new(INTERNAL_ERROR, format!("encode failed: {error}")))
}
