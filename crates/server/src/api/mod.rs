pub mod items;
pub mod push;
pub mod queues;

use axum::{Json, Router, routing::get};
use hyuqueue_store::Db;
use serde_json::json;

/// Shared server state threaded through all API handlers.
#[derive(Clone)]
pub struct AppState {
  pub db: Db,
}

impl AppState {
  pub fn new(db: Db) -> Self {
    Self { db }
  }
}

pub fn router(state: AppState) -> Router {
  Router::new()
    .route("/healthz", get(healthz))
    .nest("/api/v1", api_router())
    .with_state(state)
}

fn api_router() -> Router<AppState> {
  Router::new()
    .nest("/items", items::router())
    .nest("/queues", queues::router())
    .route("/push", axum::routing::post(push::handle_push))
}

async fn healthz() -> Json<serde_json::Value> {
  Json(json!({ "status": "healthy" }))
}
