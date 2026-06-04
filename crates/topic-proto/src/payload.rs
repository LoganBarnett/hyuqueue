//! Per-method request / response / notification payloads.
//!
//! These are serialized into the generic `params` and `result` fields
//! of the JSON-RPC envelope (see [`crate::envelope`]).  The dispatcher
//! is responsible for matching on the envelope's `method` and pulling
//! the right payload type out of `params`.

use hyuqueue_core::activity::{Activity, ActivityInvocation};
use hyuqueue_core::event::Event;
use hyuqueue_core::topic::IngestItem;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// ── init ─────────────────────────────────────────────────────────────

/// Request payload for `init`.
///
/// Carries the persisted `topic_data` snapshot — the full set of
/// key/value pairs the host has stored for this topic id.  The topic
/// uses this to hydrate its in-memory state at handshake time so
/// per-subprocess restarts resume where the previous instance left
/// off instead of starting empty.
///
/// The topic is the only writer of its own `topic_data`, so this
/// snapshot is authoritative.  Subsequent mutations flow through the
/// existing `topic_data_set` notification path.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InitRequest {
  /// Persisted `topic_data` for this topic id; empty on first run
  /// or for topics that have never written any state.
  #[serde(default)]
  pub topic_data: HashMap<String, serde_json::Value>,
}

/// Response payload for `init`.  This is the topic's complete
/// declaration of itself: identity, declared activities, and which
/// optional methods it implements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitResponse {
  /// Stable topic identifier (e.g. "jira", "email").  Must match the
  /// `id` the host has configured for this subprocess; the host
  /// rejects mismatches at handshake time.
  pub id: String,
  /// Human-readable name shown in the UI.
  pub display_name: String,
  /// Whether this topic implements `ingest`.  When false, the host
  /// will not call it.
  pub supports_ingest: bool,
  /// Whether this topic implements `execute`.  When false, any
  /// declared activity with `executor: Local` is a topic bug — the
  /// host will reject the activity at registry build time.
  pub supports_execute: bool,
  /// Item-scoped activities.  Travel with items this topic produces.
  #[serde(default)]
  pub item_activities: Vec<Activity>,
  /// Activities available on every item, regardless of source.
  #[serde(default)]
  pub global_activities: Vec<Activity>,
}

// ── ingest ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRequest {
  /// Topic-specific configuration, passed through verbatim from the
  /// host's TOML config.
  pub config: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResponse {
  pub items: Vec<IngestItem>,
}

// ── execute ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteRequest {
  pub invocation: ActivityInvocation,
  pub item_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteResponse {
  pub event: Event,
}

// ── topic_data_set (notification, topic → host) ──────────────────────

/// Notification payload for `topic_data_set`.  The host already knows
/// which topic sent the notification (it owns the subprocess), so the
/// payload does not carry `topic_id` — including it would let a topic
/// write under another topic's identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicDataSetParams {
  pub key: String,
  pub value: serde_json::Value,
}

// ── shutdown ─────────────────────────────────────────────────────────

/// Request payload for `shutdown`.  Empty; reserved.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShutdownRequest {}
