//! [`SubprocessTopic`] — a [`Topic`] implementation that delegates to
//! an out-of-process subprocess over JSON-RPC.

use crate::error::HostError;
use crate::sink::TopicDataSink;
use async_trait::async_trait;
use hyuqueue_core::activity::{Activity, ActivityInvocation};
use hyuqueue_core::event::Event;
use hyuqueue_core::topic::{IngestItem, Topic, TopicCtx, TopicError};
use hyuqueue_topic_proto::envelope::{Notification, Request, Response};
use hyuqueue_topic_proto::error::{TopicErrorData, RPC_ERROR_CODE_TOPIC};
use hyuqueue_topic_proto::method;
use hyuqueue_topic_proto::payload::{
  ExecuteRequest, ExecuteResponse, IngestRequest, IngestResponse, InitRequest,
  InitResponse, TopicDataSetParams,
};
use hyuqueue_topic_proto::version::JsonRpcVersion;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{
  AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>;

pub struct SubprocessTopic {
  id: String,
  display_name: String,
  item_activities: Vec<Activity>,
  global_activities: Vec<Activity>,

  outbound_tx: mpsc::UnboundedSender<String>,
  pending: PendingMap,
  next_id: AtomicU64,

  /// Dropped on `Drop` to signal the reader task to stop.
  shutdown_tx: Option<oneshot::Sender<()>>,
  /// Optional child handle from `spawn`; killed on `Drop`.
  child: Option<tokio::process::Child>,

  /// Held to keep the tasks tied to this struct's lifetime.  The
  /// tasks exit on their own once `shutdown_tx` is dropped (reader)
  /// and `outbound_tx` is dropped (writer); we don't need to await
  /// them, just keep the handles around so they aren't garbage
  /// collected by aborting.
  _reader_task: JoinHandle<()>,
  _writer_task: JoinHandle<()>,
}

impl SubprocessTopic {
  /// Construct from arbitrary IO halves.  Used by tests with
  /// `tokio::io::duplex`-backed pipes; mirrors the SDK's
  /// `run_with_io` shape.
  ///
  /// `initial_data` is the persisted `topic_data` snapshot the host
  /// hands the topic on init so it can hydrate its in-memory state.
  /// Empty when there is no prior persisted state for this topic.
  pub async fn from_io<R, W>(
    expected_id: &str,
    reader: R,
    writer: W,
    sink: Arc<dyn TopicDataSink>,
    initial_data: HashMap<String, serde_json::Value>,
  ) -> Result<Self, HostError>
  where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
  {
    Self::connect(expected_id, reader, writer, sink, None, initial_data).await
  }

  /// Spawn a binary and wire up its stdin/stdout to a fresh
  /// `SubprocessTopic`.  Stderr is inherited so subprocess logging
  /// passes through to server stderr.
  ///
  /// `initial_data` is the persisted `topic_data` snapshot — see
  /// [`Self::from_io`].
  pub async fn spawn(
    expected_id: &str,
    command: &[String],
    sink: Arc<dyn TopicDataSink>,
    initial_data: HashMap<String, serde_json::Value>,
  ) -> Result<Self, HostError> {
    use std::process::Stdio;
    use tokio::process::Command;

    let (program, args) = command.split_first().ok_or_else(|| {
      HostError::Spawn(std::io::Error::other("empty command argv"))
    })?;
    let mut child = Command::new(program)
      .args(args)
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::inherit())
      .spawn()
      .map_err(HostError::Spawn)?;
    let stdout = child.stdout.take().ok_or(HostError::MissingPipes)?;
    let stdin = child.stdin.take().ok_or(HostError::MissingPipes)?;
    Self::connect(expected_id, stdout, stdin, sink, Some(child), initial_data)
      .await
  }

  async fn connect<R, W>(
    expected_id: &str,
    reader: R,
    writer: W,
    sink: Arc<dyn TopicDataSink>,
    child: Option<tokio::process::Child>,
    initial_data: HashMap<String, serde_json::Value>,
  ) -> Result<Self, HostError>
  where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
  {
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<String>();
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let writer_task = spawn_writer(writer, outbound_rx);
    let reader_task = spawn_reader(
      reader,
      pending.clone(),
      sink,
      expected_id.to_string(),
      shutdown_rx,
    );

    let mut topic = SubprocessTopic {
      id: String::new(),
      display_name: String::new(),
      item_activities: vec![],
      global_activities: vec![],
      outbound_tx,
      pending,
      next_id: AtomicU64::new(0),
      shutdown_tx: Some(shutdown_tx),
      child,
      _reader_task: reader_task,
      _writer_task: writer_task,
    };

    let init = topic.handshake(expected_id, initial_data).await?;
    topic.id = init.id;
    topic.display_name = init.display_name;
    topic.item_activities = init.item_activities;
    topic.global_activities = init.global_activities;
    Ok(topic)
  }

  async fn handshake(
    &self,
    expected_id: &str,
    initial_data: HashMap<String, serde_json::Value>,
  ) -> Result<InitResponse, HostError> {
    let response = self
      .send_request(
        method::INIT,
        InitRequest {
          topic_data: initial_data,
        },
      )
      .await
      .map_err(|e| HostError::InitFailed(e.to_string()))?;
    let init: InitResponse = match response {
      Response::Success(s) => serde_json::from_value(s.result)?,
      Response::Error(e) => return Err(HostError::InitFailed(e.error.message)),
    };
    if init.id != expected_id {
      return Err(HostError::IdMismatch {
        expected: expected_id.to_string(),
        actual: init.id,
      });
    }
    Ok(init)
  }

  async fn send_request<P: Serialize>(
    &self,
    method: &str,
    params: P,
  ) -> Result<Response, RequestError> {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let params_value = serde_json::to_value(params)?;
    let request = Request {
      jsonrpc: JsonRpcVersion,
      method: method.to_string(),
      params: params_value,
      id,
    };
    let line = serde_json::to_string(&request)?;
    let (response_tx, response_rx) = oneshot::channel();
    {
      // The lock is held only for the insert — no `.await` inside.
      let mut pending = self
        .pending
        .lock()
        .map_err(|_| RequestError::PendingMapPoisoned)?;
      pending.insert(id, response_tx);
    }
    if self.outbound_tx.send(line).is_err() {
      // Writer task has exited.  Remove the dangling pending entry
      // so its slot doesn't leak until the topic is dropped.
      if let Ok(mut pending) = self.pending.lock() {
        pending.remove(&id);
      }
      return Err(RequestError::ConnectionClosed);
    }
    response_rx
      .await
      .map_err(|_| RequestError::ConnectionClosed)
  }
}

