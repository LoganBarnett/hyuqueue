//! Shared error type for the server's transactional composers.
//!
//! Every transactional path in the server — worker finalization
//! steps, HTTP handlers that compose multiple writes — does the same
//! shape of work: `begin → some sequence of (event append | item
//! insert | queue op) → commit`.  Each step can fail; the caller
//! wants a semantic enum that preserves the source chain so the
//! display message and any future recovery logic stay correct.
//!
//! Variants are grouped by the underlying store module (events,
//! items, queue) rather than by per-call-site operation name.  The
//! step-level context already lives in the wrapped error's own
//! `context` / variant data — `TxOpError::Queue(QueueError::Db {
//! context: "enqueuing item", source })` reads cleanly, no need to
//! re-encode "enqueue" at this layer.

use hyuqueue_store::{
  events::EventsError, items::ItemsError, queue::QueueError,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TxOpError {
  #[error("Failed to begin transaction: {0}")]
  BeginTx(#[source] sqlx::Error),

  #[error("Failed to commit transaction: {0}")]
  CommitTx(#[source] sqlx::Error),

  #[error("Event store operation failed: {0}")]
  Events(#[from] EventsError),

  #[error("Item store operation failed: {0}")]
  Items(#[from] ItemsError),

  #[error("Queue operation failed: {0}")]
  Queue(#[from] QueueError),
}
