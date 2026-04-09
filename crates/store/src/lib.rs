//! hyuqueue-store — SQLite persistence layer.
//!
//! # LLM Development Guidelines
//! - events is the source of truth. Never update an event.
//! - items and topic_data are projections: update them when appending events.
//! - Use string queries (sqlx::query). No compile-time macros until offline
//!   mode is configured.
//! - All public functions return thiserror error types, not raw sqlx errors.

pub mod db;
pub mod events;
pub mod items;
pub mod queues;
pub mod signals;
pub mod topic_data;

pub use db::Db;
