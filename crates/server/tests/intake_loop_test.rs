//! Tests for the intake agentic loop.
//!
//! Uses a scripted LlmClient that returns a programmed sequence of
//! `CompletionResponse` values per call.  Topics are stubbed as
//! in-process `impl Topic` types to avoid spawning subprocesses
//! (the loop is layer-agnostic — it talks to `Arc<dyn Topic>`).

use async_trait::async_trait;
use chrono::Utc;
use hyuqueue_core::{
  activity::{Activity, ActivityEffect, ActivityExecutor, ActivityInvocation},
  event::{Actor, Event, EventType, Locality},
  item::Item,
  topic::{Topic, TopicCtx, TopicError},
};
use hyuqueue_lib::llm::{
  Choice, CompletionRequest, CompletionResponse, LlmClient, LlmError,
  ResponseMessage, Role, ToolCall, ToolCallFunction,
};
use hyuqueue_server::topics::{TopicEntry, TopicRegistry};
use hyuqueue_server::workers::intake_loop::{run_loop, IntakeOutcome};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

// ── scripted LLM ─────────────────────────────────────────────────────

struct ScriptedLlm {
  /// Queued responses returned in FIFO order per call.
  queue: Mutex<Vec<CompletionResponse>>,
  /// Recorded requests for assertions.
  calls: Arc<Mutex<Vec<CompletionRequest>>>,
}

impl ScriptedLlm {
  fn new(responses: Vec<CompletionResponse>) -> Self {
    Self {
      queue: Mutex::new(responses),
      calls: Arc::new(Mutex::new(Vec::new())),
    }
  }

  fn record_handle(&self) -> Arc<Mutex<Vec<CompletionRequest>>> {
    self.calls.clone()
  }
}

#[async_trait]
impl LlmClient for ScriptedLlm {
  async fn complete(
    &self,
    req: CompletionRequest,
  ) -> Result<CompletionResponse, LlmError> {
    self.calls.lock().unwrap().push(req);
    let mut q = self.queue.lock().unwrap();
    if q.is_empty() {
      return Err(LlmError::UnexpectedResponse(
        "scripted LLM ran out of canned responses".to_string(),
      ));
    }
    Ok(q.remove(0))
  }
}

// ── response builders ────────────────────────────────────────────────

fn tool_call_response(
  calls: Vec<(&str, &str, serde_json::Value)>,
) -> CompletionResponse {
  let tool_calls = calls
    .into_iter()
    .map(|(id, name, args)| ToolCall {
      id: id.to_string(),
      tool_type: "function".to_string(),
      function: ToolCallFunction {
        name: name.to_string(),
        arguments: args.to_string(),
      },
    })
    .collect::<Vec<_>>();
  CompletionResponse {
    choices: vec![Choice {
      message: ResponseMessage {
        role: Role::Assistant,
        content: None,
        tool_calls: Some(tool_calls),
      },
      finish_reason: Some("tool_calls".to_string()),
    }],
  }
}

fn text_only_response(content: &str) -> CompletionResponse {
  CompletionResponse {
    choices: vec![Choice {
      message: ResponseMessage {
        role: Role::Assistant,
        content: Some(content.to_string()),
        tool_calls: None,
      },
      finish_reason: Some("stop".to_string()),
    }],
  }
}

// ── stub topics ──────────────────────────────────────────────────────

struct RecordingTopic {
  id: String,
  globals: Vec<Activity>,
  /// Records (activity_id, params, item_id) for each execute() call.
  invocations: Arc<Mutex<Vec<(String, serde_json::Value, Uuid)>>>,
  /// If Some, execute() returns this error.  If None, returns an ok event.
  fail_with: Option<TopicError>,
}

#[async_trait]
impl Topic for RecordingTopic {
  fn id(&self) -> &str {
    &self.id
  }
  fn display_name(&self) -> &str {
    "Recording"
  }
  fn global_activities(&self) -> Vec<Activity> {
    self.globals.clone()
  }
  async fn execute(
    &self,
    _ctx: &TopicCtx,
    invocation: &ActivityInvocation,
    item_id: Uuid,
  ) -> Result<Event, TopicError> {
    self.invocations.lock().unwrap().push((
      invocation.activity_id.clone(),
      invocation.params.clone(),
      item_id,
    ));
    if let Some(ref e) = self.fail_with {
      return Err(clone_topic_error(e));
    }
    Ok(Event {
      id: Uuid::new_v4(),
      event_type: EventType::ActionTaken,
      actor: Actor::Topic(self.id.clone()),
      locality: Locality::Local,
      payload: json!({
        "item_id": item_id,
        "activity_id": invocation.activity_id,
      }),
      created_at: Utc::now(),
    })
  }
}

