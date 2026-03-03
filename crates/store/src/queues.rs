use crate::db::Db;
use chrono::Utc;
use hyuqueue_core::queue::Queue;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum QueuesError {
  #[error("Queue '{0}' not found")]
  NotFound(Uuid),

  #[error("Database error while {context}: {source}")]
  Db {
    context: &'static str,
    #[source]
    source: sqlx::Error,
  },

  #[error("Failed to deserialize queue data: {0}")]
  Deserialize(#[from] serde_json::Error),
}

#[derive(Debug, sqlx::FromRow)]
struct QueueRow {
  id: String,
  name: String,
  tags: String,
  config: String,
  created_at: String,
  updated_at: String,
}

impl QueueRow {
  fn into_queue(self) -> Result<Queue, serde_json::Error> {
    Ok(Queue {
      id: Uuid::parse_str(&self.id).unwrap_or_default(),
      name: self.name,
      tags: serde_json::from_str(&self.tags)?,
      config: serde_json::from_str(&self.config)?,
      created_at: self
        .created_at
        .parse()
        .unwrap_or_else(|_| Utc::now()),
      updated_at: self
        .updated_at
        .parse()
        .unwrap_or_else(|_| Utc::now()),
    })
  }
}

pub async fn insert(db: &Db, queue: &Queue) -> Result<(), QueuesError> {
  let now = Utc::now().to_rfc3339();
  sqlx::query(
    "INSERT INTO queues (id, name, tags, config, created_at, updated_at)
     VALUES (?,?,?,?,?,?)",
  )
  .bind(queue.id.to_string())
  .bind(&queue.name)
  .bind(serde_json::to_string(&queue.tags).unwrap())
  .bind(serde_json::to_string(&queue.config).unwrap())
  .bind(&now)
  .bind(&now)
  .execute(db.pool())
  .await
  .map_err(|source| QueuesError::Db {
    context: "inserting queue",
    source,
  })?;
  Ok(())
}

pub async fn list(db: &Db) -> Result<Vec<Queue>, QueuesError> {
  let rows = sqlx::query_as::<_, QueueRow>(
    "SELECT * FROM queues ORDER BY name",
  )
  .fetch_all(db.pool())
  .await
  .map_err(|source| QueuesError::Db {
    context: "listing queues",
    source,
  })?;

  rows
    .into_iter()
    .map(|r| r.into_queue().map_err(QueuesError::Deserialize))
    .collect()
}

pub async fn get_by_name(
  db: &Db,
  name: &str,
) -> Result<Option<Queue>, QueuesError> {
  let row = sqlx::query_as::<_, QueueRow>(
    "SELECT * FROM queues WHERE name = ?",
  )
  .bind(name)
  .fetch_optional(db.pool())
  .await
  .map_err(|source| QueuesError::Db {
    context: "fetching queue by name",
    source,
  })?;

  row
    .map(|r| r.into_queue().map_err(QueuesError::Deserialize))
    .transpose()
}
