pub mod heartbeat;

use crate::config::TopicConfig;
use heartbeat::HeartbeatTopic;
use hyuqueue_core::topic::Topic;
use hyuqueue_store::{queues, Db};
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

/// Maps topic IDs to their compiled-in implementation, resolved queue,
/// and user-provided config.
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

/// Match each topic config entry against compiled-in topics, resolve
/// queue names to UUIDs, and build the registry. Skips entries whose
/// queue does not exist or whose topic ID has no compiled-in
/// implementation.
pub async fn build_registry(configs: &[TopicConfig], db: &Db) -> TopicRegistry {
  let mut entries = HashMap::new();

  for tc in configs {
    let topic: Arc<dyn Topic> = match tc.id.as_str() {
      "heartbeat" => Arc::new(HeartbeatTopic),
      other => {
        warn!(topic = other, "No compiled-in topic implementation, skipping");
        continue;
      }
    };

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
