//! Integration tests for `SubprocessTopic`.
//!
//! Each test sets up two `tokio::io::duplex` pipes — one direction
//! per pipe — connecting a `SubprocessTopic` (host side) to a
//! `FakeRemote` (test side that plays the topic).  The test drives
//! the fake by hand: read incoming requests, send back responses or
//! notifications.  No real subprocess, no real binary; just the
//! dispatch logic exercised against the wire format.

use async_trait::async_trait;
use hyuqueue_core::activity::{
  Activity, ActivityEffect, ActivityExecutor, ActivityInvocation,
};
use hyuqueue_core::topic::{Topic, TopicCtx, TopicError};
use hyuqueue_topic_host::{
  HostError, NoopSink, SubprocessTopic, TopicDataSink,
};
use hyuqueue_topic_proto::envelope::{
  ErrorResponse, Notification, Request, RpcError, SuccessResponse,
};
use hyuqueue_topic_proto::error::{TopicErrorData, RPC_ERROR_CODE_TOPIC};
use hyuqueue_topic_proto::method;
use hyuqueue_topic_proto::payload::{
  ExecuteResponse, IngestResponse, InitResponse, TopicDataSetParams,
};
use hyuqueue_topic_proto::version::JsonRpcVersion;
use serde::Serialize;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use uuid::Uuid;

// ── fake remote (the topic side of the wire) ─────────────────────────

struct FakeRemote {
  reader: BufReader<DuplexStream>,
  writer: DuplexStream,
}

impl FakeRemote {
  async fn next_request(&mut self) -> Request {
    let mut line = String::new();
    let n = self.reader.read_line(&mut line).await.unwrap();
    assert!(n > 0, "expected a request, got EOF");
    serde_json::from_str(line.trim_end()).unwrap()
  }

  async fn respond_success<T: Serialize>(&mut self, id: u64, result: T) {
    let resp = SuccessResponse {
      jsonrpc: JsonRpcVersion,
      result: serde_json::to_value(result).unwrap(),
      id,
    };
    self.write_value(&resp).await;
  }

  async fn respond_error(&mut self, id: u64, error: RpcError) {
    let resp = ErrorResponse {
      jsonrpc: JsonRpcVersion,
      error,
      id,
    };
    self.write_value(&resp).await;
  }

  async fn send_notification<T: Serialize>(&mut self, method: &str, params: T) {
    let note = Notification {
      jsonrpc: JsonRpcVersion,
      method: method.to_string(),
      params: serde_json::to_value(params).unwrap(),
    };
    self.write_value(&note).await;
  }

  async fn write_value<T: Serialize>(&mut self, value: &T) {
    let line = serde_json::to_string(value).unwrap();
    self.writer.write_all(line.as_bytes()).await.unwrap();
    self.writer.write_all(b"\n").await.unwrap();
    self.writer.flush().await.unwrap();
  }
}

fn make_pipes() -> (DuplexStream, DuplexStream, FakeRemote) {
  let (fake_w, sdk_r) = tokio::io::duplex(8192);
  let (sdk_w, fake_r) = tokio::io::duplex(8192);
  let fake = FakeRemote {
    reader: BufReader::new(fake_r),
    writer: fake_w,
  };
  (sdk_r, sdk_w, fake)
}

fn default_init_response(id: &str) -> InitResponse {
  InitResponse {
    id: id.to_string(),
    display_name: format!("Display for {id}"),
    supports_ingest: true,
    supports_execute: true,
    item_activities: vec![],
    global_activities: vec![],
  }
}

