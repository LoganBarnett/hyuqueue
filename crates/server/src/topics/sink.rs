//! `TopicDataSink` implementation that writes notifications into the
//! server's database.

use async_trait::async_trait;
use hyuqueue_core::event::{Actor, EventType, Locality};
use hyuqueue_store::{events, topic_data, Db};
use hyuqueue_topic_host::TopicDataSink;
use serde_json::json;
use tracing::warn;

pub struct DbBackedSink {
  db: Db,
}

impl DbBackedSink {
  pub fn new(db: Db) -> Self {
    Self { db }
  }
}

#[async_trait]
impl TopicDataSink for DbBackedSink {
  async fn set_data(
    &self,
    topic_id: &str,
    key: &str,
    value: serde_json::Value,
  ) {
    // Event first — events are the source of truth.  The projection
    // upsert below is derivable from the event stream.
    let event = events::new_event(
      EventType::TopicDataUpdated,
      Actor::Topic(topic_id.to_string()),
      Locality::Local,
      json!({
        "topic_id": topic_id,
        "key": key,
        "value": value,
      }),
    );
    if let Err(e) = events::append(self.db.pool(), &event).await {
      warn!(
        topic = %topic_id,
        key = %key,
        "Failed to append TopicDataUpdated event: {e}"
      );
      // Skip the projection too — projection without event would
      // make the audit trail wrong.
      return;
    }
    if let Err(e) = topic_data::upsert(&self.db, topic_id, key, &value).await {
      warn!(
        topic = %topic_id,
        key = %key,
        "Failed to upsert topic_data projection: {e}"
      );
    }
  }
}