impl Drop for SubprocessTopic {
  fn drop(&mut self) {
    // Signal the reader task to stop.  Dropping the sender causes
    // the receiver's await to error, which the reader's
    // `tokio::select!` handles as the shutdown branch.
    self.shutdown_tx.take();
    // Kill the child if any.  start_kill is non-blocking; the actual
    // wait happens implicitly when the process exits.  Closes the
    // child's stdout from our view, which the reader task also
    // observes as EOF if it's still running.
    if let Some(mut child) = self.child.take() {
      if let Err(e) = child.start_kill() {
        tracing::debug!(
          "topic-host: child kill failed (likely already exited): {e}"
        );
      }
    }
    // Dropping `self.outbound_tx` happens automatically here,
    // signaling the writer task to exit after draining.
  }
}

#[async_trait]
impl Topic for SubprocessTopic {
  fn id(&self) -> &str {
    &self.id
  }

  fn display_name(&self) -> &str {
    &self.display_name
  }

  fn item_activities(&self) -> Vec<Activity> {
    self.item_activities.clone()
  }

  fn global_activities(&self) -> Vec<Activity> {
    self.global_activities.clone()
  }

  async fn ingest(
    &self,
    _ctx: &TopicCtx,
    config: &serde_json::Value,
  ) -> Result<Vec<IngestItem>, TopicError> {
    let response = self
      .send_request(
        method::INGEST,
        IngestRequest {
          config: config.clone(),
        },
      )
      .await
      .map_err(|e| request_error_to_topic_error("ingest", e))?;
    response_to_payload::<IngestResponse>(response, "ingest").map(|r| r.items)
  }