/// Set up a SubprocessTopic and FakeRemote, performing the init
/// handshake.  Returns once the handshake has completed.
async fn setup(
  expected_id: &str,
  init_response: InitResponse,
  sink: Arc<dyn TopicDataSink>,
) -> (SubprocessTopic, FakeRemote) {
  let (sdk_r, sdk_w, mut fake) = make_pipes();
  let topic_fut = SubprocessTopic::from_io(
    expected_id,
    sdk_r,
    sdk_w,
    sink,
    std::collections::HashMap::new(),
  );
  let fake_fut = async {
    let req = fake.next_request().await;
    assert_eq!(req.method, method::INIT);
    fake.respond_success(req.id, &init_response).await;
    fake
  };
  let (topic_res, fake) = tokio::join!(topic_fut, fake_fut);
  (topic_res.unwrap(), fake)
}

// ── recording sink ───────────────────────────────────────────────────

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

// ── tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn init_handshake_carries_topic_data_snapshot() {
  // Verifies the persisted topic_data snapshot the host hands the
  // subprocess at construction time actually rides on the init wire
  // payload — the topic-side counterpart of `Topic::init` hydration.
  let (sdk_r, sdk_w, mut fake) = make_pipes();
  let mut initial_data = std::collections::HashMap::new();
  initial_data.insert("counter".to_string(), json!(42));
  initial_data.insert("cursor".to_string(), json!("2024-01-01"));

  let topic_fut = SubprocessTopic::from_io(
    "example",
    sdk_r,
    sdk_w,
    Arc::new(NoopSink),
    initial_data,
  );
  let fake_fut = async {
    let req = fake.next_request().await;
    assert_eq!(req.method, method::INIT);
    let data = &req.params["topic_data"];
    assert!(data.is_object(), "expected topic_data object, got {data}");
    assert_eq!(data["counter"], json!(42));
    assert_eq!(data["cursor"], json!("2024-01-01"));
    fake
      .respond_success(req.id, default_init_response("example"))
      .await;
    fake
  };
  let (topic_res, _fake) = tokio::join!(topic_fut, fake_fut);
  topic_res.expect("handshake should succeed");
}

#[tokio::test]
async fn init_handshake_caches_topic_identity() {
  let init = InitResponse {
    id: "example".to_string(),
    display_name: "Example".to_string(),
    supports_ingest: true,
    supports_execute: true,
    item_activities: vec![Activity {
      id: "example.reset".to_string(),
      label: "Reset".to_string(),
      key: 'r',
      executor: ActivityExecutor::Local,
      effect: ActivityEffect::Write,
      description: "Reset the example counter.".to_string(),
      params: json!({"type": "object"}),
      examples: vec![],
      human_only: false,
    }],
    global_activities: vec![],
  };
  let (topic, _fake) = setup("example", init.clone(), Arc::new(NoopSink)).await;
  assert_eq!(topic.id(), "example");
  assert_eq!(topic.display_name(), "Example");
  assert_eq!(topic.item_activities().len(), 1);
  assert_eq!(topic.item_activities()[0].id, "example.reset");
  assert_eq!(topic.global_activities().len(), 0);
}

#[tokio::test]
async fn init_handshake_fails_on_id_mismatch() {
  let (sdk_r, sdk_w, mut fake) = make_pipes();
  let topic_fut = SubprocessTopic::from_io(
    "expected",
    sdk_r,
    sdk_w,
    Arc::new(NoopSink),
    std::collections::HashMap::new(),
  );
  let fake_fut = async {
    let req = fake.next_request().await;
    fake
      .respond_success(req.id, default_init_response("actual"))
      .await;
  };
  let (topic_res, _) = tokio::join!(topic_fut, fake_fut);
  match topic_res {
    Err(HostError::IdMismatch { expected, actual }) => {
      assert_eq!(expected, "expected");
      assert_eq!(actual, "actual");
    }
    Err(other) => panic!("expected IdMismatch, got {:?}", other),
    Ok(_) => panic!("expected IdMismatch, got Ok"),
  }
}

