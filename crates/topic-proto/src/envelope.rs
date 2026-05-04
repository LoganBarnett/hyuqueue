//! JSON-RPC 2.0 envelope types.
//!
//! Per-method payloads (e.g. `IngestRequest`, `ExecuteResponse`) are
//! defined in [`crate::payload`] and serialized into the generic
//! `params` / `result` fields of these envelopes.  Dispatchers parse
//! the envelope first, branch on `method`, then deserialize the typed
//! payload from `params`.

use crate::version::JsonRpcVersion;
use serde::{Deserialize, Serialize};

/// A JSON-RPC 2.0 request.  Carries an `id` so the response can be
/// correlated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
  pub jsonrpc: JsonRpcVersion,
  pub method: String,
  pub params: serde_json::Value,
  pub id: u64,
}

/// A JSON-RPC 2.0 notification.  No `id`, no response expected.
/// Used for topic → host messages like `topic_data_set`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
  pub jsonrpc: JsonRpcVersion,
  pub method: String,
  pub params: serde_json::Value,
}

/// A JSON-RPC 2.0 response — either success or error.
///
/// Untagged so that the wire form is the canonical JSON-RPC shape (a
/// response object has either a `result` field or an `error` field,
/// never both, and never a discriminator).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
  Success(SuccessResponse),
  Error(ErrorResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessResponse {
  pub jsonrpc: JsonRpcVersion,
  pub result: serde_json::Value,
  pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
  pub jsonrpc: JsonRpcVersion,
  pub error: RpcError,
  pub id: u64,
}

/// JSON-RPC 2.0 error object.  `data` is optional structured detail —
/// for topic errors we put the originating [`hyuqueue_core::topic::TopicError`]
/// variant here so callers can match on it (see
/// [`crate::error::TopicErrorData`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
  pub code: i32,
  pub message: String,
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub data: Option<serde_json::Value>,
}
