//! Agentic loop for the intake LLM worker.
//!
//! Multi-turn conversation with the LLM, exposing per-item activities
//! and two halt tools.  The loop terminates when the LLM calls a
//! halt, exceeds its turn budget (forced defer_to_human), or
//! misbehaves (no tool calls = defer with reason).
//!
//! v1 has no source instructions and no confidence gating — those
//! land in a follow-up.  The system prompt is generic and the LLM
//! trusts its own judgment.

use crate::topics::TopicRegistry;
use futures::future::join_all;
use hyuqueue_core::{
  activity::{Activity, ActivityEffect, ActivityExecutor, ActivityInvocation},
  event::Event,
  item::Item,
  topic::TopicCtx,
};
use hyuqueue_lib::llm::{
  CompletionRequest, LlmClient, LlmError, Message, Tool, ToolCall, ToolFunction,
};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::warn;

// ── public outcome / error types ─────────────────────────────────────

#[derive(Debug, Clone)]
pub enum IntakeOutcome {
  AutoResolved {
    summary: String,
    transcript: Vec<Message>,
    activity_events: Vec<Event>,
  },
  DeferredToHuman {
    reason: String,
    transcript: Vec<Message>,
    activity_events: Vec<Event>,
  },
}

#[derive(Debug, Error)]
pub enum LoopError {
  #[error("LLM call failed: {0}")]
  Llm(#[from] LlmError),
}

// ── reserved halt tool names ─────────────────────────────────────────

pub const HALT_AUTO_RESOLVED: &str = "auto_resolved";
pub const HALT_DEFER_TO_HUMAN: &str = "defer_to_human";

// ── tool surface ─────────────────────────────────────────────────────

/// Build the tool surface the LLM sees for this item.  Includes:
/// - The two halt tools (always).
/// - Item-scoped activities from `item.capabilities`.
/// - Global activities from every registered topic.
///
/// Filters out:
/// - Activities marked `human_only` (intake LLM cannot fire them).
/// - Activities with `executor: Upstream` (cross-instance routing
///   is deferred; for now intake only invokes local activities).
/// - Activities whose id collides with a halt name (defensive —
///   skipped with a warning so a misconfigured topic doesn't quietly
///   override the halt semantics).
pub fn build_tools(item: &Item, registry: &TopicRegistry) -> Vec<Tool> {
  let mut tools = vec![
    halt_tool(
      HALT_AUTO_RESOLVED,
      "Conclude that this item is handled and no human attention is \
       needed.  Pass a short summary of what was done (or why nothing \
       needed doing).",
      json!({
        "type": "object",
        "properties": {
          "summary": { "type": "string" }
        },
        "required": ["summary"],
        "additionalProperties": false,
      }),
    ),
    halt_tool(
      HALT_DEFER_TO_HUMAN,
      "Escalate this item to the human queue.  Pass a reason \
       explaining what is uncertain or what specifically requires \
       human judgment.",
      json!({
        "type": "object",
        "properties": {
          "reason": { "type": "string" }
        },
        "required": ["reason"],
        "additionalProperties": false,
      }),
    ),
  ];

  // Item-scoped activities.
  tools.extend(
    item
      .capabilities
      .iter()
      .filter(|a| is_intake_invocable(a))
      .map(activity_to_tool),
  );

  // Global activities from all topics.
  tools.extend(
    registry
      .entries()
      .values()
      .flat_map(|entry| entry.topic.global_activities())
      .filter(is_intake_invocable)
      .map(|a| activity_to_tool(&a)),
  );

  tools
}

fn is_intake_invocable(act: &Activity) -> bool {
  if act.human_only {
    return false;
  }
  if !matches!(act.executor, ActivityExecutor::Local) {
    return false;
  }
  if is_reserved(&act.id) {
    warn!(
      activity = %act.id,
      "Activity id collides with reserved halt name; skipping"
    );
    return false;
  }
  // ActivityEffect::Write is OK for intake (the topic author has
  // explicitly exposed it; human_only is the escape hatch for
  // operations that should always require a human).
  let _ = ActivityEffect::Write;
  true
}

fn halt_tool(name: &str, description: &str, parameters: Value) -> Tool {
  Tool {
    tool_type: "function".to_string(),
    function: ToolFunction {
      name: name.to_string(),
      description: description.to_string(),
      parameters,
    },
  }
}

fn activity_to_tool(act: &Activity) -> Tool {
  Tool {
    tool_type: "function".to_string(),
    function: ToolFunction {
      name: act.id.clone(),
      description: act.description.clone(),
      parameters: act.params.clone(),
    },
  }
}

fn is_reserved(name: &str) -> bool {
  name == HALT_AUTO_RESOLVED || name == HALT_DEFER_TO_HUMAN
}

// ── dispatch ─────────────────────────────────────────────────────────

#[derive(Debug)]
enum HaltReason {
  AutoResolved { summary: String },
  DeferredToHuman { reason: String },
}

enum DispatchResult {
  Halt(HaltReason),
  ActivityResult { event: Event, tool_result: String },
  ToolError(String),
}

async fn dispatch_tool_call(
  call: &ToolCall,
  item: &Item,
  registry: &TopicRegistry,
  ctx: &TopicCtx,
) -> DispatchResult {
  let name = &call.function.name;
  let args: Value = match serde_json::from_str(&call.function.arguments) {
    Ok(v) => v,
    Err(e) => {
      return DispatchResult::ToolError(format!(
        "invalid JSON arguments for '{name}': {e}"
      ));
    }
  };

  if name == HALT_AUTO_RESOLVED {
    return DispatchResult::Halt(HaltReason::AutoResolved {
      summary: args
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string(),
    });
  }
  if name == HALT_DEFER_TO_HUMAN {
    return DispatchResult::Halt(HaltReason::DeferredToHuman {
      reason: args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("(no reason given)")
        .to_string(),
    });
  }

  // Activity: <topic_id>.<activity_name>
  let Some((topic_id, _)) = name.split_once('.') else {
    return DispatchResult::ToolError(format!(
      "unknown tool '{name}' (no '.' prefix and not a halt)"
    ));
  };
  let Some(entry) = registry.entries().get(topic_id) else {
    return DispatchResult::ToolError(format!(
      "no topic '{topic_id}' registered"
    ));
  };

  let invocation = ActivityInvocation {
    activity_id: name.clone(),
    params: args,
  };

  match entry.topic.execute(ctx, &invocation, item.id).await {
    Ok(event) => {
      let tool_result = serde_json::to_string(&json!({
        "ok": true,
        "event_id": event.id,
        "payload": event.payload,
      }))
      .unwrap_or_else(|_| String::from(r#"{"ok":true}"#));
      DispatchResult::ActivityResult { event, tool_result }
    }
    Err(e) => {
      DispatchResult::ToolError(format!("activity '{name}' failed: {e}"))
    }
  }
}

// ── main loop ────────────────────────────────────────────────────────

pub async fn run_loop(
  client: &dyn LlmClient,
  registry: &TopicRegistry,
  item: &Item,
  model: &str,
  turn_budget: u32,
) -> Result<IntakeOutcome, LoopError> {
  let tools = build_tools(item, registry);
  let ctx = TopicCtx::stub();

  let system_prompt = format!(
    "You are an intake assistant for a human work queue.  Your job is \
     to triage one item: either handle it autonomously by invoking \
     activities and then calling auto_resolved when done, or defer to \
     a human by calling defer_to_human with a reason explaining the \
     uncertainty.  Call exactly one halt tool when you are finished. \
     You have at most {turn_budget} turns; be concise."
  );

  let user_content = format!(
    "Item to triage.\nSource instance: {}\nTitle: {}\nBody:\n{}",
    item.source_instance_id.as_deref().unwrap_or("<none>"),
    item.title,
    item.body.as_deref().unwrap_or("(no body)")
  );

  let mut transcript =
    vec![Message::system(system_prompt), Message::user(user_content)];
  let mut activity_events: Vec<Event> = Vec::new();

  for turn in 0..turn_budget {
    let req = CompletionRequest {
      model: model.to_string(),
      messages: transcript.clone(),
      temperature: Some(0.1),
      tools: Some(tools.clone()),
    };
    let resp = client.complete(req).await?;

    let Some(choice) = resp.choices.into_iter().next() else {
      // LlmClient guarantees non-empty choices, but be defensive.
      return Ok(IntakeOutcome::DeferredToHuman {
        reason: format!("intake LLM returned no choices on turn {turn}"),
        transcript,
        activity_events,
      });
    };

    let tool_calls = choice.message.tool_calls.unwrap_or_default();

    // Echo the assistant's message into the transcript so the next
    // turn's prompt includes what it just said and did.
    transcript.push(Message::assistant_tool_calls(
      choice.message.content,
      tool_calls.clone(),
    ));

    if tool_calls.is_empty() {
      // The LLM produced free text without any tool call — it's not
      // engaging with the loop.  Treat as defer with a clear reason
      // so a human can read the transcript and see what happened.
      return Ok(IntakeOutcome::DeferredToHuman {
        reason: format!(
          "intake LLM produced no tool calls on turn {turn} \
           (no halt fired; see transcript)"
        ),
        transcript,
        activity_events,
      });
    }

    // Pre-check: a single response containing multiple halt calls
    // is a protocol violation — there's no sensible way to pick a
    // winner.  Reject the whole batch and feed the error back so
    // the LLM can re-emit a well-formed response on the next turn.
    let halt_count = tool_calls
      .iter()
      .filter(|c| is_reserved(&c.function.name))
      .count();
    if halt_count > 1 {
      let error = format!(
        "Invalid response: emitted {halt_count} halt calls in one \
         response.  At most one of '{HALT_AUTO_RESOLVED}' or \
         '{HALT_DEFER_TO_HUMAN}' is permitted per response.  Re-emit \
         with at most one halt; the entire batch was dropped."
      );
      warn!(
        halt_count,
        "LLM emitted multiple halt calls in one response; rejecting batch"
      );
      let error_result = serde_json::to_string(&json!({
        "ok": false,
        "error": error,
      }))
      .unwrap_or_else(|_| String::from(r#"{"ok":false}"#));
      for call in &tool_calls {
        transcript.push(Message::tool_result(&call.id, error_result.clone()));
      }
      continue;
    }

    // Dispatch all calls in parallel.  Multiple calls in one
    // response means "do these concurrently" per the OpenAI
    // tool-calling convention — if the model wants serial behavior
    // it can emit one call per turn and observe the result before
    // emitting the next.  Within a turn, tool results don't feed
    // back to the model until the next turn anyway, so serial-
    // within-a-turn gains nothing.
    //
    // Pre-borrow `ctx` so each closure iteration captures a copy
    // of the `&TopicCtx` reference (Copy) rather than trying to
    // move the underlying `TopicCtx`.
    let ctx_ref = &ctx;
    let results = join_all(tool_calls.iter().map(|call| {
      let call_id = call.id.clone();
      async move {
        let result = dispatch_tool_call(call, item, registry, ctx_ref).await;
        (call_id, result)
      }
    }))
    .await;

    // Bookkeeping in LLM-emission order (join_all preserves input
    // order in its results).  By the pre-check invariant there is
    // at most one Halt in this batch.
    let mut halt: Option<HaltReason> = None;
    for (call_id, result) in results {
      let content = match result {
        DispatchResult::Halt(reason) => {
          halt = Some(reason);
          json!({ "halted": true }).to_string()
        }
        DispatchResult::ActivityResult { event, tool_result } => {
          activity_events.push(event);
          tool_result
        }
        DispatchResult::ToolError(reason) => serde_json::to_string(&json!({
          "ok": false,
          "error": reason,
        }))
        .unwrap_or_else(|_| String::from(r#"{"ok":false}"#)),
      };
      transcript.push(Message::tool_result(call_id, content));
    }

    if let Some(reason) = halt {
      return Ok(match reason {
        HaltReason::AutoResolved { summary } => IntakeOutcome::AutoResolved {
          summary,
          transcript,
          activity_events,
        },
        HaltReason::DeferredToHuman { reason } => {
          IntakeOutcome::DeferredToHuman {
            reason,
            transcript,
            activity_events,
          }
        }
      });
    }
  }

  Ok(IntakeOutcome::DeferredToHuman {
    reason: format!("intake exceeded turn budget of {turn_budget}"),
    transcript,
    activity_events,
  })
}
