//! Workspace-wide log level / format types.
//!
//! Re-exported from `rust-template-foundation`'s `logging` module so
//! the workspace and the foundation use the same types.  The `common`
//! attribute on `MergeConfig` (in `crates/server/src/config.rs`)
//! expects foundation's `LogLevel` / `LogFormat`; aliasing rather
//! than redefining keeps every call site that imports
//! `hyuqueue_lib::{LogLevel, LogFormat}` working unchanged.
pub use rust_template_foundation::logging::{
  LogFormat, LogFormatParseError, LogLevel, LogLevelParseError,
};
