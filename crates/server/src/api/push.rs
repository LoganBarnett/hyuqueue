//! Webhook endpoint for external sources pushing items into the queue.
//!
//! Any caller (Emacs, shell scripts, other tools, remote hyuqueue instances)
//! can POST here to enqueue an item. No domain knowledge lives in the callers.

use crate::web_base::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use chrono::Utc;
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  item::{Item, ItemState},
};
use hyuqueue_store::{events, items};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct PushRequest {
  pub title: String,
  pub body: Option<String>,
  /// Required: identifies the origin system (e.g. "email", "jira", "slack").
  pub source: String,
  pub source_topic_id: Option<String>,
  pub queue_id: Uuid,
  #[serde(default)]
  pub metadata: serde_json::Value,
}

pub async fn handle_push(
  State(state): State<AppState>,
  Json(req): Json<PushRequest>,
) -> impl IntoResponse {
  let now = Utc::now();
  let item = Item {
    id: Uuid::new_v4(),
    queue_id: req.queue_id,
    title: req.title,
    body: req.body,
    source_topic_id: req.source_topic_id,
    source: req.source,
    delegate_from: None,
    delegate_chain: vec![],
    capabilities: vec![],
    metadata: req.metadata,
    state: ItemState::IntakePending,
    created_at: now,
    updated_at: now,
  };

  if let Err(e) = items::insert(&state.db, &item).await {
    return (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response();
  }

  let event = events::new_event(
    item.id,
    EventType::ItemCreated,
    Actor::System,
    Locality::Local,
    json!({ "source": item.source, "via": "push_webhook" }),
  );
  let _ = events::append(&state.db, &event).await;

  (StatusCode::ACCEPTED, Json(json!({ "item_id": item.id }))).into_response()
}
