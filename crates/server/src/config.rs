use clap::Parser;
use hyuqueue_lib::{LogFormat, LogLevel};
use serde::Deserialize;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
  #[error(
    "Failed to read configuration file at {path:?} during startup: {source}"
  )]
  FileRead {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },

  #[error("Failed to parse configuration file at {path:?}: {source}")]
  Parse {
    path: PathBuf,
    #[source]
    source: toml::de::Error,
  },

  #[error("Configuration validation failed: {0}")]
  Validation(String),
}

#[derive(Debug, Parser)]
#[command(
  name = "hyuqueue-server",
  about = "hyuqueue daemon — owns the queue, storage, and LLM workers"
)]
pub struct CliRaw {
  #[arg(long, env = "LOG_LEVEL")]
  pub log_level: Option<String>,

  #[arg(long, env = "LOG_FORMAT")]
  pub log_format: Option<String>,

  #[arg(short, long, env = "CONFIG_FILE")]
  pub config: Option<PathBuf>,

  /// Host to bind the HTTP API on.
  #[arg(long, env = "HOST")]
  pub host: Option<String>,

  /// Port to bind the HTTP API on.
  #[arg(long, env = "PORT")]
  pub port: Option<u16>,

  /// Path to the SQLite database file.
  #[arg(long, env = "DB_PATH")]
  pub db_path: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ConfigFileRaw {
  pub log_level: Option<String>,
  pub log_format: Option<String>,
  pub host: Option<String>,
  pub port: Option<u16>,
  pub db_path: Option<String>,
  pub llm: Option<LlmConfigRaw>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LlmConfigRaw {
  pub base_url: Option<String>,
  pub intake_model: Option<String>,
  pub review_model: Option<String>,
  pub api_key: Option<String>,
}

impl ConfigFileRaw {
  pub fn from_file(path: &PathBuf) -> Result<Self, ConfigError> {
    let contents =
      std::fs::read_to_string(path).map_err(|source| ConfigError::FileRead {
        path: path.clone(),
        source,
      })?;
    toml::from_str(&contents).map_err(|source| ConfigError::Parse {
      path: path.clone(),
      source,
    })
  }
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
  pub base_url: String,
  pub intake_model: String,
  pub review_model: String,
  pub api_key: Option<String>,
}

#[derive(Debug)]
pub struct Config {
  pub log_level: LogLevel,
  pub log_format: LogFormat,
  pub host: String,
  pub port: u16,
  pub db_path: String,
  pub llm: LlmConfig,
}

impl Config {
  pub fn bind_address(&self) -> String {
    format!("{}:{}", self.host, self.port)
  }

  pub fn from_cli_and_file(cli: CliRaw) -> Result<Self, ConfigError> {
    let file = if let Some(path) = &cli.config {
      ConfigFileRaw::from_file(path)?
    } else {
      let default = PathBuf::from("config.toml");
      if default.exists() {
        ConfigFileRaw::from_file(&default)?
      } else {
        ConfigFileRaw::default()
      }
    };

    let log_level = cli
      .log_level
      .or(file.log_level)
      .unwrap_or_else(|| "info".to_string())
      .parse::<LogLevel>()
      .map_err(|e| ConfigError::Validation(e.to_string()))?;

    let log_format = cli
      .log_format
      .or(file.log_format)
      .unwrap_or_else(|| "text".to_string())
      .parse::<LogFormat>()
      .map_err(|e| ConfigError::Validation(e.to_string()))?;

    let llm_raw = file.llm.unwrap_or_default();
    let llm = LlmConfig {
      base_url: llm_raw
        .base_url
        .unwrap_or_else(|| "http://localhost:11434/v1".to_string()),
      intake_model: llm_raw
        .intake_model
        .unwrap_or_else(|| "llama3.2".to_string()),
      review_model: llm_raw
        .review_model
        .unwrap_or_else(|| "llama3.2".to_string()),
      api_key: llm_raw.api_key,
    };

    Ok(Config {
      log_level,
      log_format,
      host: cli
        .host
        .or(file.host)
        .unwrap_or_else(|| "127.0.0.1".to_string()),
      port: cli.port.or(file.port).unwrap_or(8731),
      db_path: cli
        .db_path
        .or(file.db_path)
        .unwrap_or_else(|| "hyuqueue.db".to_string()),
      llm,
    })
  }
}
