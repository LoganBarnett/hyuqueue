//! hyuqueue-store — SQLite persistence layer.
//!
//! # LLM Development Guidelines
//! - item_events is the source of truth. Never update an event.
//! - items table is a projection: always update it when appending an event.
//! - Use string queries (sqlx::query). No compile-time macros until offline
//!   mode is configured.
//! - All public functions return thiserror error types, not raw sqlx errors.

pub mod db;
pub mod events;
pub mod items;
pub mod queues;
pub mod signals;

pub use db::Db;