fn clone_topic_error(e: &TopicError) -> TopicError {
  match e {
    TopicError::UnsupportedActivity(a, t) => {
      TopicError::UnsupportedActivity(a.clone(), t.clone())
    }
    TopicError::Execution { activity, reason } => TopicError::Execution {
      activity: activity.clone(),
      reason: reason.clone(),
    },
    TopicError::Configuration(s) => TopicError::Configuration(s.clone()),
  }
}

// ── fixtures ─────────────────────────────────────────────────────────

fn test_item(
  source_topic_id: Option<&str>,
  capabilities: Vec<Activity>,
) -> Item {
  let now = Utc::now();
  Item {
    id: Uuid::new_v4(),
    title: "test item".to_string(),
    body: Some("test body".to_string()),
    source_topic_id: source_topic_id.map(|s| s.to_string()),
    source: source_topic_id.unwrap_or("manual").to_string(),
    delegate_from: None,
    delegate_chain: vec![],
    capabilities,
    metadata: json!({}),
    created_at: now,
    updated_at: now,
  }
}

fn empty_object_schema() -> serde_json::Value {
  json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn item_activity(id: &str) -> Activity {
  Activity {
    id: id.to_string(),
    label: "Test".to_string(),
    key: 't',
    executor: ActivityExecutor::Local,
    effect: ActivityEffect::Write,
    description: format!("Test activity {id}"),
    params: empty_object_schema(),
    examples: vec![],
    human_only: false,
  }
}

/// Build a registry containing a single RecordingTopic with the
/// given globals.  Returns the registry plus a handle to the
/// topic's recorded invocations.
fn registry_with_topic(
  topic_id: &str,
  globals: Vec<Activity>,
  fail_with: Option<TopicError>,
) -> (TopicRegistry, Arc<Mutex<Vec<(String, serde_json::Value, Uuid)>>>) {
  let invocations = Arc::new(Mutex::new(Vec::new()));
  let topic = RecordingTopic {
    id: topic_id.to_string(),
    globals,
    invocations: invocations.clone(),
    fail_with,
  };
  let mut entries: HashMap<String, TopicEntry> = HashMap::new();
  entries.insert(
    topic_id.to_string(),
    TopicEntry {
      topic: Arc::new(topic),
      config: json!({}),
    },
  );
  // SAFETY-equivalent: construct via the only public path; we ship a
  // pub fn but rely on unsafe / private fields if not.  TopicRegistry
  // exposes `empty()` plus the internal HashMap; build_registry
  // populates it via subprocesses.  For tests we construct a minimal
  // shim by leaning on the test-only constructor.
  (TopicRegistry::from_entries_for_test(entries), invocations)
}

// ── tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn auto_resolved_on_first_turn() {
  let item = test_item(Some("example"), vec![]);
  let (registry, _invocations) = registry_with_topic("example", vec![], None);
  let llm = ScriptedLlm::new(vec![tool_call_response(vec![(
    "call-1",
    "auto_resolved",
    json!({"summary": "nothing to do"}),
  )])]);

  let outcome = run_loop(&llm, &registry, &item, "model", 8).await.unwrap();
  match outcome {
    IntakeOutcome::AutoResolved { summary, .. } => {
      assert_eq!(summary, "nothing to do");
    }
    other => panic!("expected AutoResolved, got {other:?}"),
  }
}

#[tokio::test]
async fn defer_to_human_on_first_turn() {
  let item = test_item(Some("example"), vec![]);
  let (registry, _invocations) = registry_with_topic("example", vec![], None);
  let llm = ScriptedLlm::new(vec![tool_call_response(vec![(
    "call-1",
    "defer_to_human",
    json!({"reason": "uncertain"}),
  )])]);

  let outcome = run_loop(&llm, &registry, &item, "model", 8).await.unwrap();
  match outcome {
    IntakeOutcome::DeferredToHuman { reason, .. } => {
      assert_eq!(reason, "uncertain");
    }
    other => panic!("expected DeferredToHuman, got {other:?}"),
  }
}

