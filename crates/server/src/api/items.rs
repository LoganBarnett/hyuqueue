//! Item CRUD and action endpoints.

use crate::web_base::AppState;
use axum::{
  extract::{Path, Query, State},
  http::StatusCode,
  response::IntoResponse,
  routing::{get, post},
  Json, Router,
};
use chrono::Utc;
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  item::{Item, ItemState},
};
use hyuqueue_store::{events, items};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
  Router::new()
    .route("/", get(list_items).post(create_item))
    .route("/{id}", get(get_item))
    .route("/{id}/action", post(invoke_action))
    .route("/{id}/ack", post(ack_item))
    .route("/next", get(next_item))
    .route("/count", get(queue_count))
}

// ── List ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListParams {
  pub queue_id: Option<Uuid>,
  pub state: Option<String>,
  #[serde(default = "default_limit")]
  pub limit: i64,
  #[serde(default)]
  pub offset: i64,
}

fn default_limit() -> i64 {
  50
}

async fn list_items(
  State(state): State<AppState>,
  Query(params): Query<ListParams>,
) -> impl IntoResponse {
  let item_state = params.state.as_deref().map(parse_state);

  match items::list(
    &state.db,
    params.queue_id,
    item_state,
    params.limit,
    params.offset,
  )
  .await
  {
    Ok(items) => Json(json!({ "items": items })).into_response(),
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

// ── Create ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateItemRequest {
  pub title: String,
  pub body: Option<String>,
  pub source: String,
  pub source_topic_id: Option<String>,
  pub queue_id: Uuid,
  #[serde(default)]
  pub metadata: serde_json::Value,
}

async fn create_item(
  State(state): State<AppState>,
  Json(req): Json<CreateItemRequest>,
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

  match items::insert(&state.db, &item).await {
    Ok(()) => {
      // Append ItemCreated event.
      let event = events::new_item_event(
        item.id,
        EventType::ItemCreated,
        Actor::System,
        Locality::Local,
        json!({ "source": item.source }),
      );
      let _ = events::append(&state.db, &event).await;

      (StatusCode::CREATED, Json(json!({ "item": item }))).into_response()
    }
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

// ── Get ───────────────────────────────────────────────────────────────────────

async fn get_item(
  State(state): State<AppState>,
  Path(id): Path<Uuid>,
) -> impl IntoResponse {
  match items::get(&state.db, id).await {
    Ok(item) => {
      // Include the event log in the response.
      let event_log = events::for_item(&state.db, id).await.unwrap_or_default();
      Json(json!({ "item": item, "events": event_log })).into_response()
    }
    Err(items::ItemsError::NotFound(_)) => {
      (StatusCode::NOT_FOUND, Json(json!({ "error": "item not found" })))
        .into_response()
    }
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

// ── Invoke action ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ActionRequest {
  pub activity_id: String,
  #[serde(default)]
  pub params: serde_json::Value,
}

async fn invoke_action(
  State(state): State<AppState>,
  Path(id): Path<Uuid>,
  Json(req): Json<ActionRequest>,
) -> impl IntoResponse {
  // Record the action as an event. Actual execution is delegated to the
  // topic system (not yet wired in the server — topics are registered at
  // startup and called from worker tasks or directly here).
  let event = events::new_item_event(
    id,
    EventType::ActionTaken,
    Actor::Human,
    Locality::Local,
    json!({
      "activity_id": req.activity_id,
      "params": req.params,
    }),
  );

  match events::append(&state.db, &event).await {
    Ok(()) => Json(json!({ "event_id": event.id })).into_response(),
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

// ── Ack ───────────────────────────────────────────────────────────────────────
// Ack is the iron-mode gate: only this advances the queue to the next item.

async fn ack_item(
  State(state): State<AppState>,
  Path(id): Path<Uuid>,
) -> impl IntoResponse {
  // Append the ack event.
  let event = events::new_item_event(
    id,
    EventType::ActionTaken,
    Actor::Human,
    Locality::Local,
    json!({ "activity_id": "ack" }),
  );
  let _ = events::append(&state.db, &event).await;

  // Update item projection to Done.
  match items::update_state(&state.db, id, ItemState::Done).await {
    Ok(()) => Json(json!({ "status": "done" })).into_response(),
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

// ── Next item (iron mode) ─────────────────────────────────────────────────────

async fn next_item(State(state): State<AppState>) -> impl IntoResponse {
  match items::next_human_item(&state.db).await {
    Ok(Some(item)) => Json(json!({ "item": item })).into_response(),
    Ok(None) => {
      Json(json!({ "item": null, "message": "queue is empty" })).into_response()
    }
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

// ── Count ─────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct CountResponse {
  count: i64,
}

async fn queue_count(State(state): State<AppState>) -> impl IntoResponse {
  match items::human_queue_count(&state.db).await {
    Ok(count) => Json(CountResponse { count }).into_response(),
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

fn parse_state(s: &str) -> ItemState {
  match s {
    "intake_pending" => ItemState::IntakePending,
    "human_pending" => ItemState::HumanPending,
    "auto_handled" => ItemState::AutoHandled,
    "done" => ItemState::Done,
    _ => ItemState::HumanPending,
  }
}
