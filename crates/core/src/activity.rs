use serde::{Deserialize, Serialize};

/// An action that can be taken on an item.
///
/// Activities come from two sources:
/// - Item-scoped: declared by the source topic, embedded in item.capabilities.
///   Only available on items from that topic.
/// - Global: registered by any installed topic, available on every item.
///   (e.g. org-mode's "refile" global activity)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
  /// Stable identifier (e.g. "jira.close", "org.refile").
  pub id: String,
  /// Human-readable label shown in the UI.
  pub label: String,
  /// Single-character keyboard shortcut. Must not conflict with vim bindings.
  pub key: char,
  /// Where this activity executes.
  pub executor: ActivityExecutor,
  /// Additional input parameters (if any).
  pub params: Vec<ActivityParam>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivityExecutor {
  /// Execute on this machine via the registered topic.
  Local,
  /// Package as an upstream signal and route via the item's delegate_from.
  Upstream,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityParam {
  pub name: String,
  pub param_type: ParamType,
  pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
  Text,
  Bool,
  Choice(Vec<String>),
}

/// The payload when a human or upstream invokes an activity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityInvocation {
  pub activity_id: String,
  /// Parameter values keyed by ActivityParam.name.
  pub params: serde_json::Value,
}