#[tokio::test]
async fn activity_then_auto_resolve() {
  let item = test_item(Some("example"), vec![item_activity("example.archive")]);
  let (registry, invocations) = registry_with_topic("example", vec![], None);
  let llm = ScriptedLlm::new(vec![
    tool_call_response(vec![("call-1", "example.archive", json!({}))]),
    tool_call_response(vec![(
      "call-2",
      "auto_resolved",
      json!({"summary": "archived"}),
    )]),
  ]);

  let outcome = run_loop(&llm, &registry, &item, "model", 8).await.unwrap();
  match outcome {
    IntakeOutcome::AutoResolved {
      summary,
      activity_events,
      ..
    } => {
      assert_eq!(summary, "archived");
      assert_eq!(activity_events.len(), 1);
      assert_eq!(activity_events[0].payload["activity_id"], "example.archive");
    }
    other => panic!("expected AutoResolved, got {other:?}"),
  }
  let recorded = invocations.lock().unwrap();
  assert_eq!(recorded.len(), 1);
  assert_eq!(recorded[0].0, "example.archive");
}

#[tokio::test]
async fn unknown_activity_returns_tool_error_loop_continues() {
  let item = test_item(Some("example"), vec![]);
  let (registry, _invocations) = registry_with_topic("example", vec![], None);
  // First turn: call an unknown activity.  Second turn: defer.
  let llm = ScriptedLlm::new(vec![
    tool_call_response(vec![("call-1", "nope.does_not_exist", json!({}))]),
    tool_call_response(vec![(
      "call-2",
      "defer_to_human",
      json!({"reason": "tool was unknown"}),
    )]),
  ]);

  let outcome = run_loop(&llm, &registry, &item, "model", 8).await.unwrap();
  match outcome {
    IntakeOutcome::DeferredToHuman {
      reason, transcript, ..
    } => {
      assert_eq!(reason, "tool was unknown");
      // Find the tool result for the unknown call.
      let tool_result = transcript
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("call-1"))
        .expect("expected a tool result for the unknown call");
      let content = tool_result.content.as_deref().unwrap_or("");
      assert!(
        content.contains("\"ok\":false"),
        "tool result should be an error, got {content:?}"
      );
    }
    other => panic!("expected DeferredToHuman, got {other:?}"),
  }
}

#[tokio::test]
async fn turn_budget_forces_defer() {
  let item = test_item(Some("example"), vec![item_activity("example.do")]);
  let (registry, _invocations) = registry_with_topic("example", vec![], None);
  // Three turns that each invoke an activity but never halt.
  let calls = (0..3).map(|i| {
    tool_call_response(vec![(
      Box::leak(format!("call-{i}").into_boxed_str()),
      "example.do",
      json!({}),
    )])
  });
  let llm = ScriptedLlm::new(calls.collect());

  let outcome = run_loop(&llm, &registry, &item, "model", 3).await.unwrap();
  match outcome {
    IntakeOutcome::DeferredToHuman { reason, .. } => {
      assert!(
        reason.contains("turn budget"),
        "expected turn-budget reason, got {reason:?}"
      );
    }
    other => panic!("expected DeferredToHuman after budget, got {other:?}"),
  }
}

#[tokio::test]
async fn no_tool_calls_defers_with_clear_reason() {
  let item = test_item(Some("example"), vec![]);
  let (registry, _invocations) = registry_with_topic("example", vec![], None);
  let llm =
    ScriptedLlm::new(vec![text_only_response("I'm not sure how to proceed.")]);

  let outcome = run_loop(&llm, &registry, &item, "model", 8).await.unwrap();
  match outcome {
    IntakeOutcome::DeferredToHuman {
      reason, transcript, ..
    } => {
      assert!(
        reason.contains("no tool calls"),
        "expected no-tool-calls reason, got {reason:?}"
      );
      // The assistant's text-only message should be in the
      // transcript so a human can see what the model said.
      assert!(
        transcript.iter().any(|m| matches!(m.role, Role::Assistant)
          && m.content.as_deref() == Some("I'm not sure how to proceed.")),
        "transcript should preserve the assistant's text"
      );
    }
    other => panic!("expected DeferredToHuman, got {other:?}"),
  }
}

