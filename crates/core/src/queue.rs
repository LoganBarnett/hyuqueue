//! Queue primitives.
//!
//! A queue is a FIFO container with a priority/jump-line affordance.
//! Items live in zero or more queues; queue membership is what
//! determines where an item is in the system.  There are no
//! user-named buckets — slicing a queue for a session ("just process
//! email today") is a filter applied to the single queue per system
//! role, not a separate container.
//!
//! The data layer for queue operations lives in `hyuqueue-store::queue`.
//! Domain types live here.

use serde::{Deserialize, Serialize};

/// Reserved name of the queue holding items awaiting intake LLM
/// processing.
pub const INTAKE: &str = "intake";

/// Reserved name of the queue holding items awaiting human attention.
pub const HUMAN: &str = "human";

/// Reserved name of the queue holding items awaiting outtake LLM
/// processing after a human ack.
pub const OUTTAKE: &str = "outtake";

/// Reserved name of the queue holding items whose processing hit an
/// unrecoverable failure.  Items here typically need software
/// engineering attention rather than triage; see the error-topic
/// recursion idea in `tasks.org` for the post-MVP plan.
pub const ERRORS: &str = "errors";

/// Filter expression applied to `list(queue, filter)` operations.
///
/// v1 supports filtering by `source_instance_id` only.  Richer
/// expressions (topic-type via runtime config lookup, metadata JSON
/// paths, full-text search) land later — the data layer just needs
/// to accept this struct, and the presentation layer (CLI, TUI,
/// Emacs) builds it from user input.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceFilter {
  /// Only return items whose `source_instance_id` equals this
  /// string.  `None` means no filter.
  pub source_instance_id: Option<String>,
}
