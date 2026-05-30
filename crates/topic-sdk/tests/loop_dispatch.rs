//! Integration tests for the SDK's JSON-RPC dispatch loop.
//!
//! Each test spawns `run_with_io` against in-memory `tokio::io::duplex`
//! pipes, drives requests in, and asserts on the lines that come back
//! out.  No real stdin/stdout, no real subprocess — the loop's
//! behavior is exercised end-to-end against the wire format.

use async_trait::async_trait;
use hyuqueue_core::activity::ActivityInvocation;
use hyuqueue_core::event::{Actor, Event, EventType, Locality};
use hyuqueue_core::topic::{IngestItem, Topic, TopicCtx, TopicError};
use hyuqueue_topic_proto::envelope::{Notification, Request, Response};
use hyuqueue_topic_proto::error::{TopicErrorData, RPC_ERROR_CODE_TOPIC};
use hyuqueue_topic_proto::method;
use hyuqueue_topic_proto::version::JsonRpcVersion;
use hyuqueue_topic_sdk::run_with_io;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use uuid::Uuid;

// ── harness ──────────────────────────────────────────────────────────

struct Harness {
  /// Test writes JSON-RPC requests to this end (one line per request).
  requests: DuplexStream,
  /// Test reads JSON-RPC responses and notifications from this end.
  responses: BufReader<DuplexStream>,
  sdk_handle: tokio::task::JoinHandle<Result<(), hyuqueue_topic_sdk::SdkError>>,
}

impl Harness {
  fn spawn<T: Topic + Send + Sync + 'static>(topic: T) -> Self {
    // Two unidirectional pipes — one for requests (test → SDK) and
    // one for responses (SDK → test).  DuplexStream is bidirectional
    // but we only ever use one direction per pair.
    let (host_w, topic_r) = tokio::io::duplex(8192);
    let (topic_w, host_r) = tokio::io::duplex(8192);
    let sdk_handle = tokio::spawn(run_with_io(topic, topic_r, topic_w));
    Harness {
      requests: host_w,
      responses: BufReader::new(host_r),
      sdk_handle,
    }
  }

  async fn send(&mut self, value: serde_json::Value) {
    let line = serde_json::to_string(&value).unwrap();
    self.requests.write_all(line.as_bytes()).await.unwrap();
    self.requests.write_all(b"\n").await.unwrap();
    self.requests.flush().await.unwrap();
  }

  async fn recv(&mut self) -> serde_json::Value {
    let mut line = String::new();
    let n = self.responses.read_line(&mut line).await.unwrap();
    assert!(n > 0, "expected a response line, got EOF");
    serde_json::from_str(line.trim_end()).unwrap()
  }

  async fn close_requests_and_finish(
    self,
  ) -> Result<(), hyuqueue_topic_sdk::SdkError> {
    drop(self.requests);
    self.sdk_handle.await.unwrap()
  }
}

fn req(id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
  json!({"jsonrpc": "2.0", "method": method, "params": params, "id": id})
}

// ── test topics ──────────────────────────────────────────────────────

struct EmptyTopic;

#[async_trait]
impl Topic for EmptyTopic {
  fn id(&self) -> &str {
    "empty"
  }
  fn display_name(&self) -> &str {
    "Empty"
  }
  async fn execute(
    &self,
    _ctx: &TopicCtx,
    invocation: &ActivityInvocation,
    _item_id: Uuid,
  ) -> Result<Event, TopicError> {
    Err(TopicError::UnsupportedActivity(
      invocation.activity_id.clone(),
      self.id().to_string(),
    ))
  }
}

struct TickingTopic;

#[async_trait]
impl Topic for TickingTopic {
  fn id(&self) -> &str {
    "ticking"
  }
  fn display_name(&self) -> &str {
    "Ticking"
  }
  async fn ingest(
    &self,
    _ctx: &TopicCtx,
    _config: &serde_json::Value,
  ) -> Result<Vec<IngestItem>, TopicError> {
    Ok(vec![IngestItem {
      title: "tick".to_string(),
      body: None,
      external_id: None,
      metadata: json!({"n": 1}),
    }])
  }
  async fn execute(
    &self,
    _ctx: &TopicCtx,
    invocation: &ActivityInvocation,
    item_id: Uuid,
  ) -> Result<Event, TopicError> {
    Ok(Event {
      id: Uuid::nil(),
      event_type: EventType::ActionTaken,
      actor: Actor::Topic(self.id().to_string()),
      locality: Locality::Local,
      payload: json!({
        "item_id": item_id,
        "activity_id": invocation.activity_id,
      }),
      created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
        .unwrap(),
    })
  }
}

struct ErroringTopic;

