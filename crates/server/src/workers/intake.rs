//! Intake LLM worker — fast, inline with item ingestion.
//!
//! Polls for IntakePending items, calls the intake LLM, and either:
//! - Marks the item AutoHandled (LLM was confident)
//! - Marks the item HumanPending (LLM was uncertain — goes to human queue)
//!
//! The uncertainty reason is embedded in the IntakeLlmAnalysis event and
//! shown to the human in the queue UI.
//!
//! When tool calls are not supported by the configured model, the worker
//! falls back to a simpler prompt that asks for a JSON decision.

use crate::config::LlmConfig;
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  item::ItemState,
};
use hyuqueue_lib::llm::{
  CompletionRequest, LlmClient, Message, OpenAiClient, Role,
};
use hyuqueue_store::{events, items, Db};
use serde_json::json;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const BATCH_SIZE: i64 = 10;

pub async fn run(db: Db, llm_config: Arc<LlmConfig>) {
  let client =
    OpenAiClient::new(llm_config.base_url.clone(), llm_config.api_key.clone());

  info!("Intake worker started");

  loop {
    match process_batch(&db, &client, &llm_config.intake_model).await {
      Ok(processed) if processed > 0 => {
        info!(count = processed, "Intake worker processed batch");
      }
      Ok(_) => {}
      Err(e) => {
        error!("Intake worker error: {e}");
      }
    }
    sleep(POLL_INTERVAL).await;
  }
}

async fn process_batch(
  db: &Db,
  client: &OpenAiClient,
  model: &str,
) -> Result<usize, Box<dyn std::error::Error>> {
  let item_ids = events::items_awaiting_intake(db, BATCH_SIZE).await?;
  let count = item_ids.len();

  for item_id in item_ids {
    let item = match items::get(db, item_id).await {
      Ok(i) => i,
      Err(e) => {
        warn!(item_id = %item_id, "Could not fetch item for intake: {e}");
        continue;
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

    match client.complete(req).await {
      Ok(resp) => {
        let text = resp
          .choices
          .first()
          .and_then(|c| c.message.content.as_deref())
          .unwrap_or("{}");

        let decision: serde_json::Value = serde_json::from_str(text)
          .unwrap_or_else(|_| {
            // Model didn't return valid JSON — escalate to human.
            json!({
              "confident": false,
              "uncertainty_reason": "LLM returned non-JSON response"
            })
          });

        let confident = decision
          .get("confident")
          .and_then(|v| v.as_bool())
          .unwrap_or(false);

        let new_state = if confident {
          ItemState::AutoHandled
        } else {
          ItemState::HumanPending
        };

        let event = events::new_event(
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
        let _ = events::append(db, &event).await;
        let _ = items::update_state(db, item_id, new_state).await;
      }
      Err(e) => {
        warn!(item_id = %item_id, "Intake LLM call failed: {e}. Escalating to human.");
        // On LLM error, always escalate — never silently drop.
        let event = events::new_event(
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
        let _ = events::append(db, &event).await;
        let _ = items::update_state(db, item_id, ItemState::HumanPending).await;
      }
    }
  }

  Ok(count)
}
