use crate::web_base::AppState;
use axum::{
  extract::State, http::StatusCode, response::IntoResponse, routing::get, Json,
  Router,
};
use chrono::Utc;
use hyuqueue_core::queue::Queue;
use hyuqueue_store::queues;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
  Router::new().route("/", get(list_queues).post(create_queue))
}

async fn list_queues(State(state): State<AppState>) -> impl IntoResponse {
  match queues::list(&state.db).await {
    Ok(qs) => Json(json!({ "queues": qs })).into_response(),
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}

#[derive(Debug, Deserialize)]
pub struct CreateQueueRequest {
  pub name: String,
  #[serde(default)]
  pub tags: Vec<String>,
}

async fn create_queue(
  State(state): State<AppState>,
  Json(req): Json<CreateQueueRequest>,
) -> impl IntoResponse {
  let now = Utc::now();
  let queue = Queue {
    id: Uuid::new_v4(),
    name: req.name,
    tags: req.tags,
    config: serde_json::Value::Object(Default::default()),
    created_at: now,
    updated_at: now,
  };

  match queues::insert(&state.db, &queue).await {
    Ok(()) => {
      (StatusCode::CREATED, Json(json!({ "queue": queue }))).into_response()
    }
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({ "error": e.to_string() })),
    )
      .into_response(),
  }
}
