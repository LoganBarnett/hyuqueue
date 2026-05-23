//! Review LLM worker — slow, agentic, pattern-recognition focused.
//!
//! Dequeues items from the `outtake` queue (populated when the human
//! acks an item), runs the review LLM, and completes the item.
//! Suggestion items the LLM produces are inserted via the items API
//! and enqueued onto the `human` queue.
//!
//! The agentic-loop rewrite (see the "Outtake LLM as agentic loop"
//! TODO in tasks.org) will replace the single-shot LLM call with a
//! multi-turn dispatcher; the outer queue-dispatch shell stays
//! the same.

use crate::config::LlmConfig;
use crate::tx_error::TxOpError;
use chrono::Duration as ChronoDuration;
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  queue as queue_names,
};
use hyuqueue_lib::llm::{CompletionRequest, LlmClient, Message, OpenAiClient};
use hyuqueue_store::{events, items, queue, Db};
use serde_json::json;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_secs(10);
const LEASE: ChronoDuration = ChronoDuration::minutes(5);

pub async fn run(db: Db, llm_config: Arc<LlmConfig>) {
  let client =
    OpenAiClient::new(llm_config.base_url.clone(), llm_config.api_key.clone());
  let worker_id = format!("outtake-{}", Uuid::new_v4());

  info!(worker_id = %worker_id, "Outtake worker started");

  loop {
    match process_next(&db, &client, &llm_config.review_model, &worker_id).await
    {
      Ok(true) => {}
      Ok(false) => sleep(POLL_INTERVAL).await,
      Err(e) => {
        error!("Outtake worker error: {e}");
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
    queue::dequeue_one(db, queue_names::OUTTAKE, worker_id, LEASE).await?
  else {
    return Ok(false);
  };
  let item_id = entry.item_id;

  let item = match items::get(db.pool(), item_id).await {
    Ok(i) => i,
    Err(e) => {
      warn!(item_id = %item_id, "Could not fetch item for review: {e}");
      if let Err(release_err) =
        queue::release(db.pool(), queue_names::OUTTAKE, item_id, worker_id)
          .await
      {
        warn!(item_id = %item_id, "Failed to release claim: {release_err}");
      }
      return Ok(true);
    }
  };

  // Fetch recent items from the same source for context.
  let recent = items::list(db.pool(), None, 50, 0)
    .await
    .unwrap_or_default()
    .into_iter()
    .filter(|i| i.source == item.source && i.id != item.id)
    .take(10)
    .map(|i| {
      json!({
        "title": i.title,
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
    messages: vec![Message::system(system_prompt), Message::user(user_content)],
    temperature: Some(0.3),
    tools: None,
  };

  let (review_event, suggestion) = match client.complete(req).await {
    Ok(resp) => {
      let text = resp
        .choices
        .first()
        .and_then(|c| c.message.content.as_deref())
        .unwrap_or("{}");

      let decision: serde_json::Value =
        serde_json::from_str(text).unwrap_or(json!({ "suggest": false }));

      let suggestion = if decision
        .get("suggest")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
      {
        Some(build_suggestion(&item, item_id, &decision))
      } else {
        None
      };

      let suggestion_id = suggestion.as_ref().map(|s| s.id);
      let event = events::new_item_event(
        item_id,
        EventType::ReviewLlmAnalysis,
        Actor::ReviewLlm,
        Locality::Local,
        json!({
          "model": model,
          "queries_run": [],
          "reasoning": text,
          "suggestion_item_id": suggestion_id,
        }),
      );
      (event, suggestion)
    }
    Err(e) => {
      warn!(item_id = %item_id, "Outtake LLM call failed: {e}");
      let event = events::new_item_event(
        item_id,
        EventType::ReviewLlmAnalysis,
        Actor::ReviewLlm,
        Locality::Local,
        json!({ "error": e.to_string() }),
      );
      (event, None)
    }
  };

  // Compose: complete the outtake queue entry, append the review
  // event, and (if any) insert + enqueue the suggestion item — all
  // in one transaction so we never have a half-created suggestion
  // or a review without a queue transition.
  if let Err(e) =
    finalize_review(db, item_id, worker_id, &review_event, suggestion).await
  {
    warn!(item_id = %item_id, "Failed to finalize outtake review: {e}");
  }

  Ok(true)
}

async fn finalize_review(
  db: &Db,
  item_id: Uuid,
  worker_id: &str,
  review_event: &hyuqueue_core::event::Event,
  suggestion: Option<hyuqueue_core::item::Item>,
) -> Result<(), TxOpError> {
  let mut tx = db.pool().begin().await.map_err(TxOpError::BeginTx)?;
  queue::complete(&mut *tx, queue_names::OUTTAKE, item_id, worker_id).await?;
  events::append(&mut *tx, review_event).await?;
  if let Some(s) = suggestion {
    items::insert(&mut *tx, &s).await?;
    let creation_event = events::new_item_event(
      s.id,
      EventType::ItemCreated,
      Actor::ReviewLlm,
      Locality::Local,
      json!({ "triggered_by": item_id }),
    );
    events::append(&mut *tx, &creation_event).await?;
    queue::enqueue(&mut *tx, queue_names::HUMAN, s.id, 0).await?;
  }
  tx.commit().await.map_err(TxOpError::CommitTx)?;
  Ok(())
}

fn build_suggestion(
  source_item: &hyuqueue_core::item::Item,
  triggered_by: Uuid,
  decision: &serde_json::Value,
) -> hyuqueue_core::item::Item {
  let title = decision
    .get("title")
    .and_then(|v| v.as_str())
    .unwrap_or("Queue hygiene suggestion");
  let description = decision
    .get("description")
    .and_then(|v| v.as_str())
    .unwrap_or("");

  hyuqueue_core::item::Item {
    id: Uuid::new_v4(),
    title: format!("[suggestion] {title}"),
    body: Some(description.to_string()),
    source_topic_id: None,
    source: "review_llm".to_string(),
    delegate_from: None,
    delegate_chain: vec![],
    capabilities: vec![],
    metadata: json!({
      "triggered_by_item_id": triggered_by,
      "source_item_source": source_item.source,
      "suggestion": decision,
    }),
    created_at: chrono::Utc::now(),
    updated_at: chrono::Utc::now(),
  }
}
