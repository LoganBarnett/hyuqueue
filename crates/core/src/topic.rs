use crate::activity::{Activity, ActivityInvocation};
use crate::event::Event;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

/// An item produced by a topic's `ingest()` method. The core crate stays
/// pure — the server worker handles DB insertion and event creation.
///
/// The host fills in `source_instance_id` on the resulting `Item`
/// from the topic's configured instance id; the topic does not need
/// to (and cannot meaningfully) provide it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestItem {
  pub title: String,
  pub body: Option<String>,
  /// Stable per-item identifier in the source system (RSS `<guid>`,
  /// email `Message-ID`, ticket key).  Topics should populate this
  /// whenever the source has a stable id — it is what enables
  /// host-side dedupe and traceback to the origin.  Leave `None`
  /// only when the source genuinely has no stable identifier.
  #[serde(default)]
  pub external_id: Option<String>,
  pub metadata: serde_json::Value,
}

/// The plugin interface. A topic is a domain of integration capability.
///
/// Each topic implements whichever methods it needs — `ingest`,
/// `item_activities`, `global_activities`, and `execute` are all
/// optional.  Only `id` and `display_name` are required.
///
/// Routing: activities with `ActivityExecutor::Upstream` are packaged
/// as outbound signals and delivered back to the originating instance.
/// `Topic::execute` is only called for `ActivityExecutor::Local`.
///
/// The `ctx` argument on `ingest` and `execute` carries the side
/// channel a topic uses to push state updates (cursors, sync tokens)
/// back to the host.  See [`TopicCtx`].
#[async_trait]
pub trait Topic: Send + Sync {
  fn id(&self) -> &str;
  fn display_name(&self) -> &str;

  /// Poll an external source for new items.  Called periodically by
  /// the ingest worker.  Returns an empty vec by default.
  async fn ingest(
    &self,
    _ctx: &TopicCtx,
    _config: &serde_json::Value,
  ) -> Result<Vec<IngestItem>, TopicError> {
    Ok(vec![])
  }

  /// Activities available on items whose `source_instance_id`
  /// matches this topic's configured instance id.
  fn item_activities(&self) -> Vec<Activity> {
    vec![]
  }

  /// Activities available on every item, regardless of source.
  fn global_activities(&self) -> Vec<Activity> {
    vec![]
  }

  /// Execute a local activity invocation.  Only called when
  /// `executor == Local`.
  async fn execute(
    &self,
    ctx: &TopicCtx,
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

// ── TopicCtx and Notifier ────────────────────────────────────────────

/// Side channel a topic uses to push state updates back to the host.
///
/// In the SDK, this writes JSON-RPC notifications to the topic's
/// stdout.  In the host (when calling a `SubprocessTopic`) and in
/// tests, a no-op or recording implementation is supplied instead.
///
/// `TopicCtx` is intentionally opaque from the topic author's
/// perspective; it is just a handle whose methods are the topic's
/// outbound API.
#[derive(Clone)]
pub struct TopicCtx {
  notifier: Arc<dyn Notifier>,
}

impl TopicCtx {
  pub fn new(notifier: Arc<dyn Notifier>) -> Self {
    Self { notifier }
  }

  /// A `TopicCtx` whose `set_data` is a no-op.  Use in host call
  /// sites that don't expect notifications to fire (e.g. when calling
  /// a `SubprocessTopic`, where notifications flow back over the
  /// wire instead) and in tests that don't care about persistence.
  pub fn stub() -> Self {
    Self::new(Arc::new(NoopNotifier))
  }

  /// Persist a key/value pair into the topic's `topic_data`
  /// projection.  Fire-and-forget at the protocol level — there is no
  /// response — but this method is fallible because the underlying
  /// notifier may fail to write (e.g. a broken stdout pipe in the
  /// SDK).  Topics that cannot meaningfully recover should propagate
  /// the error.
  pub async fn set_data(
    &self,
    key: &str,
    value: serde_json::Value,
  ) -> Result<(), TopicCtxError> {
    self.notifier.set_data(key, value).await
  }
}

impl std::fmt::Debug for TopicCtx {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TopicCtx").finish_non_exhaustive()
  }
}

/// Backing trait for [`TopicCtx`].  Implemented by the SDK
/// (writes notifications to stdout), by the host's stub
/// (`NoopNotifier`), and by test fixtures (recording the calls).
///
/// Public so out-of-tree implementations can be plugged into a
/// `TopicCtx::new(...)`.
#[async_trait]
pub trait Notifier: Send + Sync {
  async fn set_data(
    &self,
    key: &str,
    value: serde_json::Value,
  ) -> Result<(), TopicCtxError>;
}

/// Notifier that drops every call.  Suitable for host code paths and
/// tests that don't exercise persistence.
pub struct NoopNotifier;

#[async_trait]
impl Notifier for NoopNotifier {
  async fn set_data(
    &self,
    _key: &str,
    _value: serde_json::Value,
  ) -> Result<(), TopicCtxError> {
    Ok(())
  }
}

#[derive(Debug, Error)]
pub enum TopicCtxError {
  #[error("Failed to deliver notification to host: {0}")]
  NotificationDelivery(String),

  #[error("Failed to serialize notification payload: {0}")]
  NotificationSerialize(#[from] serde_json::Error),
}
