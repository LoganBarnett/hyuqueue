use clap::Parser;
use hyuqueue_lib::{LogFormat, LogLevel};
use serde::Deserialize;
use std::path::PathBuf;
use thiserror::Error;
use tokio_listener::ListenerAddress;
use tracing::warn;

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

  #[error("Invalid listen address '{address}': {reason}")]
  InvalidListenAddress {
    address: String,
    reason: &'static str,
  },

  #[error(
    "Failed to run secret command for topic '{topic}' key '{key}': \
     {reason}"
  )]
  SecretCommand {
    topic: String,
    key: String,
    reason: String,
  },
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

  /// Address to listen on: host:port for TCP, /path/to.sock for Unix socket,
  /// or sd-listen to inherit a socket from systemd.
  #[arg(long, env = "LISTEN")]
  pub listen: Option<String>,

  /// Path to the SQLite database file.
  #[arg(long, env = "DB_PATH")]
  pub db_path: Option<String>,

  /// Path to compiled frontend static assets.
  #[arg(long, env = "FRONTEND_PATH")]
  pub frontend_path: Option<PathBuf>,

  /// Base URL of the service (e.g. https://example.com), used to construct
  /// the OIDC redirect URI.
  #[arg(long, env = "BASE_URL")]
  pub base_url: Option<String>,

  /// OIDC issuer URL (e.g. https://sso.example.com/application/o/myapp).
  #[arg(long, env = "OIDC_ISSUER")]
  pub oidc_issuer: Option<String>,

  /// OIDC client ID.
  #[arg(long, env = "OIDC_CLIENT_ID")]
  pub oidc_client_id: Option<String>,

  /// Path to a file containing the OIDC client secret.
  #[arg(long, env = "OIDC_CLIENT_SECRET_FILE")]
  pub oidc_client_secret_file: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub struct TopicConfigRaw {
  pub id: String,
  pub queue: String,
  pub config: Option<toml::Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ConfigFileRaw {
  pub log_level: Option<String>,
  pub log_format: Option<String>,
  pub listen: Option<String>,
  pub db_path: Option<String>,
  pub frontend_path: Option<PathBuf>,
  pub base_url: Option<String>,
  pub oidc_issuer: Option<String>,
  pub oidc_client_id: Option<String>,
  pub oidc_client_secret_file: Option<PathBuf>,
  pub llm: Option<LlmConfigRaw>,
  pub topics: Option<Vec<TopicConfigRaw>>,
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
    let contents = std::fs::read_to_string(path).map_err(|source| {
      ConfigError::FileRead {
        path: path.clone(),
        source,
      }
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

#[derive(Debug, Clone)]
pub struct OidcConfig {
  pub issuer: String,
  pub client_id: String,
  pub client_secret: String,
}

#[derive(Debug, Clone)]
pub struct TopicConfig {
  pub id: String,
  pub queue_name: String,
  pub config: serde_json::Value,
}

#[derive(Debug)]
pub struct Config {
  pub log_level: LogLevel,
  pub log_format: LogFormat,
  pub listen_address: ListenerAddress,
  pub db_path: String,
  pub frontend_path: PathBuf,
  pub base_url: String,
  pub oidc: Option<OidcConfig>,
  pub llm: LlmConfig,
  pub topics: Vec<TopicConfig>,
}

impl Config {
  pub fn from_cli_and_file(cli: CliRaw) -> Result<Self, ConfigError> {
    let file = if let Some(path) = &cli.config {
      ConfigFileRaw::from_file(path)?
    } else {
      let cwd = PathBuf::from("config.toml");
      let xdg =
        xdg_dir("XDG_CONFIG_HOME", ".config").map(|d| d.join("config.toml"));

      if cwd.exists() {
        ConfigFileRaw::from_file(&cwd)?
      } else if let Some(ref xdg_path) = xdg.filter(|p| p.exists()) {
        ConfigFileRaw::from_file(xdg_path)?
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

    let listen_str = cli
      .listen
      .or(file.listen)
      .unwrap_or_else(|| "127.0.0.1:8731".to_string());

    let listen_address =
      listen_str.parse::<ListenerAddress>().map_err(|reason| {
        ConfigError::InvalidListenAddress {
          address: listen_str.clone(),
          reason,
        }
      })?;

    let frontend_path = cli
      .frontend_path
      .or(file.frontend_path)
      .unwrap_or_else(|| PathBuf::from("frontend/public"));

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

    let base_url = cli
      .base_url
      .or(file.base_url)
      .unwrap_or_else(|| "http://localhost:8731".to_string());

    let oidc_issuer = cli.oidc_issuer.or(file.oidc_issuer);
    let oidc_client_id = cli.oidc_client_id.or(file.oidc_client_id);
    let oidc_secret_file =
      cli.oidc_client_secret_file.or(file.oidc_client_secret_file);

    let oidc = match (&oidc_issuer, &oidc_client_id) {
      (None, None) if oidc_secret_file.is_none() => None,
      (Some(issuer), Some(client_id)) => {
        let secret_file = oidc_secret_file.ok_or_else(|| {
          ConfigError::Validation(
            "oidc_client_secret_file is required when oidc_issuer \
               and oidc_client_id are set"
              .to_string(),
          )
        })?;

        let client_secret = std::fs::read_to_string(&secret_file)
          .map(|s| s.trim().to_string())
          .map_err(|source| ConfigError::FileRead {
            path: secret_file,
            source,
          })?;

        Some(OidcConfig {
          issuer: issuer.clone(),
          client_id: client_id.clone(),
          client_secret,
        })
      }
      _ => {
        let mut present = Vec::new();
        let mut missing = Vec::new();
        for (name, val) in [
          ("oidc_issuer", oidc_issuer.is_some()),
          ("oidc_client_id", oidc_client_id.is_some()),
          ("oidc_client_secret_file", oidc_secret_file.is_some()),
        ] {
          if val {
            present.push(name);
          } else {
            missing.push(name);
          }
        }
        return Err(ConfigError::Validation(format!(
          "partial OIDC configuration: set all three fields or none. \
           present: [{}], missing: [{}]",
          present.join(", "),
          missing.join(", ")
        )));
      }
    };

    let topics = resolve_topics(file.topics.unwrap_or_default())?;

    Ok(Config {
      log_level,
      log_format,
      listen_address,
      db_path: cli.db_path.or(file.db_path).unwrap_or_else(default_db_path),
      frontend_path,
      base_url,
      oidc,
      llm,
      topics,
    })
  }
}

/// Resolve an XDG base directory: honour the environment variable if set,
/// otherwise fall back to `$HOME/<default_suffix>`.  Returns `None` when
/// no home directory can be determined (e.g. containers with no `HOME`).
fn xdg_dir(env_var: &str, default_suffix: &str) -> Option<PathBuf> {
  std::env::var_os(env_var)
    .map(PathBuf::from)
    .or_else(|| home::home_dir().map(|h| h.join(default_suffix)))
    .map(|d| d.join("hyuqueue"))
}

/// Default database path: `$XDG_DATA_HOME/hyuqueue/hyuqueue.db`.
/// Falls back to `hyuqueue.db` in the working directory if the home
/// directory cannot be determined.
fn default_db_path() -> String {
  xdg_dir("XDG_DATA_HOME", ".local/share")
    .map(|d| d.join("hyuqueue.db").to_string_lossy().into_owned())
    .unwrap_or_else(|| "hyuqueue.db".to_string())
}

/// Resolve `_cmd` suffixed keys in a TOML table: for every key ending in
/// `_cmd`, run the command and store stdout under the base key.
fn resolve_cmd_keys(
  topic_id: &str,
  table: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), ConfigError> {
  let cmd_keys: Vec<String> = table
    .keys()
    .filter(|k| k.ends_with("_cmd"))
    .cloned()
    .collect();

  for cmd_key in cmd_keys {
    let base_key = cmd_key.trim_end_matches("_cmd").to_string();
    let cmd = table
      .get(&cmd_key)
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();

    if cmd.is_empty() {
      continue;
    }

    let output = std::process::Command::new("sh")
      .args(["-c", &cmd])
      .output()
      .map_err(|e| ConfigError::SecretCommand {
        topic: topic_id.to_string(),
        key: cmd_key.clone(),
        reason: e.to_string(),
      })?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(ConfigError::SecretCommand {
        topic: topic_id.to_string(),
        key: cmd_key.clone(),
        reason: format!("exited {}: {}", output.status, stderr.trim()),
      });
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();

    table.insert(base_key, serde_json::Value::String(value));
    table.remove(&cmd_key);
  }

  Ok(())
}

/// Convert raw TOML topic configs into validated `TopicConfig` values,
/// resolving any `_cmd` keys along the way.
fn resolve_topics(
  raw: Vec<TopicConfigRaw>,
) -> Result<Vec<TopicConfig>, ConfigError> {
  raw
    .into_iter()
    .map(|t| {
      let mut json_config = t
        .config
        .map(|v| {
          serde_json::to_value(v)
            .map_err(|e| ConfigError::Validation(e.to_string()))
        })
        .transpose()?
        .unwrap_or(serde_json::Value::Object(Default::default()));

      if let Some(obj) = json_config.as_object_mut() {
        resolve_cmd_keys(&t.id, obj)?;
      } else {
        warn!(
          topic = %t.id,
          "Topic config is not a table, ignoring _cmd resolution"
        );
      }

      Ok(TopicConfig {
        id: t.id,
        queue_name: t.queue,
        config: json_config,
      })
    })
    .collect()
}
