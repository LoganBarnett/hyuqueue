//! hyuqueue-server — the daemon.
//!
//! Owns: SQLite, LLM workers (intake + review), outbound signal delivery,
//! and the HTTP API that all clients talk to.
//!
//! # LLM Development Guidelines
//! - Keep configuration in config.rs.
//! - Keep base web functionality (healthz, metrics, openapi) in web_base.rs.
//! - Add new API routes in api/ modules, not here.
//! - Maintain the staged configuration pattern (CliRaw -> ConfigFileRaw -> Config)
//! - Use semantic error types with thiserror — no anyhow.
//! - Preserve systemd::notify_ready() and systemd::spawn_watchdog() after bind.

mod logging;
mod systemd;

use hyuqueue_server::{api, auth, config, web_base, workers};
use hyuqueue_store::Db;

use axum::{routing::get, Router};
use clap::Parser;
use config::{CliRaw, Config, ConfigError};
use logging::init_logging;
use std::sync::Arc;
use thiserror::Error;
use tokio::signal;
use tower_http::trace::TraceLayer;
use tower_sessions::{cookie::SameSite, MemoryStore, SessionManagerLayer};
use tracing::{error, info};
use web_base::{AppState, AppStateError};

#[derive(Debug, Error)]
enum ApplicationError {
  #[error("Failed to load configuration: {0}")]
  Config(#[from] ConfigError),

  #[error("Failed to open database at '{path}': {source}")]
  Database {
    path: String,
    #[source]
    source: hyuqueue_store::db::DbError,
  },

  #[error("Failed to initialize application state: {0}")]
  StateInit(#[from] AppStateError),

  #[error("Failed to bind server to {address}: {source}")]
  Bind {
    address: String,
    #[source]
    source: std::io::Error,
  },

  #[error("Server runtime error: {0}")]
  Runtime(#[source] std::io::Error),
}

#[tokio::main]
async fn main() -> Result<(), ApplicationError> {
  let cli = CliRaw::parse();

  let config =
    Config::from_cli_and_file(cli).map_err(ApplicationError::Config)?;

  init_logging(config.log_level, config.log_format);

  info!("Starting hyuqueue-server");
  info!(db = %config.db_path, "Opening database");

  let db = Db::open(&config.db_path).await.map_err(|source| {
    ApplicationError::Database {
      path: config.db_path.clone(),
      source,
    }
  })?;

  info!("Database ready");

  let llm_config = Arc::new(config.llm.clone());
  let _workers = workers::spawn_all(db.clone(), llm_config);

  info!("Workers started");

  let state = AppState::init(&config, db).await.map_err(|e| {
    error!("Failed to initialize application state: {}", e);
    ApplicationError::StateInit(e)
  })?;

  let app = create_app(state);

  info!("Binding to {}", config.listen_address);

  let listener = tokio_listener::Listener::bind(
    &config.listen_address,
    &tokio_listener::SystemOptions::default(),
    &tokio_listener::UserOptions::default(),
  )
  .await
  .map_err(|source| {
    error!("Failed to bind to {}: {}", config.listen_address, source);
    ApplicationError::Bind {
      address: config.listen_address.to_string(),
      source,
    }
  })?;

  info!("hyuqueue-server listening on {}", config.listen_address);

  systemd::notify_ready();
  systemd::spawn_watchdog();

  axum::serve(listener, app.into_make_service())
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(ApplicationError::Runtime)?;

  info!("hyuqueue-server shut down");
  Ok(())
}

fn create_app(state: AppState) -> Router {
  let session_store = MemoryStore::default();
  // SameSite::Lax is required: Strict suppresses the session cookie on the
  // cross-site redirect back from the OIDC provider.
  let session_layer = SessionManagerLayer::new(session_store)
    .with_secure(true)
    .with_same_site(SameSite::Lax);

  let auth_router = Router::new()
    .route("/auth/login", get(auth::login_handler))
    .route("/auth/callback", get(auth::callback_handler))
    .route("/auth/logout", get(auth::logout_handler))
    .with_state(state.clone());

  let api_router = api::api_router().with_state(state.clone());

  web_base::base_router(state)
    .merge(auth_router)
    .nest("/api/v1", api_router)
    .layer(session_layer)
    .layer(TraceLayer::new_for_http())
}

async fn shutdown_signal() {
  let ctrl_c = async {
    signal::ctrl_c()
      .await
      .expect("failed to install Ctrl+C handler");
  };

  #[cfg(unix)]
  let terminate = async {
    signal::unix::signal(signal::unix::SignalKind::terminate())
      .expect("failed to install SIGTERM handler")
      .recv()
      .await;
  };

  #[cfg(not(unix))]
  let terminate = std::future::pending::<()>();

  tokio::select! {
    _ = ctrl_c => { info!("Received Ctrl+C, shutting down") },
    _ = terminate => { info!("Received SIGTERM, shutting down") },
  }
}
