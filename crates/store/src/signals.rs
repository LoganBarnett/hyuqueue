use crate::db::Db;
use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum SignalsError {
  #[error("Database error while {context}: {source}")]
  Db {
    context: &'static str,
    #[source]
    source: sqlx::Error,
  },
}

#[derive(Debug, sqlx::FromRow)]
pub struct OutboundSignalRow {
  pub id: String,
  pub item_id: String,
  pub target_queue_addr: String,
  pub activity_id: String,
  pub payload: String,
  pub status: String,
  pub attempts: i64,
}

pub async fn enqueue(
  db: &Db,
  item_id: Uuid,
  target_queue_addr: &str,
  activity_id: &str,
  payload: serde_json::Value,
) -> Result<Uuid, SignalsError> {
  let id = Uuid::new_v4();
  let now = Utc::now().to_rfc3339();
  sqlx::query(
    "INSERT INTO outbound_signals
       (id, item_id, target_queue_addr, activity_id, payload,
        status, attempts, created_at, updated_at)
     VALUES (?,?,?,?,?,'pending',0,?,?)",
  )
  .bind(id.to_string())
  .bind(item_id.to_string())
  .bind(target_queue_addr)
  .bind(activity_id)
  .bind(serde_json::to_string(&payload).unwrap())
  .bind(&now)
  .bind(&now)
  .execute(db.pool())
  .await
  .map_err(|source| SignalsError::Db {
    context: "enqueuing outbound signal",
    source,
  })?;
  Ok(id)
}

pub async fn pending(
  db: &Db,
  limit: i64,
) -> Result<Vec<OutboundSignalRow>, SignalsError> {
  sqlx::query_as::<_, OutboundSignalRow>(
    "SELECT id, item_id, target_queue_addr, activity_id, payload,
            status, attempts
     FROM outbound_signals
     WHERE status = 'pending'
     ORDER BY created_at ASC LIMIT ?",
  )
  .bind(limit)
  .fetch_all(db.pool())
  .await
  .map_err(|source| SignalsError::Db {
    context: "fetching pending signals",
    source,
  })
}

pub async fn mark_delivered(db: &Db, id: Uuid) -> Result<(), SignalsError> {
  let now = Utc::now().to_rfc3339();
  sqlx::query(
    "UPDATE outbound_signals
     SET status = 'delivered', updated_at = ?
     WHERE id = ?",
  )
  .bind(&now)
  .bind(id.to_string())
  .execute(db.pool())
  .await
  .map_err(|source| SignalsError::Db {
    context: "marking signal delivered",
    source,
  })?;
  Ok(())
}

pub async fn mark_failed(
  db: &Db,
  id: Uuid,
  attempts: i64,
) -> Result<(), SignalsError> {
  let now = Utc::now().to_rfc3339();
  sqlx::query(
    "UPDATE outbound_signals
     SET status = 'failed', attempts = ?, last_attempt_at = ?, updated_at = ?
     WHERE id = ?",
  )
  .bind(attempts)
  .bind(&now)
  .bind(&now)
  .bind(id.to_string())
  .execute(db.pool())
  .await
  .map_err(|source| SignalsError::Db {
    context: "marking signal failed",
    source,
  })?;
  Ok(())
}
