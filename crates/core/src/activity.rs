use serde::{Deserialize, Serialize};

/// An action that can be taken on an item.
///
/// Activities come from two sources:
/// - Item-scoped: declared by the source topic, embedded in
///   `item.capabilities`.  Only available on items from that topic.
/// - Global: registered by any installed topic, available on every item.
///   (e.g. org-mode's "refile" global activity)
///
/// The shape is designed to be serialized directly as an LLM tool
/// definition (OpenAI / MCP-compatible) with no translation layer.
/// `description`, `params` (JSON Schema), and `examples` are what the
/// model sees during tool selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
  /// Stable identifier (e.g. "jira.close", "org.refile").
  pub id: String,
  /// Short human-readable label shown in the UI key palette.
  pub label: String,
  /// Single-character keyboard shortcut. Must not conflict with vim
  /// bindings or other activities in the palette.
  pub key: char,
  /// Where this activity executes.
  pub executor: ActivityExecutor,
  /// Whether the activity mutates external state.  Outtake LLM may
  /// directly invoke `Read` activities; `Write` activities can only be
  /// proposed inside suggestion items.  Intake LLM may invoke either,
  /// subject to confidence gating.
  pub effect: ActivityEffect,
  /// Natural-language description used by LLMs during tool selection.
  /// Distinct from `label` (short, UI-facing).
  pub description: String,
  /// JSON Schema describing the parameter object accepted by this
  /// activity.  An empty object schema (`{"type": "object"}`) means the
  /// activity takes no parameters.
  pub params: serde_json::Value,
  /// Optional few-shot examples.  Empty by default.
  #[serde(default)]
  pub examples: Vec<ActivityExample>,
  /// When true, the activity is hidden from the intake LLM's tool
  /// surface.  Reserved for actions a human must always confirm
  /// (e.g. "send press release").
  #[serde(default)]
  pub human_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivityExecutor {
  /// Execute on this machine via the registered topic.
  Local,
  /// Package as an upstream signal and route via the item's
  /// `delegate_from`.
  Upstream,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivityEffect {
  /// Pure query.  No mutation of external state.
  Read,
  /// Mutates external state (sends, deletes, creates, etc.).
  Write,
}

/// Few-shot example attached to an activity declaration.  Surfaced to
/// the LLM alongside the tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityExample {
  /// Example parameter values, conforming to the activity's `params`
  /// schema.
  pub params: serde_json::Value,
  /// Natural-language description of what the activity does for these
  /// params.
  pub outcome: String,
}

/// The payload when a human or LLM invokes an activity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityInvocation {
  pub activity_id: String,
  /// Parameter values, conforming to the activity's `params` schema.
  pub params: serde_json::Value,
}
