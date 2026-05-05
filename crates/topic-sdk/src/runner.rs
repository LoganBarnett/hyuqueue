//! The JSON-RPC dispatch loop that turns a [`Topic`] impl into a
//! running subprocess.
//!
//! [`run`] is the entry point most topic binaries call from `main`;
//! [`run_with_io`] takes explicit reader/writer arguments and is what
//! the SDK's own tests use with in-memory IO.

use crate::error::SdkError;
use crate::notifier::ChannelNotifier;
use hyuqueue_core::topic::{Topic, TopicCtx, TopicError};
use hyuqueue_topic_proto::envelope::{
  ErrorResponse, Request, Response, RpcError, SuccessResponse,
};
use hyuqueue_topic_proto::error::topic_error_to_rpc_error;
use hyuqueue_topic_proto::method;
use hyuqueue_topic_proto::payload::{
  ExecuteRequest, ExecuteResponse, IngestRequest, IngestResponse, InitResponse,
};
use hyuqueue_topic_proto::version::JsonRpcVersion;
use serde::Serialize;
use std::sync::Arc;
use tokio::io::{
  AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::mpsc;

/// JSON-RPC error codes used by the SDK loop itself.  Topic-originated
/// errors use [`hyuqueue_topic_proto::error::RPC_ERROR_CODE_TOPIC`]
/// (-32000); these are dispatcher-level conditions defined by the
/// JSON-RPC 2.0 spec.
const RPC_ERROR_CODE_METHOD_NOT_FOUND: i32 = -32601;
const RPC_ERROR_CODE_INVALID_PARAMS: i32 = -32602;

/// Run the JSON-RPC loop on stdin/stdout.  Returns when stdin closes
/// or the host sends a `shutdown` request.
pub async fn run<T: Topic + 'static>(topic: T) -> Result<(), SdkError> {
  run_with_io(topic, tokio::io::stdin(), tokio::io::stdout()).await
}

/// Run the loop against arbitrary reader/writer.  Used by tests with
/// `tokio::io::duplex`-backed pipes.
pub async fn run_with_io<R, W, T>(
  topic: T,
  reader: R,
  writer: W,
) -> Result<(), SdkError>
where
  R: AsyncRead + Unpin + Send,
  W: AsyncWrite + Unpin + Send + 'static,
  T: Topic,
{
  // Both response writes and `topic_data_set` notifications need to
  // reach the writer.  A single writer task draining an mpsc channel
  // serializes them naturally without explicit locking, and gives
  // notifications FIFO ordering relative to responses.
  let (tx, mut rx) = mpsc::unbounded_channel::<String>();

  let writer_handle = tokio::spawn(async move {
    let mut writer = writer;
    while let Some(line) = rx.recv().await {
      writer
        .write_all(line.as_bytes())
        .await
        .map_err(SdkError::StdoutWrite)?;
      writer
        .write_all(b"\n")
        .await
        .map_err(SdkError::StdoutWrite)?;
      writer.flush().await.map_err(SdkError::StdoutWrite)?;
    }
    Ok::<(), SdkError>(())
  });

  let ctx = TopicCtx::new(Arc::new(ChannelNotifier::new(tx.clone())));

  let reader = BufReader::new(reader);
  let mut lines = reader.lines();

  loop {
    let line = match lines.next_line().await {
      Ok(Some(line)) => line,
      Ok(None) => break,
      Err(e) => return Err(SdkError::StdinRead(e)),
    };

    let request: Request = match serde_json::from_str(&line) {
      Ok(req) => req,
      Err(e) => {
        // Malformed input means the host and topic disagree on the
        // wire format — there's no addressable id to respond to and
        // no useful action to take.  Log and continue; the host can
        // see the line was ignored by waiting on its own request id.
        tracing::warn!("Failed to parse request line: {e}");
        continue;
      }
    };

    let is_shutdown = request.method == method::SHUTDOWN;
    let response = dispatch(&topic, &ctx, request).await;

    match serde_json::to_string(&response) {
      Ok(line) => {
        if tx.send(line).is_err() {
          // Writer task has exited (likely due to a write error).
          // No point continuing.
          break;
        }
      }
      Err(e) => {
        // Response serialization is essentially unreachable for our
        // typed payloads, but we don't assert it — log and skip the
        // response.  The host will time out waiting on this id; the
        // loop stays alive for other requests.
        tracing::error!("Failed to serialize response: {e}");
      }
    }

    if is_shutdown {
      break;
    }
  }

  // Drop the channel senders so the writer task drains and exits.
  drop(ctx);
  drop(tx);

  match writer_handle.await {
    Ok(Ok(())) => Ok(()),
    Ok(Err(e)) => Err(e),
    Err(e) => Err(SdkError::WriterTaskJoin(e)),
  }
}

