use crate::db::Db;
use chrono::Utc;
use hyuqueue_core::item::{Item, ItemState};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ItemsError {
  #[error("Item '{0}' not found")]
  NotFound(Uuid),

  #[error("Database error while {context}: {source}")]
  Db {
    context: &'static str,
    #[source]
    source: sqlx::Error,
  },

  #[error("Failed to deserialize item data: {0}")]
  Deserialize(#[from] serde_json::Error),
}

/// Minimal row type for reading items back from SQLite.
#[derive(Debug, sqlx::FromRow)]
struct ItemRow {
  id: String,
  queue_id: String,
  title: String,
  body: Option<String>,
  source_topic_id: Option<String>,
  source: String,
  delegate_from: Option<String>,
  delegate_chain: String,
  capabilities: String,
  metadata: String,
  state: String,
  created_at: String,
  updated_at: String,
}

impl ItemRow {
  fn into_item(self) -> Result<Item, serde_json::Error> {
    Ok(Item {
      id: Uuid::parse_str(&self.id).unwrap_or_default(),
      queue_id: Uuid::parse_str(&self.queue_id).unwrap_or_default(),
      title: self.title,
      body: self.body,
      source_topic_id: self.source_topic_id,
      source: self.source,
      delegate_from: self
        .delegate_from
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?,
      delegate_chain: serde_json::from_str(&self.delegate_chain)?,
      capabilities: serde_json::from_str(&self.capabilities)?,
      metadata: serde_json::from_str(&self.metadata)?,
      state: parse_state(&self.state),
      created_at: self.created_at.parse().unwrap_or_else(|_| Utc::now()),
      updated_at: self.updated_at.parse().unwrap_or_else(|_| Utc::now()),
    })
  }
}

fn parse_state(s: &str) -> ItemState {
  match s {
    "intake_pending" => ItemState::IntakePending,
    "human_pending" => ItemState::HumanPending,
    "auto_handled" => ItemState::AutoHandled,
    "done" => ItemState::Done,
    _ => ItemState::IntakePending,
  }
}

pub async fn insert(db: &Db, item: &Item) -> Result<(), ItemsError> {
  let now = Utc::now().to_rfc3339();
  sqlx::query(
    "INSERT INTO items
       (id, queue_id, title, body, source_topic_id, source,
        delegate_from, delegate_chain, capabilities, metadata,
        state, created_at, updated_at)
     VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
  )
  .bind(item.id.to_string())
  .bind(item.queue_id.to_string())
  .bind(&item.title)
  .bind(&item.body)
  .bind(&item.source_topic_id)
  .bind(&item.source)
  .bind(
    item
      .delegate_from
      .as_ref()
      .map(|d| serde_json::to_string(d).unwrap()),
  )
  .bind(serde_json::to_string(&item.delegate_chain).unwrap())
  .bind(serde_json::to_string(&item.capabilities).unwrap())
  .bind(serde_json::to_string(&item.metadata).unwrap())
  .bind(item.state.to_string())
  .bind(&now)
  .bind(&now)
  .execute(db.pool())
  .await
  .map_err(|source| ItemsError::Db {
    context: "inserting item",
    source,
  })?;
  Ok(())
}

pub async fn get(db: &Db, id: Uuid) -> Result<Item, ItemsError> {
  let row = sqlx::query_as::<_, ItemRow>("SELECT * FROM items WHERE id = ?")
    .bind(id.to_string())
    .fetch_optional(db.pool())
    .await
    .map_err(|source| ItemsError::Db {
      context: "fetching item",
      source,
    })?
    .ok_or(ItemsError::NotFound(id))?;

  row.into_item().map_err(ItemsError::Deserialize)
}

pub async fn list(
  db: &Db,
  queue_id: Option<Uuid>,
  state: Option<ItemState>,
  limit: i64,
  offset: i64,
) -> Result<Vec<Item>, ItemsError> {
  let queue_filter = queue_id.map(|id| id.to_string());
  let state_filter = state.map(|s| s.to_string());

  let rows = sqlx::query_as::<_, ItemRow>(
    "SELECT * FROM items
     WHERE (? IS NULL OR queue_id = ?)
       AND (? IS NULL OR state = ?)
     ORDER BY created_at DESC
     LIMIT ? OFFSET ?",
  )
  .bind(&queue_filter)
  .bind(&queue_filter)
  .bind(&state_filter)
  .bind(&state_filter)
  .bind(limit)
  .bind(offset)
  .fetch_all(db.pool())
  .await
  .map_err(|source| ItemsError::Db {
    context: "listing items",
    source,
  })?;

  rows
    .into_iter()
    .map(|r| r.into_item().map_err(ItemsError::Deserialize))
    .collect()
}

pub async fn update_state(
  db: &Db,
  id: Uuid,
  state: ItemState,
) -> Result<(), ItemsError> {
  let now = Utc::now().to_rfc3339();
  sqlx::query("UPDATE items SET state = ?, updated_at = ? WHERE id = ?")
    .bind(state.to_string())
    .bind(&now)
    .bind(id.to_string())
    .execute(db.pool())
    .await
    .map_err(|source| ItemsError::Db {
      context: "updating item state",
      source,
    })?;
  Ok(())
}

/// Count items in the human queue (shown to the user in iron mode).
pub async fn human_queue_count(db: &Db) -> Result<i64, ItemsError> {
  let row: (i64,) =
    sqlx::query_as("SELECT COUNT(*) FROM items WHERE state = 'human_pending'")
      .fetch_one(db.pool())
      .await
      .map_err(|source| ItemsError::Db {
        context: "counting human queue",
        source,
      })?;
  Ok(row.0)
}

/// Fetch the next item for the human (oldest human_pending).
pub async fn next_human_item(db: &Db) -> Result<Option<Item>, ItemsError> {
  let row = sqlx::query_as::<_, ItemRow>(
    "SELECT * FROM items WHERE state = 'human_pending'
     ORDER BY created_at ASC LIMIT 1",
  )
  .fetch_optional(db.pool())
  .await
  .map_err(|source| ItemsError::Db {
    context: "fetching next human item",
    source,
  })?;

  row
    .map(|r| r.into_item().map_err(ItemsError::Deserialize))
    .transpose()
}
