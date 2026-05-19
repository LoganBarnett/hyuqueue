//! Sink for `topic_data_set` notifications received from subprocess
//! topics.
//!
//! The sink is the integration point with the host's storage layer:
//! production code wires it to write into the `topic_data` projection
//! table, tests wire it to record calls into an in-memory buffer.

use async_trait::async_trait;

#[async_trait]
pub trait TopicDataSink: Send + Sync {
  /// Record that the named topic wants `key` set to `value` in its
  /// `topic_data` projection.
  ///
  /// Infallible at this layer — implementations log and continue on
  /// their own errors.  Events are the source of truth in hyuqueue's
  /// architecture, so a dropped projection write is recoverable
  /// without propagating a panic into the host's request loop.
  async fn set_data(&self, topic_id: &str, key: &str, value: serde_json::Value);
}

/// Sink that drops every call.  Useful for tests that don't care
/// about persistence.
pub struct NoopSink;

#[async_trait]
impl TopicDataSink for NoopSink {
  async fn set_data(
    &self,
    _topic_id: &str,
    _key: &str,
    _value: serde_json::Value,
  ) {
  }
}