#[tokio::test]
async fn activity_with_bad_json_args_yields_tool_error() {
  let item = test_item(Some("example"), vec![item_activity("example.do")]);
  let (registry, invocations) = registry_with_topic("example", vec![], None);
  // Manually craft a malformed tool call with non-JSON arguments,
  // then defer on the next turn.
  let bad_call = CompletionResponse {
    choices: vec![Choice {
      message: ResponseMessage {
        role: Role::Assistant,
        content: None,
        tool_calls: Some(vec![ToolCall {
          id: "bad".to_string(),
          tool_type: "function".to_string(),
          function: ToolCallFunction {
            name: "example.do".to_string(),
            arguments: "not json at all".to_string(),
          },
        }]),
      },
      finish_reason: Some("tool_calls".to_string()),
    }],
  };
  let llm = ScriptedLlm::new(vec![
    bad_call,
    tool_call_response(vec![(
      "call-2",
      "defer_to_human",
      json!({"reason": "couldn't parse"}),
    )]),
  ]);

  let outcome = run_loop(&llm, &registry, &item, "model", 8).await.unwrap();
  assert!(matches!(outcome, IntakeOutcome::DeferredToHuman { .. }));
  // The activity should NOT have been invoked since args didn't parse.
  assert!(invocations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn multiple_halts_in_one_response_is_rejected_with_feedback() {
  let item = test_item(Some("example"), vec![]);
  let (registry, _invocations) = registry_with_topic("example", vec![], None);
  let llm = ScriptedLlm::new(vec![
    // Turn 1: emit two halts in the same response — protocol violation.
    tool_call_response(vec![
      ("call-a", "auto_resolved", json!({"summary": "done"})),
      ("call-b", "defer_to_human", json!({"reason": "no actually"})),
    ]),
    // Turn 2: corrected, well-formed response.
    tool_call_response(vec![(
      "call-c",
      "defer_to_human",
      json!({"reason": "after retry"}),
    )]),
  ]);

  let outcome = run_loop(&llm, &registry, &item, "model", 8).await.unwrap();
  match outcome {
    IntakeOutcome::DeferredToHuman {
      reason, transcript, ..
    } => {
      assert_eq!(reason, "after retry");
      // Both bad-batch tool results should be in the transcript with
      // the protocol-violation error so the LLM sees what went wrong.
      let bad_results = transcript
        .iter()
        .filter(|m| {
          m.tool_call_id.as_deref() == Some("call-a")
            || m.tool_call_id.as_deref() == Some("call-b")
        })
        .collect::<Vec<_>>();
      assert_eq!(
        bad_results.len(),
        2,
        "both bad calls should get tool results"
      );
      for r in bad_results {
        let content = r.content.as_deref().unwrap_or("");
        assert!(
          content.contains("Invalid response") && content.contains("halt"),
          "expected invalid-response error, got {content:?}"
        );
      }
    }
    other => panic!("expected DeferredToHuman, got {other:?}"),
  }
}

#[tokio::test]
async fn activity_and_halt_in_same_response_both_processed() {
  let item = test_item(Some("example"), vec![item_activity("example.archive")]);
  let (registry, invocations) = registry_with_topic("example", vec![], None);
  // One response with both an activity invocation and a halt.  The
  // halt is OK (just one), so both should be dispatched in parallel
  // and the activity event should not be dropped.
  let llm = ScriptedLlm::new(vec![tool_call_response(vec![
    ("call-1", "example.archive", json!({})),
    ("call-2", "auto_resolved", json!({"summary": "archived"})),
  ])]);

  let outcome = run_loop(&llm, &registry, &item, "model", 8).await.unwrap();
  match outcome {
    IntakeOutcome::AutoResolved {
      summary,
      activity_events,
      ..
    } => {
      assert_eq!(summary, "archived");
      assert_eq!(
        activity_events.len(),
        1,
        "activity event should be captured alongside the halt"
      );
      assert_eq!(
        invocations.lock().unwrap().len(),
        1,
        "activity should have been dispatched"
      );
    }
    other => panic!(
      "expected AutoResolved with activity event preserved, got {other:?}"
    ),
  }
}

#[tokio::test]
async fn tools_include_halt_and_item_activities_only() {
  // Verify the request the LLM sees — without invoking the loop end
  // to end.  We just trigger one turn and check the recorded request.
  let item = test_item(Some("example"), vec![item_activity("example.archive")]);
  let (registry, _invocations) =
    registry_with_topic("example", vec![item_activity("example.global")], None);
  let llm = ScriptedLlm::new(vec![tool_call_response(vec![(
    "call-1",
    "auto_resolved",
    json!({"summary": "ok"}),
  )])]);
  let recorded = llm.record_handle();

  let _ = run_loop(&llm, &registry, &item, "model", 8).await.unwrap();

  let calls = recorded.lock().unwrap();
  let req = calls.first().expect("at least one request");
  let tool_names = req
    .tools
    .as_ref()
    .unwrap()
    .iter()
    .map(|t| t.function.name.clone())
    .collect::<Vec<_>>();
  assert!(tool_names.contains(&"auto_resolved".to_string()));
  assert!(tool_names.contains(&"defer_to_human".to_string()));
  assert!(tool_names.contains(&"example.archive".to_string()));
  assert!(tool_names.contains(&"example.global".to_string()));
}
