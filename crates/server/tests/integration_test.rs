use axum::{
  body::Body,
  http::{Request, StatusCode},
  Router,
};
use hyuqueue_server::web_base::{base_router, AppState};
use hyuqueue_store::Db;
use openidconnect::{
  core::{CoreProviderMetadata, CoreResponseType},
  AuthUrl, ClientId, IssuerUrl, JsonWebKeySetUrl, ResponseTypes, TokenUrl,
  UserInfoUrl,
};
use prometheus::{IntCounter, Registry};
use std::{path::PathBuf, sync::Arc};
use tower::ServiceExt;
use tower_sessions::{cookie::SameSite, MemoryStore, SessionManagerLayer};

use hyuqueue_server::config::LlmConfig;
use hyuqueue_server::topics::TopicRegistry;

// ── state helpers ────────────────────────────────────────────────────────────

async fn stub_state_no_auth(frontend_path: PathBuf) -> AppState {
  let db = Db::open(":memory:").await.unwrap();
  let registry = Registry::new();
  let request_counter =
    IntCounter::new("http_requests_total", "Total HTTP requests")
      .expect("counter creation");
  registry
    .register(Box::new(request_counter.clone()))
    .expect("counter registration");

  AppState {
    db,
    registry: Arc::new(registry),
    request_counter,
    frontend_path,
    oidc_client: None,
    llm_config: Arc::new(LlmConfig {
      base_url: "http://localhost:11434/v1".to_string(),
      intake_model: "llama3.2".to_string(),
      review_model: "llama3.2".to_string(),
      api_key: None,
    }),
    topics: Arc::new(TopicRegistry::empty()),
  }
}

async fn stub_state_with_auth(frontend_path: PathBuf) -> AppState {
  let db = Db::open(":memory:").await.unwrap();
  let registry = Registry::new();
  let request_counter =
    IntCounter::new("http_requests_total", "Total HTTP requests")
      .expect("counter creation");
  registry
    .register(Box::new(request_counter.clone()))
    .expect("counter registration");

  let issuer = IssuerUrl::new("https://stub.invalid".to_string()).unwrap();
  let provider_metadata = CoreProviderMetadata::new(
    issuer,
    AuthUrl::new("https://stub.invalid/authorize".to_string()).unwrap(),
    JsonWebKeySetUrl::new("https://stub.invalid/jwks".to_string()).unwrap(),
    vec![ResponseTypes::new(vec![CoreResponseType::Code])],
    vec![],
    vec![],
    Default::default(),
  )
  .set_token_endpoint(Some(
    TokenUrl::new("https://stub.invalid/token".to_string()).unwrap(),
  ))
  .set_userinfo_endpoint(Some(
    UserInfoUrl::new("https://stub.invalid/userinfo".to_string()).unwrap(),
  ));

  let oidc_client = openidconnect::core::CoreClient::from_provider_metadata(
    provider_metadata,
    ClientId::new("test-client".to_string()),
    Some(openidconnect::ClientSecret::new("test-secret".to_string())),
  )
  .set_redirect_uri(
    openidconnect::RedirectUrl::new(
      "https://stub.invalid/callback".to_string(),
    )
    .unwrap(),
  );

  AppState {
    db,
    registry: Arc::new(registry),
    request_counter,
    frontend_path,
    oidc_client: Some(Arc::new(oidc_client)),
    llm_config: Arc::new(LlmConfig {
      base_url: "http://localhost:11434/v1".to_string(),
      intake_model: "llama3.2".to_string(),
      review_model: "llama3.2".to_string(),
      api_key: None,
    }),
    topics: Arc::new(TopicRegistry::empty()),
  }
}

async fn state_without_frontend() -> AppState {
  stub_state_no_auth(PathBuf::from("/nonexistent")).await
}

/// Wraps `base_router` with auth routes and a session layer, mirroring
/// the production `create_app` structure.
fn app_with_session(state: AppState) -> Router {
  use axum::routing::get;
  use hyuqueue_server::auth;

  let session_store = MemoryStore::default();
  let session_layer = SessionManagerLayer::new(session_store)
    .with_secure(false)
    .with_same_site(SameSite::Lax);

  let auth_router = Router::new()
    .route("/auth/login", get(auth::login_handler))
    .route("/auth/callback", get(auth::callback_handler))
    .route("/auth/logout", get(auth::logout_handler))
    .with_state(state.clone());

  base_router(state).merge(auth_router).layer(session_layer)
}

