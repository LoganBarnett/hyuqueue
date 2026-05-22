use crate::activity::Activity;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// An item is identity + content + provenance.  Where an item is in
/// the system (intake-pending, awaiting-human, etc.) is determined
/// by queue membership in `queue_items`, not by a field on the item
/// itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
  pub id: Uuid,
  /// Short human-readable title shown in the queue.
  pub title: String,
  /// Optional longer body — email body, ticket description, etc.
  pub body: Option<String>,
  /// Which topic produced this item (e.g. "jira", "email").
  pub source_topic_id: Option<String>,
  /// The `--source` tag required on every item.  Identifies the
  /// origin system for calibration and analytics ("email", "jira",
  /// "slack", etc.).
  pub source: String,
  /// Set when this item was published from another hyuqueue instance.
  pub delegate_from: Option<DelegateRef>,
  /// Full provenance trail — ordered from origin to here.
  pub delegate_chain: Vec<DelegateRef>,
  /// Item-scoped activities declared by the source topic.  These
  /// travel with the item when it is published to another instance.
  pub capabilities: Vec<Activity>,
  /// Source-specific data (email headers, ticket fields, etc.).
  pub metadata: serde_json::Value,
  pub created_at: DateTime<Utc>,
  pub updated_at: DateTime<Utc>,
}

/// Where an item came from when it was published from another instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateRef {
  /// HTTP address of the originating hyuqueue server.
  pub queue_addr: String,
  /// ID of the item in the originating instance.
  pub item_id: Uuid,
}
