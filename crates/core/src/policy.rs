use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Instructions for the intake LLM on how to handle items matching a pattern.
///
/// The intake LLM is given the matching policy's system_prompt and examples
/// as context when deciding whether to auto-handle or escalate to the human.
///
/// Policies are created/updated by:
/// - The human via "edit prompt" action on any item.
/// - The review LLM's suggestions (after human approves).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcePolicy {
  pub id: Uuid,
  /// Glob/regex matched against "{source_topic_id}:{source}" of incoming items.
  /// Example: "email:amazon.com" or "jira:*"
  pub source_pattern: String,
  /// System prompt given to the intake LLM for matching items.
  pub system_prompt: String,
  /// Few-shot examples to anchor the LLM's behavior.
  pub examples: Vec<PolicyExample>,
  /// Confidence threshold above which the LLM auto-handles (0.0–1.0).
  pub confidence_threshold: f32,
  pub created_at: DateTime<Utc>,
  pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyExample {
  /// Brief description of the item that matched.
  pub item_summary: String,
  /// What action was taken (activity_id or description).
  pub action_taken: String,
}
