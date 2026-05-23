//! Intake LLM worker — dequeues items from the intake queue and runs
//! them through the agentic loop in [`intake_loop`].
//!
//! Outcome handling:
//!
//! - `AutoResolved` → complete the item from the intake queue, append
//!   the analysis event, append any activity events the loop
//!   produced.  All in one transaction.
//! - `DeferredToHuman` → move the item from the intake queue to the
//!   human queue, append the analysis event, append any activity
//!   events.  All in one transaction.

use crate::config::LlmConfig;
use crate::topics::TopicRegistry;
use crate::tx_error::TxOpError;
use crate::workers::intake_loop::{self, IntakeOutcome};
use chrono::Duration as ChronoDuration;
use hyuqueue_core::{
  event::{Actor, Event, EventType, Locality},
  queue as queue_names,
};
use hyuqueue_lib::llm::OpenAiClient;
use hyuqueue_store::{events, queue, Db};
use serde_json::json;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const LEASE: ChronoDuration = ChronoDuration::seconds(30);
const TURN_BUDGET: u32 = 8;

pub async fn run(
  db: Db,
  llm_config: Arc<LlmConfig>,
  registry: Arc<TopicRegistry>,
) {
  let client =
    OpenAiClient::new(llm_config.base_url.clone(), llm_config.api_key.clone());
  let worker_id = format!("intake-{}", Uuid::new_v4());

  info!(worker_id = %worker_id, "Intake worker started");

  loop {
    match process_next(
      &db,
      &client,
      &registry,
      &llm_config.intake_model,
      &worker_id,
    )
    .await
    {
      Ok(true) => {}
      Ok(false) => sleep(POLL_INTERVAL).await,
      Err(e) => {
        error!("Intake worker error: {e}");
        sleep(POLL_INTERVAL).await;
      }
    }
  }
}

async fn process_next(
  db: &Db,
  client: &OpenAiClient,
  registry: &TopicRegistry,
  model: &str,
  worker_id: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
  let Some(entry) =
    queue::dequeue_one(db, queue_names::INTAKE, worker_id, LEASE).await?
  else {
    return Ok(false);
  };
  let item_id = entry.item_id;

  let item = match hyuqueue_store::items::get(db.pool(), item_id).await {
    Ok(i) => i,
    Err(e) => {
      warn!(item_id = %item_id, "Could not fetch item for intake: {e}");
      if let Err(release_err) =
        queue::release(db.pool(), queue_names::INTAKE, item_id, worker_id).await
      {
        warn!(item_id = %item_id, "Failed to release claim: {release_err}");
      }
      return Ok(true);
    }
  };

  let outcome = match intake_loop::run_loop(
    client,
    registry,
    &item,
    model,
    TURN_BUDGET,
  )
  .await
  {
    Ok(o) => o,
    Err(e) => {
      warn!(item_id = %item_id, "Intake loop failed: {e}. Escalating to human.");
      IntakeOutcome::DeferredToHuman {
        reason: format!("intake loop error: {e}"),
        transcript: vec![],
        activity_events: vec![],
      }
    }
  };

  if let Err(e) = finalize_outcome(db, item_id, worker_id, model, outcome).await
  {
    warn!(item_id = %item_id, "Failed to finalize intake outcome: {e}");
  }

  Ok(true)
}

async fn finalize_outcome(
  db: &Db,
  item_id: Uuid,
  worker_id: &str,
  model: &str,
  outcome: IntakeOutcome,
) -> Result<(), TxOpError> {
  let mut tx = db.pool().begin().await.map_err(TxOpError::BeginTx)?;

  let (analysis_event, activity_events, auto_resolved) = match outcome {
    IntakeOutcome::AutoResolved {
      summary,
      transcript,
      activity_events,
    } => {
      let event = build_analysis_event(
        item_id,
        model,
        true,
        Some(&summary),
        None,
        transcript.len() as u32,
      );
      (event, activity_events, true)
    }
    IntakeOutcome::DeferredToHuman {
      reason,
      transcript,
      activity_events,
    } => {
      let event = build_analysis_event(
        item_id,
        model,
        false,
        None,
        Some(&reason),
        transcript.len() as u32,
      );
      (event, activity_events, false)
    }
  };

  // Append activity events in invocation order on the shared
  // transaction.  Sequential by necessity: all calls re-borrow
  // `&mut *tx`, so no fan-out is possible, and the audit trail
  // should reflect the order the LLM actually fired things.
  for ev in &activity_events {
    events::append(&mut *tx, ev).await?;
  }
  events::append(&mut *tx, &analysis_event).await?;

  if auto_resolved {
    queue::complete(&mut *tx, queue_names::INTAKE, item_id, worker_id).await?;
  } else {
    queue::move_item(
      &mut tx,
      item_id,
      queue_names::INTAKE,
      queue_names::HUMAN,
      worker_id,
    )
    .await?;
  }

  tx.commit().await.map_err(TxOpError::CommitTx)?;
  Ok(())
}

fn build_analysis_event(
  item_id: Uuid,
  model: &str,
  confident: bool,
  auto_action: Option<&str>,
  uncertainty_reason: Option<&str>,
  turn_count: u32,
) -> Event {
  events::new_item_event(
    item_id,
    EventType::IntakeLlmAnalysis,
    Actor::IntakeLlm,
    Locality::Local,
    json!({
      "model": model,
      "confident": confident,
      "auto_action": auto_action,
      "uncertainty_reason": uncertainty_reason,
      "turn_count": turn_count,
    }),
  )
}
