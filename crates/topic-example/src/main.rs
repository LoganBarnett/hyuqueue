//! topic-example — a minimal topic binary that exercises every RPC
//! method in the host ↔ topic protocol.
//!
//! Not modelled on any real domain.  The topic owns an in-memory
//! counter; `ingest` increments it and emits a synthetic "tick"
//! item, the `reset` item activity zeroes it, the `bump` global
//! activity adds a delta from params, and each mutation persists the
//! new counter value via `ctx.set_data`.  Together these calls
//! exercise the init handshake, ingest, execute on both item and
//! global activities, the `topic_data_set` notification path, and
//! the structured `TopicError` path for unsupported activities.

use async_trait::async_trait;
use chrono::Utc;
use hyuqueue_core::activity::{
  Activity, ActivityEffect, ActivityExecutor, ActivityInvocation,
};
use hyuqueue_core::event::{Actor, Event, EventType, Locality};
use hyuqueue_core::topic::{IngestItem, Topic, TopicCtx, TopicError};
use serde_json::json;
use std::sync::atomic::{AtomicI64, Ordering};
use uuid::Uuid;

const ID: &str = "example";
const COUNTER_KEY: &str = "counter";

struct ExampleTopic {
  counter: AtomicI64,
}

impl ExampleTopic {
  fn new() -> Self {
    Self {
      counter: AtomicI64::new(0),
    }
  }

  async fn persist_counter(
    &self,
    ctx: &TopicCtx,
    activity: &str,
    value: i64,
  ) -> Result<(), TopicError> {
    ctx.set_data(COUNTER_KEY, json!(value)).await.map_err(|e| {
      TopicError::Execution {
        activity: activity.to_string(),
        reason: format!("failed to persist counter: {e}"),
      }
    })
  }
}

#[async_trait]
impl Topic for ExampleTopic {
  fn id(&self) -> &str {
    ID
  }

  fn display_name(&self) -> &str {
    "Example"
  }

  fn item_activities(&self) -> Vec<Activity> {
    vec![Activity {
      id: "example.reset".to_string(),
      label: "Reset".to_string(),
      key: 'r',
      executor: ActivityExecutor::Local,
      effect: ActivityEffect::Write,
      description: "Reset the example counter to zero.".to_string(),
      params: json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
      }),
      examples: vec![],
      human_only: false,
    }]
  }

  fn global_activities(&self) -> Vec<Activity> {
    vec![Activity {
      id: "example.bump".to_string(),
      label: "Bump".to_string(),
      key: 'b',
      executor: ActivityExecutor::Local,
      effect: ActivityEffect::Write,
      description: "Add a signed delta to the example counter.".to_string(),
      params: json!({
        "type": "object",
        "properties": { "delta": { "type": "integer" } },
        "required": ["delta"],
        "additionalProperties": false,
      }),
      examples: vec![],
      human_only: false,
    }]
  }

  async fn ingest(
    &self,
    ctx: &TopicCtx,
    _config: &serde_json::Value,
  ) -> Result<Vec<IngestItem>, TopicError> {
    let next = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
    self.persist_counter(ctx, "ingest", next).await?;
    Ok(vec![IngestItem {
      title: format!("tick #{next}"),
      source: ID.to_string(),
      body: None,
      metadata: json!({ COUNTER_KEY: next }),
    }])
  }

  async fn execute(
    &self,
    ctx: &TopicCtx,
    invocation: &ActivityInvocation,
    item_id: Uuid,
  ) -> Result<Event, TopicError> {
    let new_value = match invocation.activity_id.as_str() {
      "example.reset" => {
        self.counter.store(0, Ordering::Relaxed);
        0
      }
      "example.bump" => {
        let delta = invocation
          .params
          .get("delta")
          .and_then(|v| v.as_i64())
          .ok_or_else(|| TopicError::Execution {
            activity: invocation.activity_id.clone(),
            reason: "missing or non-integer 'delta' param".to_string(),
          })?;
        self.counter.fetch_add(delta, Ordering::Relaxed) + delta
      }
      other => {
        return Err(TopicError::UnsupportedActivity(
          other.to_string(),
          ID.to_string(),
        ));
      }
    };
    self
      .persist_counter(ctx, &invocation.activity_id, new_value)
      .await?;
    Ok(Event {
      id: Uuid::new_v4(),
      event_type: EventType::ActionTaken,
      actor: Actor::Topic(ID.to_string()),
      locality: Locality::Local,
      payload: json!({
        "item_id": item_id,
        "activity_id": invocation.activity_id,
        "result_summary": format!("counter is now {new_value}"),
      }),
      created_at: Utc::now(),
    })
  }
}

#[tokio::main]
async fn main() {
  if let Err(e) = hyuqueue_topic_sdk::run(ExampleTopic::new()).await {
    eprintln!("topic-example: {e}");
    std::process::exit(1);
  }
}
