//! JSON-RPC 2.0 message types for the MCP stdio transport.
//!
//! MCP messages are line-delimited JSON on stdin/stdout — no Content-Length
//! framing. One message per line, no embedded newlines.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct Request {
  #[allow(dead_code)]
  pub jsonrpc: String,
  /// Notifications omit `id`; requests carry it and expect a response.
  #[serde(default)]
  pub id: Option<Value>,
  pub method: String,
  #[serde(default)]
  pub params: Value,
}

impl Request {
  pub fn is_notification(&self) -> bool {
    self.id.is_none()
  }
}

#[derive(Debug, Serialize)]
pub struct Response {
  pub jsonrpc: &'static str,
  pub id: Value,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub result: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<RpcError>,
}

impl Response {
  pub fn ok(id: Value, result: Value) -> Self {
    Self {
      jsonrpc: "2.0",
      id,
      result: Some(result),
      error: None,
    }
  }

  pub fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
    Self {
      jsonrpc: "2.0",
      id,
      result: None,
      error: Some(RpcError {
        code,
        message: message.into(),
        data: None,
      }),
    }
  }
}

#[derive(Debug, Serialize)]
pub struct RpcError {
  pub code: i32,
  pub message: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub data: Option<Value>,
}

// Standard JSON-RPC 2.0 error codes.
pub const PARSE_ERROR: i32 = -32700;
#[allow(dead_code)]
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;
