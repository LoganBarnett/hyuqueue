//! Errors raised by the host wrapper itself — distinct from
//! topic-originated errors which surface as
//! [`hyuqueue_core::topic::TopicError`] from `ingest`/`execute`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostError {
  #[error("Failed to spawn topic subprocess: {0}")]
  Spawn(#[source] std::io::Error),

  #[error("Topic subprocess did not expose stdin/stdout — required for the JSON-RPC channel")]
  MissingPipes,

  #[error("Failed to write request to topic subprocess: {0}")]
  WriteRequest(#[source] std::io::Error),

  #[error("Failed to serialize request: {0}")]
  SerializeRequest(#[from] serde_json::Error),

  #[error("Topic subprocess closed its stdout before responding to init")]
  InitClosed,

  #[error("Topic subprocess returned a non-success response to init: {0}")]
  InitFailed(String),

  #[error(
    "Topic id mismatch: configured as {expected:?} but subprocess reported {actual:?}"
  )]
  IdMismatch { expected: String, actual: String },
}