#[tokio::test]
async fn ingest_returns_items() {
  let (topic, mut fake) =
    setup("t", default_init_response("t"), Arc::new(NoopSink)).await;

  let ctx = TopicCtx::stub();
  let config = json!({"feed": "x"});
  let topic_handle = tokio::spawn(async move {
    let items = topic.ingest(&ctx, &config).await.unwrap();
    (items, topic)
  });

  let req = fake.next_request().await;
  assert_eq!(req.method, method::INGEST);
  fake
    .respond_success(
      req.id,
      IngestResponse {
        items: vec![hyuqueue_core::topic::IngestItem {
          title: "tick".to_string(),
          body: None,
          external_id: None,
          metadata: json!({"n": 1}),
        }],
      },
    )
    .await;

  let (items, _topic) = topic_handle.await.unwrap();
  assert_eq!(items.len(), 1);
  assert_eq!(items[0].title, "tick");
}

#[tokio::test]
async fn execute_returns_event() {
  let (topic, mut fake) =
    setup("t", default_init_response("t"), Arc::new(NoopSink)).await;

  let ctx = TopicCtx::stub();
  let invocation = ActivityInvocation {
    activity_id: "t.do".to_string(),
    params: json!({}),
  };
  let item_id = Uuid::new_v4();
  let topic_handle = tokio::spawn(async move {
    let event = topic.execute(&ctx, &invocation, item_id).await.unwrap();
    (event, topic)
  });

  let req = fake.next_request().await;
  assert_eq!(req.method, method::EXECUTE);
  fake
    .respond_success(
      req.id,
      ExecuteResponse {
        event: hyuqueue_core::event::Event {
          id: Uuid::nil(),
          event_type: hyuqueue_core::event::EventType::ActionTaken,
          actor: hyuqueue_core::event::Actor::Topic("t".to_string()),
          locality: hyuqueue_core::event::Locality::Local,
          payload: json!({"item_id": item_id, "activity_id": "t.do"}),
          created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
            .unwrap(),
        },
      },
    )
    .await;

  let (event, _topic) = topic_handle.await.unwrap();
  assert_eq!(event.payload["activity_id"], "t.do");
}

#[tokio::test]
async fn topic_error_round_trips_to_typed_variant() {
  let (topic, mut fake) =
    setup("t", default_init_response("t"), Arc::new(NoopSink)).await;

  let ctx = TopicCtx::stub();
  let config = json!({});
  let topic_handle = tokio::spawn(async move {
    let result = topic.ingest(&ctx, &config).await;
    (result, topic)
  });

  let req = fake.next_request().await;
  let data = serde_json::to_value(TopicErrorData::UnsupportedActivity {
    activity_id: "foo".to_string(),
    topic_id: "t".to_string(),
  })
  .unwrap();
  fake
    .respond_error(
      req.id,
      RpcError {
        code: RPC_ERROR_CODE_TOPIC,
        message: "Activity 'foo' is not supported by topic 't'".to_string(),
        data: Some(data),
      },
    )
    .await;

  let (result, _topic) = topic_handle.await.unwrap();
  match result {
    Err(TopicError::UnsupportedActivity(activity, topic_id)) => {
      assert_eq!(activity, "foo");
      assert_eq!(topic_id, "t");
    }
    other => panic!("expected UnsupportedActivity, got {:?}", other),
  }
}

#[tokio::test]
async fn non_topic_rpc_error_collapses_to_execution() {
  let (topic, mut fake) =
    setup("t", default_init_response("t"), Arc::new(NoopSink)).await;

  let ctx = TopicCtx::stub();
  let config = json!({});
  let topic_handle = tokio::spawn(async move {
    let result = topic.ingest(&ctx, &config).await;
    (result, topic)
  });

  let req = fake.next_request().await;
  fake
    .respond_error(
      req.id,
      RpcError {
        code: -32601, // method not found — shouldn't happen for ingest, but exercises mapping
        message: "Method not found: ingest".to_string(),
        data: None,
      },
    )
    .await;

  let (result, _topic) = topic_handle.await.unwrap();
  match result {
    Err(TopicError::Execution { activity, reason }) => {
      assert_eq!(activity, "ingest");
      assert!(reason.contains("Method not found"));
    }
    other => panic!("expected Execution, got {:?}", other),
  }
}

