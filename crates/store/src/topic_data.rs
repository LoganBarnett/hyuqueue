use crate::db::Db;
use chrono::Utc;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TopicDataError {
  #[error("Database error while {context}: {source}")]
  Db {
    context: &'static str,
    #[source]
    source: sqlx::Error,
  },

  #[error("Failed to deserialize topic data value: {0}")]
  Deserialize(#[from] serde_json::Error),
}

/// Fetch all key-value pairs for a topic.
pub async fn get_all(
  db: &Db,
  topic_id: &str,
) -> Result<HashMap<String, serde_json::Value>, TopicDataError> {
  let rows: Vec<(String, String)> =
    sqlx::query_as("SELECT key, value FROM topic_data WHERE topic_id = ?")
      .bind(topic_id)
      .fetch_all(db.pool())
      .await
      .map_err(|source| TopicDataError::Db {
        context: "fetching topic data",
        source,
      })?;

  rows
    .into_iter()
    .map(|(k, v)| {
      serde_json::from_str(&v)
        .map(|parsed| (k, parsed))
        .map_err(TopicDataError::Deserialize)
    })
    .collect()
}

/// Upsert a single key-value pair in the topic_data projection.
/// The caller is responsible for appending the corresponding event.
pub async fn upsert(
  db: &Db,
  topic_id: &str,
  key: &str,
  value: &serde_json::Value,
) -> Result<(), TopicDataError> {
  sqlx::query(
    "INSERT INTO topic_data (topic_id, key, value, updated_at)
     VALUES (?, ?, ?, ?)
     ON CONFLICT (topic_id, key)
     DO UPDATE SET value = excluded.value,
                   updated_at = excluded.updated_at",
  )
  .bind(topic_id)
  .bind(key)
  .bind(serde_json::to_string(value).unwrap())
  .bind(Utc::now().to_rfc3339())
  .execute(db.pool())
  .await
  .map_err(|source| TopicDataError::Db {
    context: "upserting topic data",
    source,
  })?;
  Ok(())
}
