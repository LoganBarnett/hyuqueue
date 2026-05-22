pub mod items;
pub mod push;

use crate::web_base::AppState;
use axum::Router;

pub fn api_router() -> Router<AppState> {
  Router::new()
    .nest("/items", items::router())
    .route("/push", axum::routing::post(push::handle_push))
}