  async fn execute(
    &self,
    _ctx: &TopicCtx,
    invocation: &ActivityInvocation,
    item_id: Uuid,
  ) -> Result<Event, TopicError> {
    let response = self
      .send_request(
        method::EXECUTE,
        ExecuteRequest {
          invocation: invocation.clone(),
          item_id,
        },
      )
      .await
      .map_err(|e| request_error_to_topic_error("execute", e))?;
    response_to_payload::<ExecuteResponse>(response, "execute").map(|r| r.event)
  }
}

// ── tasks ────────────────────────────────────────────────────────────

fn spawn_writer<W>(
  writer: W,
  mut outbound_rx: mpsc::UnboundedReceiver<String>,
) -> JoinHandle<()>
where
  W: AsyncWrite + Unpin + Send + 'static,
{
  tokio::spawn(async move {
    let mut writer = writer;
    while let Some(line) = outbound_rx.recv().await {
      if let Err(e) = writer.write_all(line.as_bytes()).await {
        tracing::warn!("topic-host: writer error: {e}");
        break;
      }
      if let Err(e) = writer.write_all(b"\n").await {
        tracing::warn!("topic-host: writer error: {e}");
        break;
      }
      if let Err(e) = writer.flush().await {
        tracing::warn!("topic-host: writer flush error: {e}");
        break;
      }
    }
  })
}

fn spawn_reader<R>(
  reader: R,
  pending: PendingMap,
  sink: Arc<dyn TopicDataSink>,
  topic_id: String,
  shutdown_rx: oneshot::Receiver<()>,
) -> JoinHandle<()>
where
  R: AsyncRead + Unpin + Send + 'static,
{
  tokio::spawn(async move {
    let reader = BufReader::new(reader);
    let mut lines = reader.lines();
    let mut shutdown_rx = shutdown_rx;
    loop {
      tokio::select! {
        // Shutdown signal: SubprocessTopic was dropped.
        _ = &mut shutdown_rx => break,
        // Incoming line from the topic subprocess.
        result = lines.next_line() => match result {
          Ok(Some(line)) => {
            handle_line(&pending, &*sink, &topic_id, &line).await;
          }
          Ok(None) => break, // EOF — subprocess closed stdout.
          Err(e) => {
            tracing::warn!("topic-host: reader error: {e}");
            break;
          }
        }
      }
    }
    // Drain pending senders so any awaiters see ConnectionClosed
    // promptly rather than hanging until the SubprocessTopic itself
    // is dropped.
    if let Ok(mut pending) = pending.lock() {
      pending.clear();
    }
  })
}

async fn handle_line(
  pending: &PendingMap,
  sink: &dyn TopicDataSink,
  topic_id: &str,
  line: &str,
) {
  let value: serde_json::Value = match serde_json::from_str(line) {
    Ok(v) => v,
    Err(e) => {
      tracing::warn!(
        "topic-host: failed to parse line from subprocess: {e}; line={line:?}"
      );
      return;
    }
  };
  // Responses carry an `id`; notifications do not.  Branch on shape.
  if value.get("id").is_some() {
    handle_response(pending, value).await;
  } else {
    handle_notification(sink, topic_id, value).await;
  }
}