#[async_trait]
impl Topic for ErroringTopic {
  fn id(&self) -> &str {
    "erroring"
  }
  fn display_name(&self) -> &str {
    "Erroring"
  }
  async fn ingest(
    &self,
    _ctx: &TopicCtx,
    _config: &serde_json::Value,
  ) -> Result<Vec<IngestItem>, TopicError> {
    Err(TopicError::Execution {
      activity: "ingest".to_string(),
      reason: "boom".to_string(),
    })
  }
  async fn execute(
    &self,
    _ctx: &TopicCtx,
    _invocation: &ActivityInvocation,
    _item_id: Uuid,
  ) -> Result<Event, TopicError> {
    unreachable!()
  }
}

struct NotifyingTopic {
  /// Records what set_data calls were made — populated by the topic
  /// during ingest() so the test can verify the topic itself fired
  /// the notification (independent of what surfaces on the wire).
  recorded: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
}

#[async_trait]
impl Topic for NotifyingTopic {
  fn id(&self) -> &str {
    "notifying"
  }
  fn display_name(&self) -> &str {
    "Notifying"
  }
  async fn ingest(
    &self,
    ctx: &TopicCtx,
    _config: &serde_json::Value,
  ) -> Result<Vec<IngestItem>, TopicError> {
    let key = "cursor";
    let value = json!(42);
    ctx.set_data(key, value.clone()).await.unwrap();
    self.recorded.lock().unwrap().push((key.to_string(), value));
    Ok(vec![])
  }
  async fn execute(
    &self,
    _ctx: &TopicCtx,
    _invocation: &ActivityInvocation,
    _item_id: Uuid,
  ) -> Result<Event, TopicError> {
    unreachable!()
  }
}

// ── tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn init_returns_topic_identity() {
  let mut h = Harness::spawn(EmptyTopic);
  h.send(req(1, method::INIT, json!({}))).await;
  let resp_value = h.recv().await;
  let resp: Response = serde_json::from_value(resp_value).unwrap();
  match resp {
    Response::Success(s) => {
      assert_eq!(s.id, 1);
      assert_eq!(s.result["id"], "empty");
      assert_eq!(s.result["display_name"], "Empty");
      assert_eq!(s.result["supports_ingest"], true);
      assert_eq!(s.result["supports_execute"], true);
      assert_eq!(s.result["item_activities"], json!([]));
      assert_eq!(s.result["global_activities"], json!([]));
    }
    Response::Error(_) => panic!("expected success response"),
  }
  drop(h.requests);
  let _ = h.sdk_handle.await.unwrap();
}

#[tokio::test]
async fn ingest_returns_items() {
  let mut h = Harness::spawn(TickingTopic);
  h.send(req(2, method::INGEST, json!({"config": {}}))).await;
  let resp: Response = serde_json::from_value(h.recv().await).unwrap();
  match resp {
    Response::Success(s) => {
      assert_eq!(s.id, 2);
      let items = s.result["items"].as_array().unwrap();
      assert_eq!(items.len(), 1);
      assert_eq!(items[0]["title"], "tick");
      assert_eq!(items[0]["metadata"]["n"], 1);
    }
    Response::Error(e) => panic!("expected success, got {:?}", e),
  }
  drop(h.requests);
  let _ = h.sdk_handle.await.unwrap();
}

#[tokio::test]
async fn execute_returns_event() {
  let mut h = Harness::spawn(TickingTopic);
  let item_id = Uuid::new_v4();
  h.send(req(
    3,
    method::EXECUTE,
    json!({
      "invocation": {"activity_id": "ticking.tick", "params": {}},
      "item_id": item_id,
    }),
  ))
  .await;
  let resp: Response = serde_json::from_value(h.recv().await).unwrap();
  match resp {
    Response::Success(s) => {
      assert_eq!(s.id, 3);
      assert_eq!(s.result["event"]["payload"]["activity_id"], "ticking.tick");
      assert_eq!(s.result["event"]["payload"]["item_id"], item_id.to_string());
    }
    Response::Error(e) => panic!("expected success, got {:?}", e),
  }
  drop(h.requests);
  let _ = h.sdk_handle.await.unwrap();
}

