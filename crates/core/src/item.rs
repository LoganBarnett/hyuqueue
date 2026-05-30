use crate::activity::Activity;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// An item is identity + content + provenance.  Where an item is in
/// the system (intake-pending, awaiting-human, etc.) is determined
/// by queue membership in `queue_items`, not by a field on the item
/// itself.
///
/// Three concepts that often get conflated:
///
/// - *Topic type* — what kind of source ("rss", "email").  Declared
///   by the topic binary.  *Not* persisted on the item; derived at
///   display time from the live config keyed by
///   `source_instance_id`.  Renaming a topic type therefore does not
///   strand existing items.
/// - *Topic instance* — `source_instance_id` below.
/// - *External item id* — `external_id` below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
  pub id: Uuid,
  /// Short human-readable title shown in the queue.
  pub title: String,
  /// Optional longer body — email body, ticket description, etc.
  pub body: Option<String>,
  /// Host-assigned instance identifier for the topic that produced
  /// this item — the `[[topics]].id` field from `config.toml` (e.g.
  /// "rss-tech", "work-email").  For items pushed via the webhook
  /// API, supplied by the caller.  May be `None` for one-off pushed
  /// items and server-generated items with no associated instance.
  pub source_instance_id: Option<String>,
  /// Stable per-item identifier within the source system (RSS
  /// `<guid>`, email `Message-ID`, Jira ticket key, etc.).
  ///
  /// Topics *should* populate this whenever the source has a stable
  /// identifier.  Absence is not an error — it means "no path back
  /// to origin," and may reduce what operations are available on
  /// this item.  Treat absence as lack-of-information, never as a
  /// signal that something went wrong.
  ///
  /// Host-side dedupe is keyed off `(source_instance_id,
  /// external_id)`.  Items without an `external_id` are not
  /// deduplicated (SQLite treats NULLs as distinct under `UNIQUE`).
  pub external_id: Option<String>,
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
