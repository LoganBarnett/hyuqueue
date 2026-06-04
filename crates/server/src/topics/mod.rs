pub mod sink;

use crate::config::TopicConfig;
use futures::future::join_all;
use hyuqueue_core::topic::Topic;
use hyuqueue_store::{topic_data, Db};
use hyuqueue_topic_host::{SubprocessTopic, TopicDataSink};
use sink::DbBackedSink;
use std::collections::HashMap;
use std::sync::Arc;
use tap::TapFallible;
use tracing::warn;

/// A registered topic paired with the user-provided config.
pub struct TopicEntry {
  pub topic: Arc<dyn Topic>,
  pub config: serde_json::Value,
}

/// Maps topic IDs to a spawned topic subprocess and the user-provided
/// config.
pub struct TopicRegistry {
  entries: HashMap<String, TopicEntry>,
}

impl TopicRegistry {
  /// An empty registry for tests or when no topics are configured.
  pub fn empty() -> Self {
    Self {
      entries: HashMap::new(),
    }
  }

  pub fn entries(&self) -> &HashMap<String, TopicEntry> {
    &self.entries
  }

  /// Build a registry from a prepared entry map.  Test-only path —
  /// production code builds via [`build_registry`] which spawns
  /// subprocesses.  Tests use this to plug in `impl Topic` stubs
  /// without crossing the subprocess boundary.
  pub fn from_entries_for_test(entries: HashMap<String, TopicEntry>) -> Self {
    Self { entries }
  }
}

/// Spawn a subprocess for each configured topic and build the
/// registry.  Topics are spawned concurrently — startup latency
/// scales with the slowest spawn, not the sum.  Failures log and
/// skip individually; startup proceeds with a partial registry.
pub async fn build_registry(configs: &[TopicConfig], db: &Db) -> TopicRegistry {
  let sink: Arc<dyn TopicDataSink> = Arc::new(DbBackedSink::new(db.clone()));
  let entries =
    join_all(configs.iter().map(|tc| spawn_one(tc, db, sink.clone())))
      .await
      .into_iter()
      .flatten()
      .collect();
  TopicRegistry { entries }
}

/// Spawn a single topic subprocess, returning the registry entry on
/// success or `None` (with a warning logged) on failure.
///
/// Fetches the persisted `topic_data` snapshot for this topic id
/// first so the subprocess can hydrate its in-memory state at
/// handshake time — see `Topic::init` and `InitRequest.topic_data`.
async fn spawn_one(
  tc: &TopicConfig,
  db: &Db,
  sink: Arc<dyn TopicDataSink>,
) -> Option<(String, TopicEntry)> {
  let initial_data = topic_data::get_all(db, &tc.id)
    .await
    .tap_err(|e| {
      warn!(
        topic = %tc.id,
        "Failed to load topic_data for hydrate-on-init: {e}; \
         starting topic with empty state"
      )
    })
    .unwrap_or_default();

  SubprocessTopic::spawn(&tc.id, &tc.command, sink, initial_data)
    .await
    .tap_err(|e| {
      warn!(
        topic = %tc.id,
        command = ?tc.command,
        "Failed to spawn topic subprocess: {e}, skipping"
      )
    })
    .ok()
    .map(|topic| {
      (
        tc.id.clone(),
        TopicEntry {
          topic: Arc::new(topic) as Arc<dyn Topic>,
          config: tc.config.clone(),
        },
      )
    })
}
