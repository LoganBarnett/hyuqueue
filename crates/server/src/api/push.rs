//! Webhook endpoint for external sources pushing items into the queue.
//!
//! Any caller (Emacs, shell scripts, other tools, remote hyuqueue
//! instances) can POST here to enqueue an item.  No domain knowledge
//! lives in the callers.

use crate::web_base::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use chrono::Utc;
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  item::Item,
  queue as queue_names,
};
use hyuqueue_store::{events, items, queue};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct PushRequest {
  pub title: String,
  pub body: Option<String>,
  /// Required: identifies the origin system (e.g. "email", "jira",
  /// "slack").
  pub source: String,
  pub source_topic_id: Option<String>,
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
    title: req.title,
    body: req.body,
    source_topic_id: req.source_topic_id,
    source: req.source,
    delegate_from: None,
    delegate_chain: vec![],
    capabilities: vec![],
    metadata: req.metadata,
    created_at: now,
    updated_at: now,
  };

  match push_tx(&state, &item).await {
    Ok(()) => (StatusCode::ACCEPTED, Json(json!({ "item_id": item.id })))
      .into_response(),
    Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e })))
      .into_response(),
  }
}

async fn push_tx(state: &AppState, item: &Item) -> Result<(), String> {
  let mut tx = state
    .db
    .pool()
    .begin()
    .await
    .map_err(|e| format!("begin tx: {e}"))?;
  let event = events::new_item_event(
    item.id,
    EventType::ItemCreated,
    Actor::System,
    Locality::Local,
    json!({ "source": item.source, "via": "push_webhook" }),
  );
  events::append(&mut *tx, &event)
    .await
    .map_err(|e| format!("event append: {e}"))?;
  items::insert(&mut *tx, item)
    .await
    .map_err(|e| format!("item insert: {e}"))?;
  queue::enqueue(&mut *tx, queue_names::INTAKE, item.id, 0)
    .await
    .map_err(|e| format!("enqueue: {e}"))?;
  tx.commit().await.map_err(|e| format!("commit: {e}"))?;
  Ok(())
}