// ── existing route tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_healthz_endpoint() {
  let app = base_router(state_without_frontend().await);

  let response = app
    .oneshot(
      Request::builder()
        .uri("/healthz")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);

  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let body_str = String::from_utf8(body.to_vec()).unwrap();

  assert!(body_str.contains("healthy"));
}

#[tokio::test]
async fn test_metrics_endpoint() {
  let app = base_router(state_without_frontend().await);

  let response = app
    .oneshot(
      Request::builder()
        .uri("/metrics")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);

  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let body_str = String::from_utf8(body.to_vec()).unwrap();

  assert!(
    body_str.contains("http_requests_total"),
    "Metrics should contain http_requests_total counter"
  );
}

#[tokio::test]
async fn test_openapi_json_endpoint() {
  let app = base_router(state_without_frontend().await);

  let response = app
    .oneshot(
      Request::builder()
        .uri("/api-docs/openapi.json")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);

  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let body_str = String::from_utf8(body.to_vec()).unwrap();

  assert!(body_str.contains("openapi"), "Response should be an OpenAPI spec");
  assert!(body_str.contains("/healthz"), "Spec should document /healthz");
  assert!(body_str.contains("/metrics"), "Spec should document /metrics");
}

#[tokio::test]
async fn test_scalar_ui_endpoint() {
  let app = base_router(state_without_frontend().await);

  let response = app
    .oneshot(
      Request::builder()
        .uri("/scalar")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);

  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();

  assert!(
    body.starts_with(b"<!doctype html>")
      || body.starts_with(b"<!DOCTYPE html>"),
    "Scalar endpoint should return HTML"
  );
}

#[tokio::test]
async fn test_spa_fallback_serves_index_html() {
  let frontend_dir = tempfile::tempdir().unwrap();
  std::fs::write(
    frontend_dir.path().join("index.html"),
    b"<!doctype html><title>hyuqueue</title>",
  )
  .unwrap();

  let state = stub_state_no_auth(frontend_dir.path().to_path_buf()).await;
  let app = base_router(state);

  for path in ["/some-page", "/nested/route", "/unknown"] {
    let response = app
      .clone()
      .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(
      response.status(),
      StatusCode::OK,
      "expected 200 for SPA path {path}"
    );
  }
}

// ── /me endpoint tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_me_no_oidc() {
  let state = stub_state_no_auth(PathBuf::from("/nonexistent")).await;
  let app = app_with_session(state);

  let response = app
    .oneshot(Request::builder().uri("/me").body(Body::empty()).unwrap())
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);

  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

  assert_eq!(json["name"], "admin");
  assert_eq!(json["auth_enabled"], false);
}

#[tokio::test]
async fn test_me_with_oidc_no_session() {
  let state = stub_state_with_auth(PathBuf::from("/nonexistent")).await;
  let app = app_with_session(state);

  let response = app
    .oneshot(Request::builder().uri("/me").body(Body::empty()).unwrap())
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);

  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

  assert_eq!(json["name"], "anonymous");
  assert_eq!(json["auth_enabled"], true);
}

// ── auth route guard tests ───────────────────────────────────────────────────