#[tokio::test]
async fn shutdown_response_then_loop_exits() {
  let mut h = Harness::spawn(EmptyTopic);
  h.send(req(4, method::SHUTDOWN, json!({}))).await;
  let resp: Response = serde_json::from_value(h.recv().await).unwrap();
  match resp {
    Response::Success(s) => {
      assert_eq!(s.id, 4);
      assert_eq!(s.result, serde_json::Value::Null);
    }
    Response::Error(e) => panic!("expected success, got {:?}", e),
  }
  // Loop should exit on its own without further input.
  let result = h.sdk_handle.await.unwrap();
  assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn eof_terminates_loop_cleanly() {
  let h = Harness::spawn(EmptyTopic);
  // Drop the request writer immediately — SDK should see EOF and exit.
  let result = h.close_requests_and_finish().await;
  assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn unknown_method_returns_method_not_found() {
  let mut h = Harness::spawn(EmptyTopic);
  h.send(req(5, "no_such_method", json!({}))).await;
  let resp: Response = serde_json::from_value(h.recv().await).unwrap();
  match resp {
    Response::Error(e) => {
      assert_eq!(e.id, 5);
      assert_eq!(e.error.code, -32601);
      assert!(e.error.message.contains("no_such_method"));
    }
    Response::Success(_) => panic!("expected error"),
  }
  drop(h.requests);
  let _ = h.sdk_handle.await.unwrap();
}

#[tokio::test]
async fn topic_error_returns_minus_32000_with_structured_data() {
  let mut h = Harness::spawn(ErroringTopic);
  h.send(req(6, method::INGEST, json!({"config": {}}))).await;
  let resp: Response = serde_json::from_value(h.recv().await).unwrap();
  match resp {
    Response::Error(e) => {
      assert_eq!(e.id, 6);
      assert_eq!(e.error.code, RPC_ERROR_CODE_TOPIC);
      let data: TopicErrorData =
        serde_json::from_value(e.error.data.unwrap()).unwrap();
      assert_eq!(
        data,
        TopicErrorData::Execution {
          activity: "ingest".to_string(),
          reason: "boom".to_string(),
        }
      );
    }
    Response::Success(_) => panic!("expected error"),
  }
  drop(h.requests);
  let _ = h.sdk_handle.await.unwrap();
}

#[tokio::test]
async fn set_data_emits_topic_data_set_notification() {
  let recorded = Arc::new(Mutex::new(Vec::new()));
  let topic = NotifyingTopic {
    recorded: recorded.clone(),
  };
  let mut h = Harness::spawn(topic);
  h.send(req(7, method::INGEST, json!({"config": {}}))).await;

  // The first line on the response stream should be the
  // notification (emitted from inside ingest before the response).
  let first: serde_json::Value = h.recv().await;
  assert_eq!(first["jsonrpc"], "2.0");
  assert_eq!(first["method"], "topic_data_set");
  assert_eq!(first["params"]["key"], "cursor");
  assert_eq!(first["params"]["value"], 42);
  assert!(
    first.get("id").is_none(),
    "notification must not carry id, got {first:?}"
  );

  // The second line is the ingest response itself.
  let second: serde_json::Value = h.recv().await;
  let resp: Response = serde_json::from_value(second).unwrap();
  match resp {
    Response::Success(s) => assert_eq!(s.id, 7),
    Response::Error(e) => panic!("expected success, got {:?}", e),
  }

  // Confirm the topic itself recorded the notification (independent
  // of what surfaced on the wire).
  let recorded = recorded.lock().unwrap();
  assert_eq!(recorded.len(), 1);
  assert_eq!(recorded[0].0, "cursor");
  assert_eq!(recorded[0].1, json!(42));

  drop(h.requests);
  let _ = h.sdk_handle.await.unwrap();
}

#[tokio::test]
async fn parse_failure_skips_line_and_continues() {
  let mut h = Harness::spawn(EmptyTopic);
  // Garbage line — SDK logs and continues.
  h.requests.write_all(b"not even json\n").await.unwrap();
  h.requests.flush().await.unwrap();
  // Then a real request.
  h.send(req(8, method::INIT, json!({}))).await;
  let resp: Response = serde_json::from_value(h.recv().await).unwrap();
  match resp {
    Response::Success(s) => assert_eq!(s.id, 8),
    Response::Error(e) => panic!("expected success after garbage, got {:?}", e),
  }
  drop(h.requests);
  let _ = h.sdk_handle.await.unwrap();
}

#[tokio::test]
async fn invalid_params_returns_minus_32602() {
  let mut h = Harness::spawn(TickingTopic);
  // ingest expects {"config": ...} but we send something
  // structurally wrong.
  h.send(req(9, method::INGEST, json!("not an object"))).await;
  let resp: Response = serde_json::from_value(h.recv().await).unwrap();
  match resp {
    Response::Error(e) => {
      assert_eq!(e.id, 9);
      assert_eq!(e.error.code, -32602);
    }
    Response::Success(_) => panic!("expected error"),
  }
  drop(h.requests);
  let _ = h.sdk_handle.await.unwrap();
}

// ── unused; kept for readability of the test imports above ───────────
#[allow(dead_code)]
fn _types_used(_n: Notification, _r: Request, _v: JsonRpcVersion) {}
