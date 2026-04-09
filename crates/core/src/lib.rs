//! hyuqueue-core — pure domain logic. No I/O, no database, no HTTP.
//!
//! # LLM Development Guidelines
//! - No I/O of any kind in this crate.
//! - All types must be Serialize + Deserialize (they cross the HTTP boundary).
//! - events is the source of truth; items and topic_data are projections.
//! - Activity routing: local vs upstream is determined by ActivityExecutor,
//!   never by activity name or item source.

pub mod activity;
pub mod event;
pub mod item;
pub mod policy;
pub mod queue;
pub mod topic;
