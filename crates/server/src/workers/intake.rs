//! Intake LLM worker — fast, inline with item ingestion.
//!
//! Dequeues items from the `intake` queue, runs them through the
//! intake LLM, and either:
//!
//! - Completes the item from the intake queue (LLM was confident,
//!   item is auto-handled).
//! - Moves the item to the `human` queue (LLM was uncertain).
//!
//! The uncertainty reason is embedded in the `IntakeLlmAnalysis`
//! event and shown to the human in the queue UI.
//!
//! The agentic-loop rewrite (see the "Intake LLM as agentic loop"
//! TODO in tasks.org) will replace the single-shot LLM call below
//! with a multi-turn dispatcher; the outer queue-dispatch shell
//! stays the same.

use crate::config::LlmConfig;
use chrono::Duration as ChronoDuration;
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  queue as queue_names,
};
use hyuqueue_lib::llm::{
  CompletionRequest, LlmClient, Message, OpenAiClient, Role,
};
use hyuqueue_store::{events, items, queue, Db};
use serde_json::json;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const LEASE: ChronoDuration = ChronoDuration::seconds(30);

pub async fn run(db: Db, llm_config: Arc<LlmConfig>) {
  let client =
    OpenAiClient::new(llm_config.base_url.clone(), llm_config.api_key.clone());
  let worker_id = format!("intake-{}", Uuid::new_v4());

  info!(worker_id = %worker_id, "Intake worker started");

  loop {
    match process_next(&db, &client, &llm_config.intake_model, &worker_id).await
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
  model: &str,
  worker_id: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
  let Some(entry) =
    queue::dequeue_one(db, queue_names::INTAKE, worker_id, LEASE).await?
  else {
    return Ok(false);
  };
  let item_id = entry.item_id;

  let item = match items::get(db.pool(), item_id).await {
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

  let system_prompt = "You are a triage assistant. \
    Decide whether this item requires human attention or can be auto-archived. \
    Respond with JSON: {\"confident\": bool, \"auto_action\": \"archive\" | null, \
    \"uncertainty_reason\": string | null}";

  let user_content = format!(
    "Source: {}\nTitle: {}\nBody: {}",
    item.source,
    item.title,
    item.body.as_deref().unwrap_or("(none)")
  );

  let req = CompletionRequest {
    model: model.to_string(),
    messages: vec![
      Message {
        role: Role::System,
        content: system_prompt.to_string(),
      },
      Message {
        role: Role::User,
        content: user_content,
      },
    ],
    temperature: Some(0.1),
    tools: None,
  };

  let (confident, decision_event) = match client.complete(req).await {
    Ok(resp) => {
      let text = resp
        .choices
        .first()
        .and_then(|c| c.message.content.as_deref())
        .unwrap_or("{}");

      let decision: serde_json::Value = serde_json::from_str(text)
        .unwrap_or_else(|_| {
          json!({
            "confident": false,
            "uncertainty_reason": "LLM returned non-JSON response"
          })
        });

      let confident = decision
        .get("confident")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

      let event = events::new_item_event(
        item_id,
        EventType::IntakeLlmAnalysis,
        Actor::IntakeLlm,
        Locality::Local,
        json!({
          "model": model,
          "confident": confident,
          "auto_action": decision.get("auto_action"),
          "uncertainty_reason": decision.get("uncertainty_reason"),
        }),
      );
      (confident, event)
    }
    Err(e) => {
      warn!(
        item_id = %item_id,
        "Intake LLM call failed: {e}. Escalating to human."
      );
      let event = events::new_item_event(
        item_id,
        EventType::IntakeLlmAnalysis,
        Actor::IntakeLlm,
        Locality::Local,
        json!({
          "model": model,
          "confident": false,
          "uncertainty_reason": format!("LLM error: {e}"),
        }),
      );
      (false, event)
    }
  };

  // Compose the analysis event and queue transition in one
  // transaction so the audit trail and the projection move
  // together.
  if let Err(e) =
    finalize_decision(db, item_id, confident, &decision_event, worker_id).await
  {
    warn!(item_id = %item_id, "Failed to finalize intake decision: {e}");
  }

  Ok(true)
}

async fn finalize_decision(
  db: &Db,
  item_id: Uuid,
  confident: bool,
  decision_event: &hyuqueue_core::event::Event,
  worker_id: &str,
) -> Result<(), String> {
  let mut tx = db
    .pool()
    .begin()
    .await
    .map_err(|e| format!("begin tx: {e}"))?;
  events::append(&mut *tx, decision_event)
    .await
    .map_err(|e| format!("analysis event append: {e}"))?;
  if confident {
    queue::complete(&mut *tx, queue_names::INTAKE, item_id, worker_id)
      .await
      .map_err(|e| format!("complete: {e}"))?;
  } else {
    queue::move_item(
      &mut tx,
      item_id,
      queue_names::INTAKE,
      queue_names::HUMAN,
      worker_id,
    )
    .await
    .map_err(|e| format!("move to human: {e}"))?;
  }
  tx.commit().await.map_err(|e| format!("commit: {e}"))?;
  Ok(())
}
