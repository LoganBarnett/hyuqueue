//! hyuqueue — CLI client.
//!
//! A thin HTTP client that speaks to hyuqueue-server and outputs JSON.
//! All domain logic lives in the server. This is just argument parsing
//! and HTTP calls.
//!
//! # LLM Development Guidelines
//! - Keep this thin. No domain logic here.
//! - stdout = JSON output (for piping). stderr = logs.
//! - All subcommands call the server and print the response.
//! - Add new subcommands by adding a variant to Commands and a handler fn.

mod config;
mod logging;

use clap::{Parser, Subcommand};
use config::{Config, ConfigError};
use logging::init_logging;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
enum ApplicationError {
  #[error("Failed to load configuration: {0}")]
  Config(#[from] ConfigError),

  #[error("HTTP request failed: {0}")]
  Http(#[from] reqwest::Error),
}

#[derive(Debug, Parser)]
#[command(
  name = "hyuqueue",
  about = "hyuqueue — human work queue",
  version
)]
struct Cli {
  #[arg(long, env = "LOG_LEVEL", global = true)]
  log_level: Option<String>,

  #[arg(long, env = "LOG_FORMAT", global = true)]
  log_format: Option<String>,

  #[arg(short, long, env = "CONFIG_FILE", global = true)]
  config: Option<PathBuf>,

  /// Override the server URL (default: http://127.0.0.1:8731).
  #[arg(long, env = "HYUQUEUE_SERVER", global = true)]
  server: Option<String>,

  #[command(subcommand)]
  command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
  /// List items in the human queue.
  List {
    #[arg(long)]
    queue_id: Option<Uuid>,
    #[arg(long)]
    state: Option<String>,
    #[arg(long, default_value = "50")]
    limit: i64,
  },

  /// Show a single item and its event log.
  Show { id: Uuid },

  /// Add a new item to the queue.
  Add {
    #[arg(long)]
    title: String,
    /// Origin system identifier (e.g. "email", "jira", "slack"). Required.
    #[arg(long)]
    source: String,
    #[arg(long)]
    queue_id: Uuid,
    #[arg(long)]
    body: Option<String>,
    /// JSON metadata blob.
    #[arg(long)]
    meta: Option<String>,
  },

  /// Invoke an activity on an item.
  Action {
    id: Uuid,
    activity_id: String,
    /// JSON params blob.
    #[arg(long)]
    params: Option<String>,
  },

  /// Ack an item — marks it done and (in iron mode) advances the queue.
  Ack { id: Uuid },

  /// Show the next item in the human queue (iron mode).
  Next,

  /// Show the count of items in the human queue.
  Count,

  /// Queue management.
  Queue {
    #[command(subcommand)]
    cmd: QueueCommands,
  },
}

#[derive(Debug, Subcommand)]
enum QueueCommands {
  /// List all queues.
  List,
  /// Create a new queue.
  Add {
    #[arg(long)]
    name: String,
    #[arg(long, value_delimiter = ',')]
    tags: Vec<String>,
  },
}

#[tokio::main]
async fn main() -> Result<(), ApplicationError> {
  let cli = Cli::parse();

  let mut config = Config::from_file_or_default(cli.config)?;

  // CLI --server overrides config file.
  if let Some(url) = cli.server {
    config.server_url = url;
  }
  if let Some(level) = cli.log_level {
    config.log_level = level
      .parse()
      .unwrap_or(hyuqueue_lib::LogLevel::Info);
  }
  if let Some(fmt) = cli.log_format {
    config.log_format = fmt
      .parse()
      .unwrap_or(hyuqueue_lib::LogFormat::Text);
  }

  init_logging(config.log_level, config.log_format);

  let http = reqwest::Client::new();
  let base = config.server_url.trim_end_matches('/').to_string();

  let result = match cli.command {
    Commands::List {
      queue_id,
      state,
      limit,
    } => {
      let mut url = format!("{base}/api/v1/items?limit={limit}");
      if let Some(qid) = queue_id {
        url.push_str(&format!("&queue_id={qid}"));
      }
      if let Some(s) = state {
        url.push_str(&format!("&state={s}"));
      }
      http.get(&url).send().await?.text().await?
    }

    Commands::Show { id } => {
      http
        .get(format!("{base}/api/v1/items/{id}"))
        .send()
        .await?
        .text()
        .await?
    }

    Commands::Add {
      title,
      source,
      queue_id,
      body,
      meta,
    } => {
      let metadata: serde_json::Value = meta
        .as_deref()
        .map(|s| serde_json::from_str(s).unwrap_or(serde_json::Value::Null))
        .unwrap_or(serde_json::Value::Object(Default::default()));

      let body_json = serde_json::json!({
        "title": title,
        "source": source,
        "queue_id": queue_id,
        "body": body,
        "metadata": metadata,
      });

      http
        .post(format!("{base}/api/v1/items"))
        .json(&body_json)
        .send()
        .await?
        .text()
        .await?
    }

    Commands::Action {
      id,
      activity_id,
      params,
    } => {
      let params_val: serde_json::Value = params
        .as_deref()
        .map(|s| serde_json::from_str(s).unwrap_or(serde_json::Value::Null))
        .unwrap_or(serde_json::Value::Object(Default::default()));

      let body_json =
        serde_json::json!({ "activity_id": activity_id, "params": params_val });

      http
        .post(format!("{base}/api/v1/items/{id}/action"))
        .json(&body_json)
        .send()
        .await?
        .text()
        .await?
    }

    Commands::Ack { id } => {
      http
        .post(format!("{base}/api/v1/items/{id}/ack"))
        .send()
        .await?
        .text()
        .await?
    }

    Commands::Next => {
      http
        .get(format!("{base}/api/v1/items/next"))
        .send()
        .await?
        .text()
        .await?
    }

    Commands::Count => {
      http
        .get(format!("{base}/api/v1/items/count"))
        .send()
        .await?
        .text()
        .await?
    }

    Commands::Queue { cmd } => match cmd {
      QueueCommands::List => {
        http
          .get(format!("{base}/api/v1/queues"))
          .send()
          .await?
          .text()
          .await?
      }
      QueueCommands::Add { name, tags } => {
        let body_json = serde_json::json!({ "name": name, "tags": tags });
        http
          .post(format!("{base}/api/v1/queues"))
          .json(&body_json)
          .send()
          .await?
          .text()
          .await?
      }
    },
  };

  // Output to stdout — ready for piping and JSON parsing.
  println!("{result}");
  Ok(())
}
