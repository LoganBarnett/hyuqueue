use chrono::Utc;
use hyuqueue_core::item::Item;
use sqlx::SqliteExecutor;
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

  #[error("Failed to serialize item field '{field}': {source}")]
  Serialize {
    field: &'static str,
    #[source]
    source: serde_json::Error,
  },

  #[error("Failed to deserialize item data: {0}")]
  Deserialize(#[from] serde_json::Error),
}

/// Minimal row type for reading items back from SQLite.
#[derive(Debug, sqlx::FromRow)]
struct ItemRow {
  id: String,
  title: String,
  body: Option<String>,
  source_topic_id: Option<String>,
  source: String,
  delegate_from: Option<String>,
  delegate_chain: String,
  capabilities: String,
  metadata: String,
  created_at: String,
  updated_at: String,
}

impl ItemRow {
  fn into_item(self) -> Result<Item, serde_json::Error> {
    Ok(Item {
      id: Uuid::parse_str(&self.id).unwrap_or_default(),
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
      created_at: self.created_at.parse().unwrap_or_else(|_| Utc::now()),
      updated_at: self.updated_at.parse().unwrap_or_else(|_| Utc::now()),
    })
  }
}

pub async fn insert<'e, E: SqliteExecutor<'e>>(
  executor: E,
  item: &Item,
) -> Result<(), ItemsError> {
  let now = Utc::now().to_rfc3339();
  let delegate_from_str = item
    .delegate_from
    .as_ref()
    .map(|d| ser(d, "delegate_from"))
    .transpose()?;
  let delegate_chain_str = ser(&item.delegate_chain, "delegate_chain")?;
  let capabilities_str = ser(&item.capabilities, "capabilities")?;
  let metadata_str = ser(&item.metadata, "metadata")?;
  sqlx::query(
    "INSERT INTO items
       (id, title, body, source_topic_id, source,
        delegate_from, delegate_chain, capabilities, metadata,
        created_at, updated_at)
     VALUES (?,?,?,?,?,?,?,?,?,?,?)",
  )
  .bind(item.id.to_string())
  .bind(&item.title)
  .bind(&item.body)
  .bind(&item.source_topic_id)
  .bind(&item.source)
  .bind(delegate_from_str)
  .bind(delegate_chain_str)
  .bind(capabilities_str)
  .bind(metadata_str)
  .bind(&now)
  .bind(&now)
  .execute(executor)
  .await
  .map_err(|source| ItemsError::Db {
    context: "inserting item",
    source,
  })?;
  Ok(())
}

fn ser<T: serde::Serialize>(
  value: &T,
  field: &'static str,
) -> Result<String, ItemsError> {
  serde_json::to_string(value)
    .map_err(|source| ItemsError::Serialize { field, source })
}

pub async fn get<'e, E: SqliteExecutor<'e>>(
  executor: E,
  id: Uuid,
) -> Result<Item, ItemsError> {
  let row = sqlx::query_as::<_, ItemRow>("SELECT * FROM items WHERE id = ?")
    .bind(id.to_string())
    .fetch_optional(executor)
    .await
    .map_err(|source| ItemsError::Db {
      context: "fetching item",
      source,
    })?
    .ok_or(ItemsError::NotFound(id))?;

  row.into_item().map_err(ItemsError::Deserialize)
}

/// List items, optionally filtered by source.  Note this is a raw
/// item listing — for "what's in a queue" use
/// `hyuqueue_store::queue::list` which respects queue membership
/// and shows claim state.
pub async fn list<'e, E: SqliteExecutor<'e>>(
  executor: E,
  source: Option<&str>,
  limit: i64,
  offset: i64,
) -> Result<Vec<Item>, ItemsError> {
  let rows = sqlx::query_as::<_, ItemRow>(
    "SELECT * FROM items
     WHERE (? IS NULL OR source = ?)
     ORDER BY created_at DESC
     LIMIT ? OFFSET ?",
  )
  .bind(source)
  .bind(source)
  .bind(limit)
  .bind(offset)
  .fetch_all(executor)
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
