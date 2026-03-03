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
  pub content: String,
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

#[derive(Debug, Deserialize)]
pub struct ToolCall {
  pub id: String,
  pub function: ToolCallFunction,
}

#[derive(Debug, Deserialize)]
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
    let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

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
