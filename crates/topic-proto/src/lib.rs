//! hyuqueue-topic-proto — wire types for the topic ↔ host JSON-RPC
//! protocol.
//!
//! This crate defines the on-the-wire envelope and per-method payloads
//! for communication between the hyuqueue server and out-of-process
//! topic subprocesses.  It contains no I/O — line framing, transport,
//! and process management live in the SDK and host wrapper crates.
//!
//! # Wire format
//!
//! JSON-RPC 2.0 over newline-delimited JSON on stdin/stdout.  Each
//! request, response, or notification is a single line of JSON
//! terminated by `\n`.
//!
//! # Direction
//!
//! Most calls flow host → topic (`init`, `ingest`, `execute`,
//! `shutdown`).  The `topic_data_set` notification flows topic → host
//! and lets a topic persist state without a separate back-channel.

pub mod envelope;
pub mod error;
pub mod method;
pub mod payload;
pub mod version;

pub use envelope::{
  ErrorResponse, Notification, Request, Response, RpcError, SuccessResponse,
};
pub use error::{
  topic_error_to_rpc_error, TopicErrorData, RPC_ERROR_CODE_TOPIC,
};
pub use payload::{
  ExecuteRequest, ExecuteResponse, IngestRequest, IngestResponse, InitRequest,
  InitResponse, ShutdownRequest, TopicDataSetParams,
};
pub use version::JsonRpcVersion;
