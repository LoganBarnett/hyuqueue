pub mod sink;

use crate::config::TopicConfig;
use hyuqueue_core::topic::Topic;
use hyuqueue_store::{queues, Db};
use hyuqueue_topic_host::{SubprocessTopic, TopicDataSink};
use sink::DbBackedSink;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

/// A registered topic paired with its resolved queue ID and config.
pub struct TopicEntry {
  pub topic: Arc<dyn Topic>,
  pub queue_id: Uuid,
  pub config: serde_json::Value,
}

/// Maps topic IDs to a spawned topic subprocess, its resolved queue,
/// and the user-provided config.
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
}

/// Spawn a subprocess for each configured topic and build the
/// registry.  Topics whose queue is unknown or whose subprocess fails
/// to spawn are logged and skipped — startup proceeds with a partial
/// registry rather than failing the whole server.
pub async fn build_registry(configs: &[TopicConfig], db: &Db) -> TopicRegistry {
  let sink: Arc<dyn TopicDataSink> = Arc::new(DbBackedSink::new(db.clone()));
  let mut entries = HashMap::new();

  for tc in configs {
    let queue = match queues::get_by_name(db, &tc.queue_name).await {
      Ok(Some(q)) => q,
      Ok(None) => {
        warn!(
          topic = %tc.id,
          queue = %tc.queue_name,
          "Queue not found, skipping topic"
        );
        continue;
      }
      Err(e) => {
        warn!(
          topic = %tc.id,
          queue = %tc.queue_name,
          "Failed to look up queue: {e}, skipping topic"
        );
        continue;
      }
    };

    let topic =
      match SubprocessTopic::spawn(&tc.id, &tc.command, sink.clone()).await {
        Ok(t) => Arc::new(t) as Arc<dyn Topic>,
        Err(e) => {
          warn!(
            topic = %tc.id,
            command = ?tc.command,
            "Failed to spawn topic subprocess: {e}, skipping"
          );
          continue;
        }
      };

    entries.insert(
      tc.id.clone(),
      TopicEntry {
        topic,
        queue_id: queue.id,
        config: tc.config.clone(),
      },
    );
  }

  TopicRegistry { entries }
}
