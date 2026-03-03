//! hyuqueue-server — the daemon.
//!
//! Owns: SQLite, LLM workers (intake + review), outbound signal delivery,
//! and the HTTP API that all clients talk to.
//!
//! # LLM Development Guidelines
//! - Keep configuration in config.rs.
//! - Add new API routes in api/ modules, not here.
//! - Workers are in workers/ — each is a long-running tokio task.
//! - Use semantic error types with thiserror — no anyhow.

use hyuqueue_server::{api, config, logging, workers};
use hyuqueue_store::Db;

use axum::Router;
use clap::Parser;
use config::{CliRaw, Config, ConfigError};
use logging::init_logging;
use std::sync::Arc;
use thiserror::Error;
use tokio::signal;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

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

  let state = api::AppState::new(db);
  let app = Router::new()
    .merge(api::router(state))
    .layer(TraceLayer::new_for_http());

  let addr = config.bind_address();
  info!(address = %addr, "Binding HTTP server");

  let listener =
    tokio::net::TcpListener::bind(&addr).await.map_err(|source| {
      ApplicationError::Bind {
        address: addr.clone(),
        source,
      }
    })?;

  info!(address = %addr, "hyuqueue-server listening");
  info!(address = %addr, "Health check: http://{}/healthz", addr);
  info!(address = %addr, "API: http://{}/api/v1", addr);

  axum::serve(listener, app)
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(ApplicationError::Runtime)?;

  info!("hyuqueue-server shut down");
  Ok(())
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
