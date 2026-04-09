//! Background workers — run as tokio tasks inside the server daemon.
//!
//! Four workers:
//! - ingest:   polls registered topics for new items
//! - intake:   processes IntakePending items through the intake LLM
//! - review:   processes Done items through the review LLM (slower)
//! - outbound: delivers upstream signals to remote hyuqueue instances

pub mod ingest;
pub mod intake;
pub mod outbound;
pub mod review;

use crate::config::LlmConfig;
use crate::topics::TopicRegistry;
use hyuqueue_store::Db;
use std::sync::Arc;
use tokio::task::JoinHandle;

pub struct WorkerHandles {
  pub ingest: JoinHandle<()>,
  pub intake: JoinHandle<()>,
  pub review: JoinHandle<()>,
  pub outbound: JoinHandle<()>,
}

/// Spawn all background workers. Call once at server startup.
pub fn spawn_all(
  db: Db,
  llm_config: Arc<LlmConfig>,
  topic_registry: Arc<TopicRegistry>,
) -> WorkerHandles {
  WorkerHandles {
    ingest: tokio::spawn(ingest::run(db.clone(), topic_registry)),
    intake: tokio::spawn(intake::run(db.clone(), llm_config.clone())),
    review: tokio::spawn(review::run(db.clone(), llm_config.clone())),
    outbound: tokio::spawn(outbound::run(db)),
  }
}
