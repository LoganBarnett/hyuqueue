use async_trait::async_trait;
use chrono::Utc;
use hyuqueue_core::{
  activity::ActivityInvocation,
  event::Event,
  topic::{IngestItem, Topic, TopicError},
};
use serde_json::json;

/// Test topic that generates one synthetic item per ingest cycle.
/// Useful for proving the ingest pipeline end-to-end.
pub struct HeartbeatTopic;

#[async_trait]
impl Topic for HeartbeatTopic {
  fn id(&self) -> &str {
    "heartbeat"
  }

  fn display_name(&self) -> &str {
    "Heartbeat"
  }

  async fn ingest(
    &self,
    _config: &serde_json::Value,
  ) -> Result<Vec<IngestItem>, TopicError> {
    Ok(vec![IngestItem {
      title: "Heartbeat".to_string(),
      source: "heartbeat".to_string(),
      body: None,
      metadata: json!({ "timestamp": Utc::now().to_rfc3339() }),
    }])
  }

  async fn execute(
    &self,
    invocation: &ActivityInvocation,
    _item_id: uuid::Uuid,
  ) -> Result<Event, TopicError> {
    Err(TopicError::UnsupportedActivity(
      invocation.activity_id.clone(),
      self.id().to_string(),
    ))
  }
}
