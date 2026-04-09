//! Ingest worker — polls registered topics for new items.
//!
//! Iterates the topic registry on a fixed interval, calling each
//! topic's `ingest()` method. Returned `IngestItem` values are
//! persisted: first an `ItemCreated` event is appended, then the
//! `items` projection is updated.

use crate::topics::TopicRegistry;
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  item::{Item, ItemState},
};
use hyuqueue_store::{events, items, Db};
use serde_json::json;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_secs(30);

pub async fn run(db: Db, registry: Arc<TopicRegistry>) {
  info!("Ingest worker started");

  loop {
    process_all(&db, &registry).await;
    sleep(POLL_INTERVAL).await;
  }
}

async fn process_all(db: &Db, registry: &TopicRegistry) {
  for (topic_id, entry) in registry.entries() {
    let ingest_items = match entry.topic.ingest(&entry.config).await {
      Ok(items) => items,
      Err(e) => {
        warn!(
          topic = %topic_id,
          "Ingest failed: {e}"
        );
        continue;
      }
    };

    if ingest_items.is_empty() {
      continue;
    }

    info!(
      topic = %topic_id,
      count = ingest_items.len(),
      "Ingested items from topic"
    );

    for ingest_item in ingest_items {
      let item_id = Uuid::new_v4();

      // Event first — events are the source of truth.
      let event = events::new_item_event(
        item_id,
        EventType::ItemCreated,
        Actor::Topic(topic_id.clone()),
        Locality::Local,
        json!({
          "source": ingest_item.source,
          "metadata": ingest_item.metadata,
        }),
      );

      if let Err(e) = events::append(db, &event).await {
        error!(
          topic = %topic_id,
          item_id = %item_id,
          "Failed to append ItemCreated event: {e}"
        );
        continue;
      }

      // Then update the items projection.
      let item = Item {
        id: item_id,
        queue_id: entry.queue_id,
        title: ingest_item.title,
        body: ingest_item.body,
        source_topic_id: Some(topic_id.clone()),
        source: ingest_item.source,
        delegate_from: None,
        delegate_chain: vec![],
        capabilities: vec![],
        metadata: ingest_item.metadata.clone(),
        state: ItemState::IntakePending,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
      };

      if let Err(e) = items::insert(db, &item).await {
        error!(
          topic = %topic_id,
          item_id = %item_id,
          "Failed to insert ingested item: {e}"
        );
      }
    }
  }
}
