//! Error code constants and `TopicError` ↔ `RpcError` mapping.
//!
//! JSON-RPC 2.0 reserves error codes -32000 through -32099 for
//! application-defined "server errors".  We use a single code,
//! [`RPC_ERROR_CODE_TOPIC`], for all topic-originated errors and
//! discriminate variants via the structured `data` field.

use crate::envelope::RpcError;
use hyuqueue_core::topic::TopicError;
use serde::{Deserialize, Serialize};

/// JSON-RPC error code for topic-originated errors.
///
/// Per JSON-RPC 2.0, codes -32000 to -32099 are reserved for
/// "implementation-defined server-errors."
pub const RPC_ERROR_CODE_TOPIC: i32 = -32000;

/// Structured detail attached to the `data` field of a topic
/// `RpcError`.  Mirrors the variants of `hyuqueue_core::topic::TopicError`
/// so the host can reconstruct typed errors after deserializing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TopicErrorData {
  UnsupportedActivity {
    activity_id: String,
    topic_id: String,
  },
  Execution {
    activity: String,
    reason: String,
  },
  Configuration {
    detail: String,
  },
}

impl From<&TopicError> for TopicErrorData {
  fn from(err: &TopicError) -> Self {
    match err {
      TopicError::UnsupportedActivity(activity_id, topic_id) => {
        TopicErrorData::UnsupportedActivity {
          activity_id: activity_id.clone(),
          topic_id: topic_id.clone(),
        }
      }
      TopicError::Execution { activity, reason } => TopicErrorData::Execution {
        activity: activity.clone(),
        reason: reason.clone(),
      },
      TopicError::Configuration(detail) => TopicErrorData::Configuration {
        detail: detail.clone(),
      },
    }
  }
}

/// Convert a `TopicError` to an `RpcError` with structured `data`.
///
/// Fallible because `serde_json::to_value` returns `Result`; in
/// practice `TopicErrorData` is a plain enum of `String` fields and
/// the conversion does not fail, but per the project's no-`unwrap`
/// rule the result is propagated rather than asserted.
pub fn topic_error_to_rpc_error(
  err: &TopicError,
) -> Result<RpcError, serde_json::Error> {
  let data = serde_json::to_value(TopicErrorData::from(err))?;
  Ok(RpcError {
    code: RPC_ERROR_CODE_TOPIC,
    message: err.to_string(),
    data: Some(data),
  })
}
