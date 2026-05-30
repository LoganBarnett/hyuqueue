//! Webhook endpoint for external sources pushing items into the queue.
//!
//! Any caller (Emacs, shell scripts, other tools, remote hyuqueue
//! instances) can POST here to enqueue an item.  No domain knowledge
//! lives in the callers.

use crate::tx_error::TxOpError;
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
  /// Optional: which configured topic instance this push is being
  /// attributed to (e.g. "work-email").  Free-form for the push API
  /// — there is no requirement that the value match a live
  /// `[[topics]]` instance, though using a matching id is what
  /// enables topic-type derivation for display.
  pub source_instance_id: Option<String>,
  /// Optional: stable per-item identifier in the source system
  /// (e.g. `Message-ID` for emails pushed by an external script).
  /// Populating this enables host-side dedupe on
  /// `(source_instance_id, external_id)`.
  pub external_id: Option<String>,
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
    source_instance_id: req.source_instance_id,
    external_id: req.external_id,
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
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

async fn push_tx(state: &AppState, item: &Item) -> Result<(), TxOpError> {
  let mut tx = state.db.pool().begin().await.map_err(TxOpError::BeginTx)?;
  let event = events::new_item_event(
    item.id,
    EventType::ItemCreated,
    Actor::System,
    Locality::Local,
    json!({
      "source_instance_id": item.source_instance_id,
      "external_id": item.external_id,
      "via": "push_webhook",
    }),
  );
  events::append(&mut *tx, &event).await?;
  items::insert(&mut *tx, item).await?;
  queue::enqueue(&mut *tx, queue_names::INTAKE, item.id, 0).await?;
  tx.commit().await.map_err(TxOpError::CommitTx)?;
  Ok(())
}