#[tokio::test]
async fn test_auth_routes_return_404_without_oidc() {
  let state = stub_state_no_auth(PathBuf::from("/nonexistent")).await;
  let app = app_with_session(state);

  for path in ["/auth/login", "/auth/logout"] {
    let response = app
      .clone()
      .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(
      response.status(),
      StatusCode::NOT_FOUND,
      "expected 404 for {path} without OIDC"
    );
  }

  // callback needs query params; without them Axum rejects before our
  // guard, but we can confirm it still doesn't 500 or 200.
  let response = app
    .oneshot(
      Request::builder()
        .uri("/auth/callback?code=x&state=y")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_auth_login_redirects_with_oidc() {
  let state = stub_state_with_auth(PathBuf::from("/nonexistent")).await;
  let app = app_with_session(state);

  let response = app
    .oneshot(
      Request::builder()
        .uri("/auth/login")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  // The stub provider's authorize URL should trigger a redirect.
  assert_eq!(response.status(), StatusCode::SEE_OTHER);
  let location = response
    .headers()
    .get("location")
    .expect("redirect should have Location header")
    .to_str()
    .unwrap();
  assert!(
    location.contains("stub.invalid"),
    "redirect should point at the stub OIDC provider"
  );
}

// ── item API path-parameter tests ────────────────────────────────────────────

/// Build a router that includes the API routes, mirroring production.
fn app_with_api(state: AppState) -> Router {
  use hyuqueue_server::api;

  let session_store = MemoryStore::default();
  let session_layer = SessionManagerLayer::new(session_store)
    .with_secure(false)
    .with_same_site(SameSite::Lax);

  let api_router = api::api_router().with_state(state.clone());

  base_router(state)
    .nest("/api/v1", api_router)
    .layer(session_layer)
}

/// Create an item via POST then fetch it by ID via GET.
/// This exercises the `{id}` path parameter in the items router.
#[tokio::test]
async fn test_get_item_by_id() {
  use hyuqueue_core::queue::Queue;

  let state = state_without_frontend().await;

  // Seed a queue so we have a valid queue_id.
  let queue = Queue {
    id: uuid::Uuid::new_v4(),
    name: "test-queue".to_string(),
    tags: vec![],
    config: serde_json::json!({}),
    created_at: chrono::Utc::now(),
    updated_at: chrono::Utc::now(),
  };
  hyuqueue_store::queues::insert(&state.db, &queue)
    .await
    .unwrap();

  let app = app_with_api(state);

  // POST to create an item.
  let create_body = serde_json::json!({
    "title": "test item",
    "source": "integration-test",
    "queue_id": queue.id,
  });

  let create_resp = app
    .clone()
    .oneshot(
      Request::builder()
        .method("POST")
        .uri("/api/v1/items")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&create_body).unwrap()))
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(
    create_resp.status(),
    StatusCode::CREATED,
    "POST /api/v1/items should return 201"
  );

  let body = axum::body::to_bytes(create_resp.into_body(), usize::MAX)
    .await
    .unwrap();
  let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
  let item_id = created["item"]["id"]
    .as_str()
    .expect("response should contain item.id");

  // GET the item by ID.
  let get_resp = app
    .oneshot(
      Request::builder()
        .uri(format!("/api/v1/items/{item_id}"))
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(
    get_resp.status(),
    StatusCode::OK,
    "GET /api/v1/items/{{id}} should return 200"
  );

  let body = axum::body::to_bytes(get_resp.into_body(), usize::MAX)
    .await
    .unwrap();
  let fetched: serde_json::Value = serde_json::from_slice(&body).unwrap();
  assert_eq!(fetched["item"]["id"], item_id);
}

// ── config tests ─────────────────────────────────────────────────────────────

#[test]
fn test_config_no_oidc() {
  use hyuqueue_server::config::{CliRaw, Config};

  let cli = CliRaw {
    log_level: None,
    log_format: None,
    config: None,
    listen: None,
    db_path: None,
    frontend_path: None,
    base_url: Some("https://example.com".to_string()),
    oidc_issuer: None,
    oidc_client_id: None,
    oidc_client_secret_file: None,
  };

  let config = Config::from_cli_and_file(cli).unwrap();
  assert!(config.oidc.is_none());
}

#[test]
fn test_config_full_oidc() {
  use hyuqueue_server::config::{CliRaw, Config};

  let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .join("tests/fixtures/oidc-client-secret");

  let cli = CliRaw {
    log_level: None,
    log_format: None,
    config: None,
    listen: None,
    db_path: None,
    frontend_path: None,
    base_url: Some("https://example.com".to_string()),
    oidc_issuer: Some("https://sso.example.com".to_string()),
    oidc_client_id: Some("my-client".to_string()),
    oidc_client_secret_file: Some(fixture),
  };

  let config = Config::from_cli_and_file(cli).unwrap();
  let oidc = config.oidc.expect("OIDC config should be Some");
  assert_eq!(oidc.issuer, "https://sso.example.com");
  assert_eq!(oidc.client_id, "my-client");
  assert_eq!(oidc.client_secret, "test-secret-not-for-production");
}

#[test]
fn test_config_partial_oidc_errors() {
  use hyuqueue_server::config::{CliRaw, Config};

  let cli = CliRaw {
    log_level: None,
    log_format: None,
    config: None,
    listen: None,
    db_path: None,
    frontend_path: None,
    base_url: Some("https://example.com".to_string()),
    oidc_issuer: Some("https://sso.example.com".to_string()),
    oidc_client_id: None,
    oidc_client_secret_file: None,
  };

  let err = Config::from_cli_and_file(cli).unwrap_err();
  let msg = err.to_string();
  assert!(
    msg.contains("partial OIDC") && msg.contains("missing"),
    "error should describe partial OIDC config, got: {msg}"
  );
}
