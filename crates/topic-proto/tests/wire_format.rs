//! Wire-format and round-trip tests for the topic-proto envelope and
//! payload types.
//!
//! Two patterns are used:
//!
//! - *Wire shape*: serialize a value and assert the JSON matches a
//!   canonical `json!()` literal.  Catches accidental field renames
//!   or shape drift that would break the protocol.
//! - *Round-trip*: serialize → deserialize → serialize again, assert
//!   the two JSON forms are identical.  Doesn't require `PartialEq`
//!   on the underlying types.

use hyuqueue_core::activity::{
  Activity, ActivityEffect, ActivityExecutor, ActivityInvocation,
};
use hyuqueue_core::event::{Actor, Event, EventType, Locality};
use hyuqueue_core::topic::{IngestItem, TopicError};
use hyuqueue_topic_proto::{
  envelope::{
    ErrorResponse, Notification, Request, Response, RpcError, SuccessResponse,
  },
  error::{topic_error_to_rpc_error, TopicErrorData, RPC_ERROR_CODE_TOPIC},
  method,
  payload::{
    ExecuteRequest, ExecuteResponse, IngestRequest, IngestResponse,
    InitRequest, InitResponse, ShutdownRequest, TopicDataSetParams,
  },
  version::JsonRpcVersion,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use uuid::Uuid;

// ── helpers ──────────────────────────────────────────────────────────

/// Serialize, deserialize, serialize again, assert the two JSON forms
/// are identical.
fn round_trip<T: Serialize + DeserializeOwned>(value: &T) {
  let first = serde_json::to_value(value).expect("first serialize");
  let parsed: T = serde_json::from_value(first.clone()).expect("deserialize");
  let second = serde_json::to_value(&parsed).expect("second serialize");
  assert_eq!(first, second, "round-trip changed the wire form");
}

// ── version ──────────────────────────────────────────────────────────

#[test]
fn json_rpc_version_serializes_as_literal_string() {
  let v = serde_json::to_value(JsonRpcVersion).unwrap();
  assert_eq!(v, json!("2.0"));
}

#[test]
fn json_rpc_version_rejects_non_2_0() {
  let err = serde_json::from_value::<JsonRpcVersion>(json!("1.0")).unwrap_err();
  assert!(err.to_string().contains("2.0"), "{err}");
}

#[test]
fn json_rpc_version_rejects_non_string() {
  assert!(serde_json::from_value::<JsonRpcVersion>(json!(2.0)).is_err());
  assert!(serde_json::from_value::<JsonRpcVersion>(json!(null)).is_err());
}

// ── envelope wire shapes ─────────────────────────────────────────────

#[test]
fn request_wire_shape_matches_jsonrpc_2() {
  let req = Request {
    jsonrpc: JsonRpcVersion,
    method: method::INGEST.to_string(),
    params: json!({"config": {"feed_url": "https://example.com/rss"}}),
    id: 7,
  };
  let v = serde_json::to_value(&req).unwrap();
  assert_eq!(
    v,
    json!({
      "jsonrpc": "2.0",
      "method": "ingest",
      "params": {"config": {"feed_url": "https://example.com/rss"}},
      "id": 7,
    })
  );
}

#[test]
fn notification_has_no_id_field() {
  let note = Notification {
    jsonrpc: JsonRpcVersion,
    method: method::TOPIC_DATA_SET.to_string(),
    params: json!({"key": "cursor", "value": 42}),
  };
  let v = serde_json::to_value(&note).unwrap();
  assert_eq!(
    v,
    json!({
      "jsonrpc": "2.0",
      "method": "topic_data_set",
      "params": {"key": "cursor", "value": 42},
    })
  );
  assert!(v.get("id").is_none(), "notification must not carry id");
}

#[test]
fn success_response_wire_shape() {
  let resp = SuccessResponse {
    jsonrpc: JsonRpcVersion,
    result: json!({"ok": true}),
    id: 1,
  };
  assert_eq!(
    serde_json::to_value(&resp).unwrap(),
    json!({"jsonrpc": "2.0", "result": {"ok": true}, "id": 1})
  );
}

#[test]
fn error_response_wire_shape() {
  let resp = ErrorResponse {
    jsonrpc: JsonRpcVersion,
    error: RpcError {
      code: RPC_ERROR_CODE_TOPIC,
      message: "boom".to_string(),
      data: Some(json!({"kind": "execution"})),
    },
    id: 2,
  };
  assert_eq!(
    serde_json::to_value(&resp).unwrap(),
    json!({
      "jsonrpc": "2.0",
      "error": {
        "code": -32000,
        "message": "boom",
        "data": {"kind": "execution"},
      },
      "id": 2,
    })
  );
}

#[test]
fn rpc_error_omits_data_when_none() {
  let err = RpcError {
    code: RPC_ERROR_CODE_TOPIC,
    message: "no detail".to_string(),
    data: None,
  };
  let v = serde_json::to_value(&err).unwrap();
  assert_eq!(v, json!({"code": -32000, "message": "no detail"}));
}

// ── response untagged dispatch ───────────────────────────────────────

#[test]
fn response_deserializes_success_when_result_present() {
  let raw = json!({"jsonrpc": "2.0", "result": {"x": 1}, "id": 5});
  let parsed: Response = serde_json::from_value(raw).unwrap();
  match parsed {
    Response::Success(s) => assert_eq!(s.id, 5),
    Response::Error(_) => panic!("should have parsed as Success"),
  }
}

#[test]
fn response_deserializes_error_when_error_present() {
  let raw = json!({
    "jsonrpc": "2.0",
    "error": {"code": -32000, "message": "x"},
    "id": 5,
  });
  let parsed: Response = serde_json::from_value(raw).unwrap();
  match parsed {
    Response::Error(e) => {
      assert_eq!(e.id, 5);
      assert_eq!(e.error.code, -32000);
    }
    Response::Success(_) => panic!("should have parsed as Error"),
  }
}

// ── per-method payload round-trips ───────────────────────────────────

#[test]
fn init_request_round_trips() {
  round_trip(&InitRequest::default());
}

#[test]
fn init_response_round_trips() {
  let resp = InitResponse {
    id: "example".to_string(),
    display_name: "Example Topic".to_string(),
    supports_ingest: true,
    supports_execute: true,
    item_activities: vec![Activity {
      id: "example.reset".to_string(),
      label: "Reset".to_string(),
      key: 'r',
      executor: ActivityExecutor::Local,
      effect: ActivityEffect::Write,
      description: "Reset the example counter to zero.".to_string(),
      params: json!({"type": "object", "properties": {}}),
      examples: vec![],
      human_only: false,
    }],
    global_activities: vec![],
  };
  round_trip(&resp);
}

#[test]
fn init_response_omitted_activities_default_to_empty() {
  let raw = json!({
    "id": "x",
    "display_name": "X",
    "supports_ingest": false,
    "supports_execute": false,
  });
  let parsed: InitResponse = serde_json::from_value(raw).unwrap();
  assert!(parsed.item_activities.is_empty());
  assert!(parsed.global_activities.is_empty());
}

#[test]
fn ingest_request_round_trips() {
  round_trip(&IngestRequest {
    config: json!({"feed_url": "https://example.com"}),
  });
}

#[test]
fn ingest_response_round_trips() {
  round_trip(&IngestResponse {
    items: vec![IngestItem {
      title: "Tick #1".to_string(),
      source: "example".to_string(),
      body: None,
      metadata: json!({"counter": 1}),
    }],
  });
}

#[test]
fn execute_request_round_trips() {
  round_trip(&ExecuteRequest {
    invocation: ActivityInvocation {
      activity_id: "example.reset".to_string(),
      params: json!({}),
    },
    item_id: Uuid::nil(),
  });
}

#[test]
fn execute_response_round_trips() {
  round_trip(&ExecuteResponse {
    event: Event {
      id: Uuid::nil(),
      event_type: EventType::ActionTaken,
      actor: Actor::Topic("example".to_string()),
      locality: Locality::Local,
      payload: json!({
        "item_id": Uuid::nil(),
        "activity_id": "example.reset",
        "params": {},
      }),
      created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
        .unwrap(),
    },
  });
}

#[test]
fn topic_data_set_params_round_trips() {
  round_trip(&TopicDataSetParams {
    key: "cursor".to_string(),
    value: json!(42),
  });
}

#[test]
fn topic_data_set_params_does_not_carry_topic_id() {
  // Topic identity comes from the host's view of which subprocess sent
  // the notification — the wire format intentionally does not let the
  // topic claim its own id here.
  let v = serde_json::to_value(&TopicDataSetParams {
    key: "k".to_string(),
    value: json!(null),
  })
  .unwrap();
  assert!(v.get("topic_id").is_none());
}

#[test]
fn shutdown_request_round_trips() {
  round_trip(&ShutdownRequest::default());
}

// ── error mapping ────────────────────────────────────────────────────

#[test]
fn unsupported_activity_maps_to_rpc_error() {
  let err =
    TopicError::UnsupportedActivity("foo".to_string(), "bar".to_string());
  let rpc = topic_error_to_rpc_error(&err).unwrap();
  assert_eq!(rpc.code, RPC_ERROR_CODE_TOPIC);
  assert!(rpc.message.contains("foo"));
  let data: TopicErrorData = serde_json::from_value(rpc.data.unwrap()).unwrap();
  assert_eq!(
    data,
    TopicErrorData::UnsupportedActivity {
      activity_id: "foo".to_string(),
      topic_id: "bar".to_string(),
    }
  );
}

#[test]
fn execution_error_maps_to_rpc_error() {
  let err = TopicError::Execution {
    activity: "send".to_string(),
    reason: "smtp died".to_string(),
  };
  let rpc = topic_error_to_rpc_error(&err).unwrap();
  assert_eq!(rpc.code, RPC_ERROR_CODE_TOPIC);
  let data: TopicErrorData = serde_json::from_value(rpc.data.unwrap()).unwrap();
  assert_eq!(
    data,
    TopicErrorData::Execution {
      activity: "send".to_string(),
      reason: "smtp died".to_string(),
    }
  );
}

#[test]
fn configuration_error_maps_to_rpc_error() {
  let err = TopicError::Configuration("missing api key".to_string());
  let rpc = topic_error_to_rpc_error(&err).unwrap();
  let data: TopicErrorData = serde_json::from_value(rpc.data.unwrap()).unwrap();
  assert_eq!(
    data,
    TopicErrorData::Configuration {
      detail: "missing api key".to_string(),
    }
  );
}

#[test]
fn topic_error_data_round_trips_through_json() {
  let cases = [
    TopicErrorData::UnsupportedActivity {
      activity_id: "a".to_string(),
      topic_id: "t".to_string(),
    },
    TopicErrorData::Execution {
      activity: "x".to_string(),
      reason: "y".to_string(),
    },
    TopicErrorData::Configuration {
      detail: "z".to_string(),
    },
  ];
  for case in cases {
    let json = serde_json::to_value(&case).unwrap();
    let back: TopicErrorData = serde_json::from_value(json).unwrap();
    assert_eq!(case, back);
  }
}

// ── method names ─────────────────────────────────────────────────────

#[test]
fn method_constants_are_stable_strings() {
  assert_eq!(method::INIT, "init");
  assert_eq!(method::INGEST, "ingest");
  assert_eq!(method::EXECUTE, "execute");
  assert_eq!(method::TOPIC_DATA_SET, "topic_data_set");
  assert_eq!(method::SHUTDOWN, "shutdown");
}
