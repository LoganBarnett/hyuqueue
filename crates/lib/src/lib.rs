//! hyuqueue-lib — shared types used across all crates.
//!
//! # LLM Development Guidelines
//! - Keep this crate minimal: only truly shared primitives live here.
//! - Domain logic belongs in hyuqueue-core.
//! - No I/O in this crate.

pub mod llm;
pub mod logging;

pub use logging::{LogFormat, LogLevel};