#[tokio::test]
async fn topic_data_set_notification_reaches_sink() {
  let calls = Arc::new(Mutex::new(Vec::new()));
  let sink = Arc::new(RecordingSink {
    calls: calls.clone(),
  });
  let (_topic, mut fake) = setup("t", default_init_response("t"), sink).await;

  fake
    .send_notification(
      method::TOPIC_DATA_SET,
      TopicDataSetParams {
        key: "cursor".to_string(),
        value: json!(42),
      },
    )
    .await;

  // Give the reader task a tick to process the notification.  The
  // sink call is async but cheap — a yield is enough.
  for _ in 0..10 {
    if !calls.lock().unwrap().is_empty() {
      break;
    }
    tokio::task::yield_now().await;
  }

  let calls = calls.lock().unwrap();
  assert_eq!(calls.len(), 1);
  assert_eq!(calls[0].0, "t");
  assert_eq!(calls[0].1, "cursor");
  assert_eq!(calls[0].2, json!(42));
}

#[tokio::test]
async fn concurrent_requests_resolve_correctly_out_of_order() {
  let (topic, mut fake) =
    setup("t", default_init_response("t"), Arc::new(NoopSink)).await;
  let topic = Arc::new(topic);

  let ctx_a = TopicCtx::stub();
  let topic_a = topic.clone();
  let a_handle = tokio::spawn(async move {
    topic_a.ingest(&ctx_a, &json!({"which": "a"})).await
  });
  let ctx_b = TopicCtx::stub();
  let topic_b = topic.clone();
  let b_handle = tokio::spawn(async move {
    topic_b.ingest(&ctx_b, &json!({"which": "b"})).await
  });

  // Read both requests, respond in reverse id order.
  let req1 = fake.next_request().await;
  let req2 = fake.next_request().await;
  // Respond to req2 first, then req1 — out of order on the wire.
  fake
    .respond_success(
      req2.id,
      IngestResponse {
        items: vec![hyuqueue_core::topic::IngestItem {
          title: "from-req2".to_string(),
          body: None,
          external_id: None,
          metadata: json!({}),
        }],
      },
    )
    .await;
  fake
    .respond_success(
      req1.id,
      IngestResponse {
        items: vec![hyuqueue_core::topic::IngestItem {
          title: "from-req1".to_string(),
          body: None,
          external_id: None,
          metadata: json!({}),
        }],
      },
    )
    .await;

  let a_items = a_handle.await.unwrap().unwrap();
  let b_items = b_handle.await.unwrap().unwrap();
  // The earlier-issued request (req1) was answered with "from-req1",
  // independent of the response order on the wire.
  assert_eq!(a_items[0].title, "from-req1");
  assert_eq!(b_items[0].title, "from-req2");
}

#[tokio::test]
async fn connection_closed_during_request_returns_execution_error() {
  let (topic, mut fake) =
    setup("t", default_init_response("t"), Arc::new(NoopSink)).await;

  let ctx = TopicCtx::stub();
  let config = json!({});
  let topic_handle = tokio::spawn(async move {
    let result = topic.ingest(&ctx, &config).await;
    (result, topic)
  });

  // Read the request, then close the writer without responding.
  let _req = fake.next_request().await;
  drop(fake);

  let (result, _topic) = topic_handle.await.unwrap();
  match result {
    Err(TopicError::Execution { activity, reason }) => {
      assert_eq!(activity, "ingest");
      assert!(
        reason.contains("closed") || reason.contains("connection"),
        "unexpected reason: {reason}"
      );
    }
    other => {
      panic!("expected Execution after connection close, got {:?}", other)
    }
  }
}
