//! Background workers — run as tokio tasks inside the server daemon.
//!
//! Three workers, three queues:
//! - intake:   processes IntakePending items through the intake LLM
//! - review:   processes Done items through the review LLM (slower)
//! - outbound: delivers upstream signals to remote hyuqueue instances

pub mod intake;
pub mod outbound;
pub mod review;

use hyuqueue_store::Db;
use std::sync::Arc;
use tokio::task::JoinHandle;

use crate::config::LlmConfig;

pub struct WorkerHandles {
  pub intake: JoinHandle<()>,
  pub review: JoinHandle<()>,
  pub outbound: JoinHandle<()>,
}

/// Spawn all background workers. Call once at server startup.
pub fn spawn_all(db: Db, llm_config: Arc<LlmConfig>) -> WorkerHandles {
  WorkerHandles {
    intake: tokio::spawn(intake::run(db.clone(), llm_config.clone())),
    review: tokio::spawn(review::run(db.clone(), llm_config.clone())),
    outbound: tokio::spawn(outbound::run(db)),
  }
}
