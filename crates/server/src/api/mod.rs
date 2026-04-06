pub mod items;
pub mod push;
pub mod queues;

use crate::web_base::AppState;
use axum::Router;

pub fn api_router() -> Router<AppState> {
  Router::new()
    .nest("/items", items::router())
    .nest("/queues", queues::router())
    .route("/push", axum::routing::post(push::handle_push))
}
