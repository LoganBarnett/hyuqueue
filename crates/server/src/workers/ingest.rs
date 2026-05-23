//! Ingest worker — polls registered topics for new items.
//!
//! Iterates the topic registry on a fixed interval, calling each
//! topic's `ingest()` method.  Each returned `IngestItem` is
//! persisted in three steps:
//!
//! 1. Append an `ItemCreated` event (events are the source of truth).
//! 2. Insert the item projection.
//! 3. Enqueue the item id into the `intake` queue so the intake
//!    worker picks it up.
//!
//! Topics are polled concurrently (one topic's slow integration
//! doesn't block another), and per-topic items persist concurrently
//! (SQLite serializes writes at the connection level, so this is
//! more about expressing independence than throughput).

use crate::topics::{TopicEntry, TopicRegistry};
use crate::tx_error::TxOpError;
use futures::future::join_all;
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  item::Item,
  queue as queue_names,
  topic::{IngestItem, TopicCtx},
};
use hyuqueue_store::{events, items, queue, Db};
use serde_json::json;
use std::sync::Arc;
use tap::TapFallible;
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
  let ctx = TopicCtx::stub();
  join_all(
    registry
      .entries()
      .iter()
      .map(|(topic_id, entry)| ingest_one_topic(db, topic_id, entry, &ctx)),
  )
  .await;
}

async fn ingest_one_topic(
  db: &Db,
  topic_id: &str,
  entry: &TopicEntry,
  ctx: &TopicCtx,
) {
  let Some(items) = entry
    .topic
    .ingest(ctx, &entry.config)
    .await
    .tap_err(|e| warn!(topic = %topic_id, "Ingest failed: {e}"))
    .ok()
    .filter(|items| !items.is_empty())
  else {
    return;
  };

  info!(
    topic = %topic_id,
    count = items.len(),
    "Ingested items from topic"
  );

  join_all(
    items
      .into_iter()
      .map(|ingest_item| persist_item(db, topic_id, ingest_item)),
  )
  .await;
}

async fn persist_item(db: &Db, topic_id: &str, ingest_item: IngestItem) {
  let item_id = Uuid::new_v4();
  if let Err(e) = persist_item_inner(db, topic_id, item_id, ingest_item).await {
    error!(topic = %topic_id, item_id = %item_id, "Failed to persist: {e}");
  }
}

async fn persist_item_inner(
  db: &Db,
  topic_id: &str,
  item_id: Uuid,
  ingest_item: IngestItem,
) -> Result<(), TxOpError> {
  let item = Item {
    id: item_id,
    title: ingest_item.title,
    body: ingest_item.body,
    source_topic_id: Some(topic_id.to_string()),
    source: ingest_item.source.clone(),
    delegate_from: None,
    delegate_chain: vec![],
    capabilities: vec![],
    metadata: ingest_item.metadata.clone(),
    created_at: chrono::Utc::now(),
    updated_at: chrono::Utc::now(),
  };

  // All four writes (creation event, item insert, queue enqueue,
  // enqueue audit event) must succeed atomically — otherwise the
  // intake worker could see a queued item with no projection, or
  // the audit trail could miss an event for an item that exists.
  let mut tx = db.pool().begin().await.map_err(TxOpError::BeginTx)?;

  events::append(
    &mut *tx,
    &events::new_item_event(
      item_id,
      EventType::ItemCreated,
      Actor::Topic(topic_id.to_string()),
      Locality::Local,
      json!({
        "source": ingest_item.source,
        "metadata": ingest_item.metadata,
      }),
    ),
  )
  .await?;

  items::insert(&mut *tx, &item).await?;

  queue::enqueue(&mut *tx, queue_names::INTAKE, item_id, 0).await?;

  events::append(
    &mut *tx,
    &events::new_event(
      EventType::ItemEnqueued,
      Actor::System,
      Locality::Local,
      json!({
        "item_id": item_id,
        "queue": queue_names::INTAKE,
        "priority": 0,
      }),
    ),
  )
  .await?;

  tx.commit().await.map_err(TxOpError::CommitTx)?;
  Ok(())
}
