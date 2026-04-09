//! The event log — the source of truth for all state changes.
//!
//! The `items` and `topic_data` tables are materialized projections
//! of these events. When in doubt, trust the events.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
  pub id: Uuid,
  pub event_type: EventType,
  pub actor: Actor,
  pub locality: Locality,
  /// Event-specific data. Shape varies by event_type.
  pub payload: serde_json::Value,
  pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
  /// Item was created and entered the intake queue.
  ItemCreated,
  /// Intake LLM analyzed the item.
  IntakeLlmAnalysis,
  /// A human or LLM invoked an activity on the item.
  ActionTaken,
  /// Human requested the item be re-processed through intake.
  ReIntakeRequested,
  /// Review LLM analyzed the item post-ack.
  ReviewLlmAnalysis,
  /// Review LLM produced a suggestion item.
  SuggestionCreated,
  /// An upstream signal was packaged and queued for delivery.
  UpstreamSignalSent,
  /// An upstream signal was received from a downstream instance.
  UpstreamSignalReceived,
  /// A source policy was updated (possibly affecting future intake routing).
  PolicyUpdated,
  /// A topic's persistent key-value data was updated.
  TopicDataUpdated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
  System,
  IntakeLlm,
  ReviewLlm,
  Human,
  /// A specific topic identified by its id string.
  Topic(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
  /// This event was produced and executed on this instance.
  Local,
  /// This event is a signal being routed upstream via delegate_from.
  UpstreamSignal,
}

// ── Typed payloads ────────────────────────────────────────────────────────────
// These are serialized into Event.payload as JSON.
// Item-scoped payloads carry an explicit item_id field.

/// Payload for EventType::IntakeLlmAnalysis
#[derive(Debug, Serialize, Deserialize)]
pub struct IntakeLlmPayload {
  pub item_id: Uuid,
  pub model: String,
  pub confident: bool,
  /// Why the LLM was uncertain (shown to human in queue).
  pub uncertainty_reason: Option<String>,
  /// The decision taken if confident.
  pub auto_action: Option<String>,
}

/// Payload for EventType::ActionTaken
#[derive(Debug, Serialize, Deserialize)]
pub struct ActionTakenPayload {
  pub item_id: Uuid,
  pub activity_id: String,
  pub params: serde_json::Value,
  /// Human-readable description of what happened (e.g. "refiled to ~/notes.org::Inbox").
  pub result_summary: Option<String>,
}

/// Payload for EventType::ReviewLlmAnalysis
#[derive(Debug, Serialize, Deserialize)]
pub struct ReviewLlmPayload {
  pub item_id: Uuid,
  pub model: String,
  /// Tool calls the review LLM made to query item history.
  pub queries_run: Vec<serde_json::Value>,
  pub reasoning: Option<String>,
  /// If Some, a suggestion item was created.
  pub suggestion_item_id: Option<Uuid>,
}

/// Payload for EventType::TopicDataUpdated
#[derive(Debug, Serialize, Deserialize)]
pub struct TopicDataPayload {
  pub topic_id: String,
  pub key: String,
  pub value: serde_json::Value,
}
