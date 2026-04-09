use crate::activity::{Activity, ActivityInvocation};
use crate::event::Event;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// An item produced by a topic's `ingest()` method. The core crate stays
/// pure — the server worker handles DB insertion and event creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestItem {
  pub title: String,
  pub source: String,
  pub body: Option<String>,
  pub metadata: serde_json::Value,
}

/// The plugin interface. A topic is a domain of integration capability.
///
/// Topics are compiled into the binary for MVP. Each topic implements
/// whichever of these it needs — none are required:
///
/// - ingest: produce items from an external source
/// - item_activities: activities for items FROM this topic (travel with items)
/// - global_activities: activities available on ANY item (e.g. org-mode refile)
/// - execute: perform an activity when invoked
///
/// Routing: activities with ActivityExecutor::Upstream are packaged as
/// outbound signals and delivered back to the originating instance.
/// The Topic::execute method is only called for ActivityExecutor::Local.
#[async_trait]
pub trait Topic: Send + Sync {
  fn id(&self) -> &str;
  fn display_name(&self) -> &str;

  /// Poll an external source for new items. Called periodically by the
  /// ingest worker. Returns an empty vec by default.
  async fn ingest(
    &self,
    _config: &serde_json::Value,
  ) -> Result<Vec<IngestItem>, TopicError> {
    Ok(vec![])
  }

  /// Activities available on items whose source_topic_id matches this topic.
  fn item_activities(&self) -> Vec<Activity> {
    vec![]
  }

  /// Activities available on every item, regardless of source.
  fn global_activities(&self) -> Vec<Activity> {
    vec![]
  }

  /// Execute a local activity invocation. Only called when executor == Local.
  async fn execute(
    &self,
    invocation: &ActivityInvocation,
    item_id: uuid::Uuid,
  ) -> Result<Event, TopicError>;
}

#[derive(Debug, Error)]
pub enum TopicError {
  #[error("Activity '{0}' is not supported by topic '{1}'")]
  UnsupportedActivity(String, String),

  #[error("Execution of activity '{activity}' failed: {reason}")]
  Execution { activity: String, reason: String },

  #[error("Topic configuration error: {0}")]
  Configuration(String),
}
