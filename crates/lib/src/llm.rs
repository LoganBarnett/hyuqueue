//! LLM client abstraction — OpenAI-compatible REST API.
//!
//! Ollama speaks this natively. Claude and others adapt to it via proxy.
//! The codebase never knows which model is behind the endpoint.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Request types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct CompletionRequest {
  pub model: String,
  pub messages: Vec<Message>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub temperature: Option<f32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub tools: Option<Vec<Tool>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
  pub role: Role,
  /// Text content.  May be `None` for assistant messages whose
  /// payload is in `tool_calls` (the OpenAI spec allows
  /// `"content": null` in that case).
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub content: Option<String>,
  /// Tool calls the assistant invoked in this message.  Echoed back
  /// into the next turn's `messages` array so the model knows what
  /// it just did.  Always `None` on user / system / tool messages.
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub tool_calls: Option<Vec<ToolCall>>,
  /// On a tool-result message (`role = Tool`), identifies which
  /// `tool_call.id` this result corresponds to.
  #[serde(skip_serializing_if = "Option::is_none", default)]
  pub tool_call_id: Option<String>,
}

impl Message {
  pub fn system(content: impl Into<String>) -> Self {
    Self {
      role: Role::System,
      content: Some(content.into()),
      tool_calls: None,
      tool_call_id: None,
    }
  }

  pub fn user(content: impl Into<String>) -> Self {
    Self {
      role: Role::User,
      content: Some(content.into()),
      tool_calls: None,
      tool_call_id: None,
    }
  }

  /// Echo an assistant message that included tool calls.  Pass
  /// `content = None` if the model returned no text alongside the
  /// calls (common for tool-only turns).
  pub fn assistant_tool_calls(
    content: Option<String>,
    tool_calls: Vec<ToolCall>,
  ) -> Self {
    Self {
      role: Role::Assistant,
      content,
      tool_calls: Some(tool_calls),
      tool_call_id: None,
    }
  }

  /// Send a tool result back to the model.  `content` is the
  /// stringified result (typically JSON).
  pub fn tool_result(
    tool_call_id: impl Into<String>,
    content: impl Into<String>,
  ) -> Self {
    Self {
      role: Role::Tool,
      content: Some(content.into()),
      tool_calls: None,
      tool_call_id: Some(tool_call_id.into()),
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
  System,
  User,
  Assistant,
  Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
  #[serde(rename = "type")]
  pub tool_type: String, // always "function"
  pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
  pub name: String,
  pub description: String,
  pub parameters: serde_json::Value, // JSON Schema object
}

// ── Response types ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CompletionResponse {
  pub choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
  pub message: ResponseMessage,
  pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseMessage {
  pub role: Role,
  pub content: Option<String>,
  pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
  pub id: String,
  /// Always `"function"` per the OpenAI spec.  Serialized so we can
  /// echo tool calls back into the next turn's messages.
  #[serde(rename = "type", default = "default_tool_type")]
  pub tool_type: String,
  pub function: ToolCallFunction,
}

fn default_tool_type() -> String {
  "function".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
  pub name: String,
  pub arguments: String, // JSON string
}

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum LlmError {
  #[error("HTTP request to LLM failed: {0}")]
  Http(#[from] reqwest::Error),

  #[error("LLM returned an unexpected response: {0}")]
  UnexpectedResponse(String),

  #[error("LLM response contained no choices")]
  EmptyResponse,
}

// ── Trait ────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait LlmClient: Send + Sync {
  async fn complete(
    &self,
    req: CompletionRequest,
  ) -> Result<CompletionResponse, LlmError>;
}

// ── OpenAI-compatible implementation ─────────────────────────────────────────

pub struct OpenAiClient {
  http: reqwest::Client,
  base_url: String,
  api_key: Option<String>,
}

impl OpenAiClient {
  pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
    Self {
      http: reqwest::Client::new(),
      base_url: base_url.into(),
      api_key,
    }
  }
}

#[async_trait]
impl LlmClient for OpenAiClient {
  async fn complete(
    &self,
    req: CompletionRequest,
  ) -> Result<CompletionResponse, LlmError> {
    let url =
      format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

    let mut builder = self.http.post(&url).json(&req);

    if let Some(key) = &self.api_key {
      builder = builder.bearer_auth(key);
    }

    let resp = builder.send().await?.error_for_status()?;

    let completion: CompletionResponse = resp.json().await?;

    if completion.choices.is_empty() {
      return Err(LlmError::EmptyResponse);
    }

    Ok(completion)
  }
}
