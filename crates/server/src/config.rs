//! Hyuqueue-server configuration.
//!
//! The shape is `Config`; everything else is supporting machinery.
//! `Config` carries the merged result of CLI args, the TOML config
//! file, and defaults.  The `MergeConfig` derive macro generates
//! `CliRaw`, `ConfigFileRaw`, `ConfigError`, `from_cli_and_file`,
//! and the `CliApp` trait impl.
//!
//! Env vars follow the foundation's POSIX-§8.1 convention:
//! `hyuqueue_server_<flag>` (lowercase, underscore-separated).  See
//! `rust-template-foundation/USAGE.org` for the rationale.

use hyuqueue_lib::{LogFormat, LogLevel};
use rust_template_foundation::auth::OidcConfig;
use rust_template_foundation::config::{
  credential_secret_path, ConfigFileError,
};
use rust_template_foundation::MergeConfig;
use serde::Deserialize;
use std::path::PathBuf;
use thiserror::Error;
use tokio_listener::ListenerAddress;
use tracing::warn;

// ── Extra error variant ─────────────────────────────────────────────

/// Errors that are hyuqueue-specific and not part of the foundation's
/// generic `ConfigError` shape.  Flattened into the generated
/// `ConfigError::Extra` via `extra_error = "ExtraConfigError"`.
#[derive(Debug, Error)]
pub enum ExtraConfigError {
  #[error(
    "Failed to run secret command for topic '{topic}' key '{key}': \
     {reason}"
  )]
  SecretCommand {
    topic: String,
    key: String,
    reason: String,
  },

  #[error(
    "Topic '{topic}' is missing required 'command' field (argv for \
     the subprocess)"
  )]
  TopicCommandMissing { topic: String },

  #[error("Failed to convert topic '{topic}' config to JSON: {source}")]
  TopicConfigJson {
    topic: String,
    #[source]
    source: serde_json::Error,
  },
}

// ── Extra CLI args (flattened into the generated CliRaw) ────────────

/// OIDC CLI arguments flattened into the macro-generated `CliRaw`.
/// Env-var names are written out long-hand because clap bakes them
/// into the struct's own attributes at the struct's compile site —
/// the macro's bare-`env` derivation does not reach inside
/// `extra_cli` types.  Names follow the same `hyuqueue_server_*`
/// convention the macro uses elsewhere.
#[derive(Debug, clap::Args)]
pub struct OidcCliFields {
  /// OIDC issuer URL
  /// (e.g. https://sso.example.com/application/o/hyuqueue).
  #[arg(long, env = "hyuqueue_server_oidc_issuer")]
  pub oidc_issuer: Option<String>,

  /// OIDC client ID.
  #[arg(long, env = "hyuqueue_server_oidc_client_id")]
  pub oidc_client_id: Option<String>,

  /// Path to a file containing the OIDC client secret.
  #[arg(long, env = "hyuqueue_server_oidc_client_secret_file")]
  pub oidc_client_secret_file: Option<PathBuf>,
}

// ── Extra file fields (flattened into the generated ConfigFileRaw) ──

/// Config-file fields that don't have a CLI counterpart on a merged
/// `Config` field — OIDC (CLI-side handled via `OidcCliFields`),
/// LLM, and topics.  Flattened into the macro-generated
/// `ConfigFileRaw` via `extra_file = "ExtraFileFields"` and
/// accessible as `file.extra` inside `resolve_<field>` methods.
#[derive(Debug, Deserialize, Default)]
pub struct ExtraFileFields {
  pub oidc_issuer: Option<String>,
  pub oidc_client_id: Option<String>,
  pub oidc_client_secret_file: Option<PathBuf>,
  pub llm: Option<LlmConfigRaw>,
  pub topics: Option<Vec<TopicConfigRaw>>,
}

// ── Raw topic + LLM file shapes ─────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct TopicConfigRaw {
  pub id: String,
  /// argv for the topic subprocess.  First element is the binary
  /// path; remaining elements are passed as args.  Every topic is a
  /// subprocess — see crates/topic-host and crates/topic-sdk.
  pub command: Vec<String>,
  pub config: Option<toml::Value>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LlmConfigRaw {
  pub base_url: Option<String>,
  pub intake_model: Option<String>,
  pub review_model: Option<String>,
  pub api_key: Option<String>,
}

// ── Resolved (validated) types ──────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LlmConfig {
  pub base_url: String,
  pub intake_model: String,
  pub review_model: String,
  pub api_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TopicConfig {
  pub id: String,
  pub command: Vec<String>,
  pub config: serde_json::Value,
}

// ── Merged config ───────────────────────────────────────────────────

#[derive(Debug, MergeConfig)]
#[merge_config(
  app_name = "hyuqueue-server",
  extra_cli = "OidcCliFields",
  extra_file = "ExtraFileFields",
  extra_error = "ExtraConfigError"
)]
pub struct Config {
  #[merge_config(common)]
  pub log_level: LogLevel,

  #[merge_config(common)]
  pub log_format: LogFormat,

