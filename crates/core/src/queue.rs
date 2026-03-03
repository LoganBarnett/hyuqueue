use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
  pub id: Uuid,
  pub name: String,
  /// Tags used for view filtering (e.g. "work", "personal").
  pub tags: Vec<String>,
  /// Topic-specific configuration (ingestion settings, credentials, etc.).
  pub config: serde_json::Value,
  pub created_at: DateTime<Utc>,
  pub updated_at: DateTime<Utc>,
}
