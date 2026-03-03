//! Review LLM worker — slow, agentic, pattern-recognition focused.
//!
//! Triggered after every ack (polls for Done items not yet reviewed).
//! The review LLM has access to query tools to inspect item history before
//! deciding whether to suggest a policy change or filter.
//!
//! When tool calls are unsupported by the model, falls back to a structured
//! context dump (recent similar items pre-computed by SQL and passed as text).

use crate::config::LlmConfig;
use hyuqueue_core::event::{Actor, EventType, Locality};
use hyuqueue_lib::llm::{CompletionRequest, LlmClient, Message, OpenAiClient, Role};
use hyuqueue_store::{Db, events, items};
use serde_json::json;
use std::sync::Arc;
use tokio::time::{Duration, sleep};
use tracing::{error, info, warn};

// Review worker runs less frequently than intake — it's the slower lane.
const POLL_INTERVAL: Duration = Duration::from_secs(10);
const BATCH_SIZE: i64 = 5;

pub async fn run(db: Db, llm_config: Arc<LlmConfig>) {
  let client = OpenAiClient::new(
    llm_config.base_url.clone(),
    llm_config.api_key.clone(),
  );

  info!("Review worker started");

  loop {
    match process_batch(&db, &client, &llm_config.review_model).await {
      Ok(processed) if processed > 0 => {
        info!(count = processed, "Review worker processed batch");
      }
      Ok(_) => {}
      Err(e) => {
        error!("Review worker error: {e}");
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
  let item_ids = events::items_awaiting_review(db, BATCH_SIZE).await?;
  let count = item_ids.len();

  for item_id in item_ids {
    let item = match items::get(db, item_id).await {
      Ok(i) => i,
      Err(e) => {
        warn!(item_id = %item_id, "Could not fetch item for review: {e}");
        continue;
      }
    };

    // Fetch recent items from the same source for context.
    let recent = items::list(db, None, None, 20, 0)
      .await
      .unwrap_or_default()
      .into_iter()
      .filter(|i| i.source == item.source && i.id != item.id)
      .take(10)
      .map(|i| {
        json!({
          "title": i.title,
          "state": i.state.to_string(),
          "created_at": i.created_at,
        })
      })
      .collect::<Vec<_>>();

    let system_prompt = "You are a queue hygiene assistant. \
      Review this item and recent similar items. \
      If you see a clear pattern of items that could be auto-handled \
      (e.g. always immediately acked, always same action), suggest a policy. \
      If no suggestion, respond with {\"suggest\": false}. \
      Otherwise: {\"suggest\": true, \"title\": str, \"description\": str}";

    let user_content = format!(
      "Item source: {}\nItem title: {}\n\nRecent similar items: {}",
      item.source,
      item.title,
      serde_json::to_string_pretty(&recent).unwrap_or_default()
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
      temperature: Some(0.3),
      tools: None, // Tool call support added when models reliably support it.
    };

    match client.complete(req).await {
      Ok(resp) => {
        let text = resp
          .choices
          .first()
          .and_then(|c| c.message.content.as_deref())
          .unwrap_or("{}");

        let decision: serde_json::Value =
          serde_json::from_str(text).unwrap_or(json!({ "suggest": false }));

        let suggestion_item_id = if decision
          .get("suggest")
          .and_then(|v| v.as_bool())
          .unwrap_or(false)
        {
          // Create a suggestion item for the human queue.
          let title = decision
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Queue hygiene suggestion");
          let description = decision
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");

          let suggestion = hyuqueue_core::item::Item {
            id: uuid::Uuid::new_v4(),
            queue_id: item.queue_id,
            title: format!("[suggestion] {title}"),
            body: Some(description.to_string()),
            source_topic_id: None,
            source: "review_llm".to_string(),
            delegate_from: None,
            delegate_chain: vec![],
            capabilities: vec![],
            metadata: json!({
              "triggered_by_item_id": item_id,
              "suggestion": decision,
            }),
            state: hyuqueue_core::item::ItemState::HumanPending,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
          };

          let sid = suggestion.id;
          let _ = items::insert(db, &suggestion).await;

          let creation_event = events::new_event(
            sid,
            EventType::ItemCreated,
            Actor::ReviewLlm,
            Locality::Local,
            json!({ "triggered_by": item_id }),
          );
          let _ = events::append(db, &creation_event).await;

          Some(sid)
        } else {
          None
        };

        let review_event = events::new_event(
          item_id,
          EventType::ReviewLlmAnalysis,
          Actor::ReviewLlm,
          Locality::Local,
          json!({
            "model": model,
            "queries_run": [],
            "reasoning": text,
            "suggestion_item_id": suggestion_item_id,
          }),
        );
        let _ = events::append(db, &review_event).await;
      }
      Err(e) => {
        warn!(item_id = %item_id, "Review LLM call failed: {e}");
        // Log the failure but don't retry immediately — the item remains
        // in the "awaiting review" pool and will be picked up next cycle.
        let event = events::new_event(
          item_id,
          EventType::ReviewLlmAnalysis,
          Actor::ReviewLlm,
          Locality::Local,
          json!({ "error": e.to_string() }),
        );
        let _ = events::append(db, &event).await;
      }
    }
  }

  Ok(count)
}