  /// Address to listen on: host:port for TCP, /path/to.sock for
  /// Unix socket, or sd-listen to inherit from systemd.
  #[merge_config(
    name = "listen",
    env,
    default = "\"127.0.0.1:8731\".to_string()",
    parse
  )]
  pub listen_address: ListenerAddress,

  /// Path to the SQLite database file.
  #[merge_config(env, default = "default_db_path()")]
  pub db_path: String,

  /// Path to compiled frontend static assets.
  #[merge_config(
    env,
    default = "std::path::PathBuf::from(\"frontend/public\")"
  )]
  pub frontend_path: PathBuf,

  /// Base URL of the service (e.g. https://example.com), used to
  /// construct the OIDC redirect URI.
  #[merge_config(env, default = "\"http://localhost:8731\".to_string()")]
  pub base_url: String,

  #[merge_config(skip)]
  pub oidc: Option<OidcConfig>,

  #[merge_config(skip)]
  pub llm: LlmConfig,

  #[merge_config(skip)]
  pub topics: Vec<TopicConfig>,
}

impl Config {
  fn resolve_oidc(
    cli: &CliRaw,
    file: &ConfigFileRaw,
  ) -> Result<Option<OidcConfig>, ConfigError> {
    let oidc_issuer = cli
      .extra
      .oidc_issuer
      .clone()
      .or_else(|| file.extra.oidc_issuer.clone());
    let oidc_client_id = cli
      .extra
      .oidc_client_id
      .clone()
      .or_else(|| file.extra.oidc_client_id.clone());
    let oidc_secret_file = cli
      .extra
      .oidc_client_secret_file
      .clone()
      .or_else(|| file.extra.oidc_client_secret_file.clone());

    match (&oidc_issuer, &oidc_client_id) {
      (None, None) if oidc_secret_file.is_none() => Ok(None),
      (Some(issuer), Some(client_id)) => {
        let secret_file = oidc_secret_file
          .or_else(credential_secret_path)
          .ok_or_else(|| {
            ConfigError::Validation(
              "oidc_client_secret_file is required when \
               oidc_issuer and oidc_client_id are set (set it \
               explicitly or run under systemd with \
               LoadCredential)"
                .to_string(),
            )
          })?;

        let client_secret = std::fs::read_to_string(&secret_file)
          .map(|s| s.trim().to_string())
          .map_err(|source| ConfigFileError::FileRead {
            path: secret_file,
            source,
          })?;

        Ok(Some(OidcConfig {
          issuer: issuer.clone(),
          client_id: client_id.clone(),
          client_secret,
        }))
      }
      _ => {
        let mut present = Vec::new();
        let mut missing = Vec::new();
        for (name, val) in [
          ("oidc_issuer", oidc_issuer.is_some()),
          ("oidc_client_id", oidc_client_id.is_some()),
          (
            "oidc_client_secret_file",
            oidc_secret_file.is_some() || credential_secret_path().is_some(),
          ),
        ] {
          if val {
            present.push(name);
          } else {
            missing.push(name);
          }
        }
        Err(ConfigError::Validation(format!(
          "partial OIDC configuration: set all three fields \
           or none. present: [{}], missing: [{}]",
          present.join(", "),
          missing.join(", ")
        )))
      }
    }
  }

  fn resolve_llm(
    _cli: &CliRaw,
    file: &ConfigFileRaw,
  ) -> Result<LlmConfig, ConfigError> {
    let raw = file.extra.llm.clone().unwrap_or_default();
    Ok(LlmConfig {
      base_url: raw
        .base_url
        .unwrap_or_else(|| "http://localhost:11434/v1".to_string()),
      intake_model: raw.intake_model.unwrap_or_else(|| "llama3.2".to_string()),
      review_model: raw.review_model.unwrap_or_else(|| "llama3.2".to_string()),
      api_key: raw.api_key,
    })
  }

  fn resolve_topics(
    _cli: &CliRaw,
    file: &ConfigFileRaw,
  ) -> Result<Vec<TopicConfig>, ExtraConfigError> {
    let raw = file.extra.topics.clone().unwrap_or_default();
    resolve_topics(raw)
  }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Resolve an XDG base directory: honour the environment variable if
/// set, otherwise fall back to `$HOME/<default_suffix>`.  Returns
/// `None` when no home directory can be determined (e.g. containers
/// with no `HOME`).
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

/// Resolve `_cmd` suffixed keys in a TOML table: for every key
/// ending in `_cmd`, run the command and store stdout under the
/// base key.
fn resolve_cmd_keys(
  topic_id: &str,
  table: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), ExtraConfigError> {
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
      .map_err(|e| ExtraConfigError::SecretCommand {
        topic: topic_id.to_string(),
        key: cmd_key.clone(),
        reason: e.to_string(),
      })?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(ExtraConfigError::SecretCommand {
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

/// Convert raw TOML topic configs into validated `TopicConfig`
/// values, resolving any `_cmd` keys along the way.
fn resolve_topics(
  raw: Vec<TopicConfigRaw>,
) -> Result<Vec<TopicConfig>, ExtraConfigError> {
  raw
    .into_iter()
    .map(|t| {
      let mut json_config = t
        .config
        .map(|v| {
          serde_json::to_value(v).map_err(|source| {
            ExtraConfigError::TopicConfigJson {
              topic: t.id.clone(),
              source,
            }
          })
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

      if t.command.is_empty() {
        return Err(ExtraConfigError::TopicCommandMissing {
          topic: t.id.clone(),
        });
      }
      Ok(TopicConfig {
        id: t.id,
        command: t.command,
        config: json_config,
      })
    })
    .collect()
}
