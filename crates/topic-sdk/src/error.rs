//! SDK-level errors — failures that arise running the loop itself,
//! distinct from topic-originated errors which become structured
//! JSON-RPC error responses on the wire.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SdkError {
  #[error("Failed to read from stdin: {0}")]
  StdinRead(#[source] std::io::Error),

  #[error("Failed to write to stdout: {0}")]
  StdoutWrite(#[source] std::io::Error),

  #[error("Writer task panicked or was aborted")]
  WriterTaskJoin(#[source] tokio::task::JoinError),
}
