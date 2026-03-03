use hyuqueue_lib::{LogFormat, LogLevel};
use serde::Deserialize;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
  #[error(
    "Failed to read config file at {path:?}: {source}"
  )]
  FileRead {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },

  #[error("Failed to parse config file at {path:?}: {source}")]
  Parse {
    path: PathBuf,
    #[source]
    source: toml::de::Error,
  },

  #[error("Configuration validation failed: {0}")]
  Validation(String),
}

#[derive(Debug, Deserialize, Default)]
pub struct ConfigFileRaw {
  pub log_level: Option<String>,
  pub log_format: Option<String>,
  pub server_url: Option<String>,
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

#[derive(Debug)]
pub struct Config {
  pub log_level: LogLevel,
  pub log_format: LogFormat,
  /// Base URL of the hyuqueue-server to talk to.
  pub server_url: String,
}

impl Config {
  pub fn from_file_or_default(config_path: Option<PathBuf>) -> Result<Self, ConfigError> {
    let file = if let Some(path) = config_path {
      ConfigFileRaw::from_file(&path)?
    } else {
      let default = PathBuf::from("config.toml");
      if default.exists() {
        ConfigFileRaw::from_file(&default)?
      } else {
        ConfigFileRaw::default()
      }
    };

    let log_level = file
      .log_level
      .unwrap_or_else(|| "info".to_string())
      .parse::<LogLevel>()
      .map_err(|e| ConfigError::Validation(e.to_string()))?;

    let log_format = file
      .log_format
      .unwrap_or_else(|| "text".to_string())
      .parse::<LogFormat>()
      .map_err(|e| ConfigError::Validation(e.to_string()))?;

    Ok(Config {
      log_level,
      log_format,
      server_url: file
        .server_url
        .unwrap_or_else(|| "http://127.0.0.1:8731".to_string()),
    })
  }
}