async fn dispatch<T: Topic>(
  topic: &T,
  ctx: &TopicCtx,
  request: Request,
) -> Response {
  let id = request.id;
  match request.method.as_str() {
    method::INIT => handle_init(topic, id),
    method::INGEST => handle_ingest(topic, ctx, request, id).await,
    method::EXECUTE => handle_execute(topic, ctx, request, id).await,
    method::SHUTDOWN => handle_shutdown(id),
    other => method_not_found_response(id, other),
  }
}

fn handle_init<T: Topic>(topic: &T, id: u64) -> Response {
  // `supports_ingest` is reported `true` even when the topic relies on
  // the trait's default impl (which returns an empty vec) — calling it
  // is always safe, and the host has no way to learn otherwise from
  // the trait surface.  `supports_execute` is `true` because the trait
  // makes `execute` required.
  let payload = InitResponse {
    id: topic.id().to_string(),
    display_name: topic.display_name().to_string(),
    supports_ingest: true,
    supports_execute: true,
    item_activities: topic.item_activities(),
    global_activities: topic.global_activities(),
  };
  success_response(id, payload)
}

async fn handle_ingest<T: Topic>(
  topic: &T,
  ctx: &TopicCtx,
  request: Request,
  id: u64,
) -> Response {
  match serde_json::from_value::<IngestRequest>(request.params) {
    Err(e) => invalid_params_response(id, e),
    Ok(req) => topic
      .ingest(ctx, &req.config)
      .await
      .map(|items| success_response(id, IngestResponse { items }))
      .unwrap_or_else(|e| topic_error_response(id, &e)),
  }
}

async fn handle_execute<T: Topic>(
  topic: &T,
  ctx: &TopicCtx,
  request: Request,
  id: u64,
) -> Response {
  match serde_json::from_value::<ExecuteRequest>(request.params) {
    Err(e) => invalid_params_response(id, e),
    Ok(req) => topic
      .execute(ctx, &req.invocation, req.item_id)
      .await
      .map(|event| success_response(id, ExecuteResponse { event }))
      .unwrap_or_else(|e| topic_error_response(id, &e)),
  }
}

fn handle_shutdown(id: u64) -> Response {
  success_response(id, serde_json::Value::Null)
}

fn success_response<T: Serialize>(id: u64, payload: T) -> Response {
  // Payload serialization is essentially unreachable for our typed
  // response shapes; the fallback to `Null` keeps the loop honest
  // (no `expect` asserting an unprovable invariant) without breaking
  // dispatch on a malformed payload.
  let result = serde_json::to_value(payload).unwrap_or_else(|e| {
    tracing::error!("Failed to serialize response payload: {e}");
    serde_json::Value::Null
  });
  Response::Success(SuccessResponse {
    jsonrpc: JsonRpcVersion,
    result,
    id,
  })
}

fn invalid_params_response(id: u64, err: serde_json::Error) -> Response {
  Response::Error(ErrorResponse {
    jsonrpc: JsonRpcVersion,
    error: RpcError {
      code: RPC_ERROR_CODE_INVALID_PARAMS,
      message: format!("Invalid params: {err}"),
      data: None,
    },
    id,
  })
}

fn method_not_found_response(id: u64, method: &str) -> Response {
  Response::Error(ErrorResponse {
    jsonrpc: JsonRpcVersion,
    error: RpcError {
      code: RPC_ERROR_CODE_METHOD_NOT_FOUND,
      message: format!("Method not found: {method}"),
      data: None,
    },
    id,
  })
}

fn topic_error_response(id: u64, err: &TopicError) -> Response {
  // If structured `data` serialization fails, fall back to the
  // bare-message form rather than asserting infallibility.  The
  // host loses the typed `TopicErrorData` in that case but still
  // gets the human-readable message and error code.
  let rpc_error = topic_error_to_rpc_error(err).unwrap_or_else(|e| {
    tracing::error!("Failed to serialize TopicErrorData: {e}");
    RpcError {
      code: hyuqueue_topic_proto::error::RPC_ERROR_CODE_TOPIC,
      message: err.to_string(),
      data: None,
    }
  });
  Response::Error(ErrorResponse {
    jsonrpc: JsonRpcVersion,
    error: rpc_error,
    id,
  })
}
