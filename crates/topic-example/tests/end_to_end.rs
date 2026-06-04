//! End-to-end test that actually spawns the topic-example binary and
//! drives it through SubprocessTopic.  This is the first exercise of
//! the full host ↔ topic protocol with a real subprocess (every other
//! test in this workspace uses in-memory duplex pipes).
//!
//! `env!("CARGO_BIN_EXE_<name>")` is resolved by cargo at test build
//! time and points at the freshly-built binary in target/, so the
//! integration test always picks up the current code.

use async_trait::async_trait;
use hyuqueue_core::activity::ActivityInvocation;
use hyuqueue_core::topic::{Topic, TopicCtx, TopicError};
use hyuqueue_topic_host::{SubprocessTopic, TopicDataSink};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

struct RecordingSink {
  calls: Arc<Mutex<Vec<(String, String, serde_json::Value)>>>,
}

#[async_trait]
impl TopicDataSink for RecordingSink {
  async fn set_data(
    &self,
    topic_id: &str,
    key: &str,
    value: serde_json::Value,
  ) {
    self.calls.lock().unwrap().push((
      topic_id.to_string(),
      key.to_string(),
      value,
    ));
  }
}

fn binary_path() -> String {
  env!("CARGO_BIN_EXE_hyuqueue-topic-example").to_string()
}

async fn spawn_example(
  calls: Arc<Mutex<Vec<(String, String, serde_json::Value)>>>,
) -> SubprocessTopic {
  spawn_example_with_data(calls, HashMap::new()).await
}

/// Like `spawn_example` but seeds the init handshake with a
/// pre-existing `topic_data` snapshot — used by the resume-after-
/// restart test to verify hydrate-on-init.
async fn spawn_example_with_data(
  calls: Arc<Mutex<Vec<(String, String, serde_json::Value)>>>,
  initial_data: HashMap<String, serde_json::Value>,
) -> SubprocessTopic {
  let sink: Arc<dyn TopicDataSink> = Arc::new(RecordingSink { calls });
  SubprocessTopic::spawn("example", &[binary_path()], sink, initial_data)
    .await
    .expect("spawn topic-example")
}