async fn handle_response(pending: &PendingMap, value: serde_json::Value) {
  let response: Response = match serde_json::from_value(value) {
    Ok(r) => r,
    Err(e) => {
      tracing::warn!("topic-host: failed to parse response: {e}");
      return;
    }
  };
  let id = match &response {
    Response::Success(s) => s.id,
    Response::Error(e) => e.id,
  };
  let sender = match pending.lock() {
    Ok(mut map) => map.remove(&id),
    Err(_) => {
      tracing::warn!("topic-host: pending map poisoned; dropping response");
      return;
    }
  };
  match sender {
    Some(sender) => {
      if sender.send(response).is_err() {
        tracing::debug!(
          "topic-host: response receiver for id {id} was dropped"
        );
      }
    }
    None => {
      tracing::warn!("topic-host: response for unknown request id {id}");
    }
  }
}

async fn handle_notification(
  sink: &dyn TopicDataSink,
  topic_id: &str,
  value: serde_json::Value,
) {
  let notification: Notification = match serde_json::from_value(value) {
    Ok(n) => n,
    Err(e) => {
      tracing::warn!("topic-host: failed to parse notification: {e}");
      return;
    }
  };
  match notification.method.as_str() {
    method::TOPIC_DATA_SET => {
      let params: TopicDataSetParams =
        match serde_json::from_value(notification.params) {
          Ok(p) => p,
          Err(e) => {
            tracing::warn!("topic-host: bad topic_data_set params: {e}");
            return;
          }
        };
      sink.set_data(topic_id, &params.key, params.value).await;
    }
    other => {
      tracing::warn!("topic-host: unknown notification method: {other}");
    }
  }
}

// ── error mapping ────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
enum RequestError {
  #[error("Failed to serialize request params: {0}")]
  Serialize(#[from] serde_json::Error),
  #[error("Subprocess connection closed before response")]
  ConnectionClosed,
  #[error("Pending request map was poisoned")]
  PendingMapPoisoned,
}

fn request_error_to_topic_error(
  activity: &str,
  err: RequestError,
) -> TopicError {
  TopicError::Execution {
    activity: activity.to_string(),
    reason: err.to_string(),
  }
}

fn response_to_payload<T>(
  response: Response,
  activity: &str,
) -> Result<T, TopicError>
where
  T: serde::de::DeserializeOwned,
{
  match response {
    Response::Success(s) => {
      serde_json::from_value(s.result).map_err(|e| TopicError::Execution {
        activity: activity.to_string(),
        reason: format!("failed to parse response payload: {e}"),
      })
    }
    Response::Error(e) => Err(rpc_error_to_topic_error(activity, e.error)),
  }
}

fn rpc_error_to_topic_error(
  activity: &str,
  err: hyuqueue_topic_proto::envelope::RpcError,
) -> TopicError {
  // Topic errors carry a structured `data` field with TopicErrorData.
  // Other RPC errors (method-not-found, invalid-params, etc.) are
  // collapsed into Execution since the trait surface only carries the
  // three TopicError variants.
  if err.code == RPC_ERROR_CODE_TOPIC {
    if let Some(data) = err.data {
      match serde_json::from_value::<TopicErrorData>(data) {
        Ok(typed) => return topic_error_data_to_topic_error(typed),
        Err(parse_err) => {
          tracing::warn!(
            "topic-host: failed to parse TopicErrorData: {parse_err}"
          );
          // Fall through to generic Execution below.
        }
      }
    }
  }
  TopicError::Execution {
    activity: activity.to_string(),
    reason: err.message,
  }
}

fn topic_error_data_to_topic_error(data: TopicErrorData) -> TopicError {
  match data {
    TopicErrorData::UnsupportedActivity {
      activity_id,
      topic_id,
    } => TopicError::UnsupportedActivity(activity_id, topic_id),
    TopicErrorData::Execution { activity, reason } => {
      TopicError::Execution { activity, reason }
    }
    TopicErrorData::Configuration { detail } => {
      TopicError::Configuration(detail)
    }
  }
}
