use crate::db::Db;
use chrono::Utc;
use hyuqueue_core::event::Event;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum EventsError {
  #[error("Database error while {context}: {source}")]
  Db {
    context: &'static str,
    #[source]
    source: sqlx::Error,
  },

  #[error("Failed to deserialize event data: {0}")]
  Deserialize(#[from] serde_json::Error),
}

pub async fn append(db: &Db, event: &Event) -> Result<(), EventsError> {
  sqlx::query(
    "INSERT INTO events
       (id, event_type, actor, locality, payload, created_at)
     VALUES (?,?,?,?,?,?)",
  )
  .bind(event.id.to_string())
  .bind(
    serde_json::to_string(&event.event_type)
      .unwrap()
      .trim_matches('"')
      .to_string(),
  )
  .bind(
    serde_json::to_string(&event.actor)
      .unwrap()
      .trim_matches('"')
      .to_string(),
  )
  .bind(
    serde_json::to_string(&event.locality)
      .unwrap()
      .trim_matches('"')
      .to_string(),
  )
  .bind(serde_json::to_string(&event.payload).unwrap())
  .bind(event.created_at.to_rfc3339())
  .execute(db.pool())
  .await
  .map_err(|source| EventsError::Db {
    context: "appending event",
    source,
  })?;
  Ok(())
}

pub async fn for_item(
  db: &Db,
  item_id: Uuid,
) -> Result<Vec<serde_json::Value>, EventsError> {
  let rows: Vec<(String,)> = sqlx::query_as(
    "SELECT payload FROM events
     WHERE json_extract(payload, '$.item_id') = ?
     ORDER BY created_at ASC",
  )
  .bind(item_id.to_string())
  .fetch_all(db.pool())
  .await
  .map_err(|source| EventsError::Db {
    context: "fetching events for item",
    source,
  })?;

  rows
    .into_iter()
    .map(|(p,)| serde_json::from_str(&p).map_err(EventsError::Deserialize))
    .collect()
}

/// Fetch items in a given state that have not yet been processed by the
/// review LLM. Used by the review worker to find work.
pub async fn items_awaiting_review(
  db: &Db,
  limit: i64,
) -> Result<Vec<Uuid>, EventsError> {
  // Items that are Done but have no ReviewLlmAnalysis event yet.
  let rows: Vec<(String,)> = sqlx::query_as(
    "SELECT i.id FROM items i
     WHERE i.state = 'done'
       AND NOT EXISTS (
         SELECT 1 FROM events e
         WHERE e.event_type = 'review_llm_analysis'
           AND json_extract(e.payload, '$.item_id') = i.id
       )
     ORDER BY i.updated_at ASC
     LIMIT ?",
  )
  .bind(limit)
  .fetch_all(db.pool())
  .await
  .map_err(|source| EventsError::Db {
    context: "fetching items awaiting review",
    source,
  })?;

  Ok(
    rows
      .into_iter()
      .filter_map(|(id,)| Uuid::parse_str(&id).ok())
      .collect(),
  )
}

/// Fetch items awaiting intake LLM processing.
pub async fn items_awaiting_intake(
  db: &Db,
  limit: i64,
) -> Result<Vec<Uuid>, EventsError> {
  let rows: Vec<(String,)> = sqlx::query_as(
    "SELECT id FROM items WHERE state = 'intake_pending'
     ORDER BY created_at ASC LIMIT ?",
  )
  .bind(limit)
  .fetch_all(db.pool())
  .await
  .map_err(|source| EventsError::Db {
    context: "fetching items awaiting intake",
    source,
  })?;

  Ok(
    rows
      .into_iter()
      .filter_map(|(id,)| Uuid::parse_str(&id).ok())
      .collect(),
  )
}

/// Build a new Event with a fresh id and current timestamp.
pub fn new_event(
  event_type: hyuqueue_core::event::EventType,
  actor: hyuqueue_core::event::Actor,
  locality: hyuqueue_core::event::Locality,
  payload: serde_json::Value,
) -> Event {
  Event {
    id: Uuid::new_v4(),
    event_type,
    actor,
    locality,
    payload,
    created_at: Utc::now(),
  }
}

/// Convenience: build an item-scoped Event, merging `item_id` into
/// the payload automatically.
pub fn new_item_event(
  item_id: Uuid,
  event_type: hyuqueue_core::event::EventType,
  actor: hyuqueue_core::event::Actor,
  locality: hyuqueue_core::event::Locality,
  extra: serde_json::Value,
) -> Event {
  let payload = match extra {
    serde_json::Value::Object(mut map) => {
      map.insert(
        "item_id".to_string(),
        serde_json::Value::String(item_id.to_string()),
      );
      serde_json::Value::Object(map)
    }
    _ => serde_json::json!({ "item_id": item_id.to_string() }),
  };

  new_event(event_type, actor, locality, payload)
}