/// The notification → sink hop is asynchronous on the host side.
/// Wait up to ~1s for at least `expected` calls to land before
/// continuing.
async fn wait_for_calls(
  calls: &Arc<Mutex<Vec<(String, String, serde_json::Value)>>>,
  expected: usize,
) {
  for _ in 0..50 {
    if calls.lock().unwrap().len() >= expected {
      return;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

#[tokio::test]
async fn init_handshake_reports_declared_topic() {
  let calls = Arc::new(Mutex::new(Vec::new()));
  let topic = spawn_example(calls).await;
  assert_eq!(topic.id(), "example");
  assert_eq!(topic.display_name(), "Example");

  let item_acts = topic.item_activities();
  assert_eq!(item_acts.len(), 1);
  assert_eq!(item_acts[0].id, "example.reset");

  let global_acts = topic.global_activities();
  assert_eq!(global_acts.len(), 1);
  assert_eq!(global_acts[0].id, "example.bump");
}

#[tokio::test]
async fn ingest_produces_tick_and_persists_counter() {
  let calls = Arc::new(Mutex::new(Vec::new()));
  let topic = spawn_example(calls.clone()).await;

  let ctx = TopicCtx::stub();
  let items = topic.ingest(&ctx, &json!({})).await.unwrap();
  assert_eq!(items.len(), 1);
  assert_eq!(items[0].external_id.as_deref(), Some("tick-1"));
  assert!(
    items[0].title.starts_with("tick #"),
    "unexpected title: {:?}",
    items[0].title
  );
  assert_eq!(items[0].metadata["counter"], 1);

  wait_for_calls(&calls, 1).await;
  let calls = calls.lock().unwrap();
  assert_eq!(calls.len(), 1);
  assert_eq!(calls[0].0, "example");
  assert_eq!(calls[0].1, "counter");
  assert_eq!(calls[0].2, 1);
}

/// The whole point of hydrate-on-init: a "restart" of topic-example
/// should resume the counter from the persisted value rather than
/// starting fresh.  The first ingest after the restart emits
/// "tick-6" (counter was 5, fetch_add -> 6), not "tick-1".
///
/// This is the regression test that locks in the read path closing
/// the duplicate-emission gap that motivated the host-side dedupe
/// constraint.
#[tokio::test]
async fn counter_resumes_from_persisted_topic_data() {
  let calls = Arc::new(Mutex::new(Vec::new()));
  let mut initial = HashMap::new();
  initial.insert("counter".to_string(), json!(5));
  let topic = spawn_example_with_data(calls.clone(), initial).await;

  let ctx = TopicCtx::stub();
  let items = topic.ingest(&ctx, &json!({})).await.unwrap();
  assert_eq!(items.len(), 1);
  assert_eq!(
    items[0].external_id.as_deref(),
    Some("tick-6"),
    "expected counter to resume from 5; if this fails, hydrate-on-init \
     is not feeding Topic::init"
  );
  assert_eq!(items[0].metadata["counter"], 6);

  wait_for_calls(&calls, 1).await;
  let calls = calls.lock().unwrap();
  assert_eq!(calls.last().unwrap().2, 6);
}

#[tokio::test]
async fn execute_reset_item_activity() {
  let calls = Arc::new(Mutex::new(Vec::new()));
  let topic = spawn_example(calls.clone()).await;

  let ctx = TopicCtx::stub();
  // Bump it first so reset has something to clear.
  topic.ingest(&ctx, &json!({})).await.unwrap();
  topic.ingest(&ctx, &json!({})).await.unwrap();

  let event = topic
    .execute(
      &ctx,
      &ActivityInvocation {
        activity_id: "example.reset".to_string(),
        params: json!({}),
      },
      Uuid::new_v4(),
    )
    .await
    .unwrap();
  assert_eq!(event.payload["activity_id"], "example.reset");
  assert_eq!(
    event.payload["result_summary"].as_str().unwrap(),
    "counter is now 0"
  );

  // Three set_data calls: two from ingest, one from reset.
  wait_for_calls(&calls, 3).await;
  let calls = calls.lock().unwrap();
  let last = calls.last().unwrap();
  assert_eq!(last.1, "counter");
  assert_eq!(last.2, 0);
}

#[tokio::test]
async fn execute_bump_global_activity_uses_delta_param() {
  let calls = Arc::new(Mutex::new(Vec::new()));
  let topic = spawn_example(calls.clone()).await;

  let ctx = TopicCtx::stub();
  let event = topic
    .execute(
      &ctx,
      &ActivityInvocation {
        activity_id: "example.bump".to_string(),
        params: json!({ "delta": 5 }),
      },
      Uuid::new_v4(),
    )
    .await
    .unwrap();
  assert_eq!(event.payload["activity_id"], "example.bump");
  assert_eq!(
    event.payload["result_summary"].as_str().unwrap(),
    "counter is now 5"
  );

  wait_for_calls(&calls, 1).await;
  let calls = calls.lock().unwrap();
  assert_eq!(calls.last().unwrap().2, 5);
}

#[tokio::test]
async fn unsupported_activity_round_trips_via_real_subprocess() {
  let calls = Arc::new(Mutex::new(Vec::new()));
  let topic = spawn_example(calls).await;

  let result = topic
    .execute(
      &TopicCtx::stub(),
      &ActivityInvocation {
        activity_id: "example.nonexistent".to_string(),
        params: json!({}),
      },
      Uuid::new_v4(),
    )
    .await;

  match result {
    Err(TopicError::UnsupportedActivity(activity, topic_id)) => {
      assert_eq!(activity, "example.nonexistent");
      assert_eq!(topic_id, "example");
    }
    other => panic!("expected UnsupportedActivity, got {:?}", other),
  }
}

#[tokio::test]
async fn bump_with_missing_delta_returns_execution_error() {
  let calls = Arc::new(Mutex::new(Vec::new()));
  let topic = spawn_example(calls).await;

  let result = topic
    .execute(
      &TopicCtx::stub(),
      &ActivityInvocation {
        activity_id: "example.bump".to_string(),
        params: json!({}),
      },
      Uuid::new_v4(),
    )
    .await;
  match result {
    Err(TopicError::Execution { activity, reason }) => {
      assert_eq!(activity, "example.bump");
      assert!(reason.contains("delta"), "unexpected reason: {reason}");
    }
    other => panic!("expected Execution error, got {:?}", other),
  }
}
