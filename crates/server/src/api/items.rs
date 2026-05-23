//! Item CRUD and action endpoints.

use crate::tx_error::TxOpError;
use crate::web_base::AppState;
use axum::{
  extract::{Path, Query, State},
  http::StatusCode,
  response::IntoResponse,
  routing::{get, post},
  Json, Router,
};
use chrono::{Duration as ChronoDuration, Utc};
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  item::Item,
  queue as queue_names,
};
use hyuqueue_store::{events, items, queue};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

/// Worker id used by the API's own queue interactions.  HTTP requests
/// are per-process actions; a fresh worker id per call keeps queue
/// ops simple without tracking per-session state.
fn api_worker_id() -> String {
  format!("api-{}", Uuid::new_v4())
}

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
  /// Optional source filter (e.g. ?source=email).
  pub source: Option<String>,
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
  match items::list(
    state.db.pool(),
    params.source.as_deref(),
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

  // Item creation is a three-step persist (event, insert, enqueue)
  // that must succeed atomically.  Compose in one transaction.
  match create_item_tx(&state, &item).await {
    Ok(()) => {
      (StatusCode::CREATED, Json(json!({ "item": item }))).into_response()
    }
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

async fn create_item_tx(
  state: &AppState,
  item: &Item,
) -> Result<(), TxOpError> {
  let mut tx = state.db.pool().begin().await.map_err(TxOpError::BeginTx)?;
  let event = events::new_item_event(
    item.id,
    EventType::ItemCreated,
    Actor::System,
    Locality::Local,
    json!({ "source": item.source }),
  );
  events::append(&mut *tx, &event).await?;
  items::insert(&mut *tx, item).await?;
  queue::enqueue(&mut *tx, queue_names::INTAKE, item.id, 0).await?;
  tx.commit().await.map_err(TxOpError::CommitTx)?;
  Ok(())
}

// ── Get ───────────────────────────────────────────────────────────────────────

async fn get_item(
  State(state): State<AppState>,
  Path(id): Path<Uuid>,
) -> impl IntoResponse {
  match items::get(state.db.pool(), id).await {
    Ok(item) => {
      let event_log = events::for_item(state.db.pool(), id)
        .await
        .unwrap_or_default();
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

  match events::append(state.db.pool(), &event).await {
    Ok(()) => Json(json!({ "event_id": event.id })).into_response(),
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

// ── Ack ───────────────────────────────────────────────────────────────────────
// Ack is the human's halt signal.  At the queue layer it claims the
// item from the human queue, appends the ack event, and moves the
// item to the outtake queue — all three in a single transaction so
// the ack either fully happens or doesn't happen at all.

async fn ack_item(
  State(state): State<AppState>,
  Path(id): Path<Uuid>,
) -> impl IntoResponse {
  let worker_id = api_worker_id();
  let lease = ChronoDuration::seconds(30);

  match ack_item_tx(&state, id, &worker_id, lease).await {
    Ok(()) => Json(json!({ "status": "done" })).into_response(),
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

async fn ack_item_tx(
  state: &AppState,
  id: Uuid,
  worker_id: &str,
  lease: ChronoDuration,
) -> Result<(), TxOpError> {
  let mut tx = state.db.pool().begin().await.map_err(TxOpError::BeginTx)?;

  // HTTP API is stateless — each request generates a fresh worker
  // id, so we can't honor a prior lease.  force_claim bypasses
  // lease coordination; the HTTP path is implicitly authoritative
  // for the local operator.  TUI/Emacs clients with persistent
  // worker ids should hold a real lease and use the worker-id-
  // respecting API instead.
  queue::force_claim(&mut *tx, queue_names::HUMAN, id, worker_id, lease)
    .await?;

  let event = events::new_item_event(
    id,
    EventType::ActionTaken,
    Actor::Human,
    Locality::Local,
    json!({ "activity_id": "ack" }),
  );
  events::append(&mut *tx, &event).await?;

  queue::move_item(
    &mut tx,
    id,
    queue_names::HUMAN,
    queue_names::OUTTAKE,
    worker_id,
  )
  .await?;

  tx.commit().await.map_err(TxOpError::CommitTx)?;
  Ok(())
}

// ── Next item (iron mode) ─────────────────────────────────────────────────────
// Returns the head of the human queue without claiming it (the
// caller, typically the TUI, displays it; a separate ack call takes
// the lease and completes it).

async fn next_item(State(state): State<AppState>) -> impl IntoResponse {
  let entries =
    match queue::list(state.db.pool(), queue_names::HUMAN, &Default::default())
      .await
    {
      Ok(es) => es,
      Err(e) => {
        return (
          StatusCode::INTERNAL_SERVER_ERROR,
          Json(json!({ "error": e.to_string() })),
        )
          .into_response();
      }
    };

  let next = entries.into_iter().find(|e| {
    e.claimed_by.is_none() || e.lease_expires_at.is_some_and(|t| t < Utc::now())
  });

  match next {
    None => {
      Json(json!({ "item": null, "message": "queue is empty" })).into_response()
    }
    Some(entry) => match items::get(state.db.pool(), entry.item_id).await {
      Ok(item) => Json(json!({ "item": item })).into_response(),
      Err(e) => (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
      )
        .into_response(),
    },
  }
}

// ── Count ─────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct CountResponse {
  count: i64,
}

async fn queue_count(State(state): State<AppState>) -> impl IntoResponse {
  match queue::depth(state.db.pool(), queue_names::HUMAN).await {
    Ok(count) => Json(CountResponse { count }).into_response(),
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}
