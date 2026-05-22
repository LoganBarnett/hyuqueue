use chrono::Utc;
use hyuqueue_core::event::Event;
use sqlx::SqliteExecutor;
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

  #[error("Failed to serialize event field '{field}': {source}")]
  Serialize {
    field: &'static str,
    #[source]
    source: serde_json::Error,
  },

  #[error("Failed to deserialize event data: {0}")]
  Deserialize(#[from] serde_json::Error),
}

pub async fn append<'e, E: SqliteExecutor<'e>>(
  executor: E,
  event: &Event,
) -> Result<(), EventsError> {
  let event_type_str = ser_unquoted(&event.event_type, "event_type")?;
  let actor_str = ser_unquoted(&event.actor, "actor")?;
  let locality_str = ser_unquoted(&event.locality, "locality")?;
  let payload_str = ser(&event.payload, "payload")?;
  sqlx::query(
    "INSERT INTO events
       (id, event_type, actor, locality, payload, created_at)
     VALUES (?,?,?,?,?,?)",
  )
  .bind(event.id.to_string())
  .bind(event_type_str)
  .bind(actor_str)
  .bind(locality_str)
  .bind(payload_str)
  .bind(event.created_at.to_rfc3339())
  .execute(executor)
  .await
  .map_err(|source| EventsError::Db {
    context: "appending event",
    source,
  })?;
  Ok(())
}

fn ser<T: serde::Serialize>(
  value: &T,
  field: &'static str,
) -> Result<String, EventsError> {
  serde_json::to_string(value)
    .map_err(|source| EventsError::Serialize { field, source })
}

/// Serialize an enum-as-string (e.g. `EventType`, `Actor`) and strip
/// the surrounding JSON quotes so the result fits in a SQLite TEXT
/// column without extra escaping at the application layer.
fn ser_unquoted<T: serde::Serialize>(
  value: &T,
  field: &'static str,
) -> Result<String, EventsError> {
  ser(value, field).map(|s| s.trim_matches('"').to_string())
}

pub async fn for_item<'e, E: SqliteExecutor<'e>>(
  executor: E,
  item_id: Uuid,
) -> Result<Vec<serde_json::Value>, EventsError> {
  let rows: Vec<(String,)> = sqlx::query_as(
    "SELECT payload FROM events
     WHERE json_extract(payload, '$.item_id') = ?
     ORDER BY created_at ASC",
  )
  .bind(item_id.to_string())
  .fetch_all(executor)
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
