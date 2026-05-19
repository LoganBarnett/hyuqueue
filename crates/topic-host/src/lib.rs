//! hyuqueue-topic-host — server-side wrapper that talks to a topic
//! subprocess over JSON-RPC on its stdin/stdout pipes.
//!
//! [`SubprocessTopic`] implements [`hyuqueue_core::topic::Topic`] so
//! that the existing `TopicRegistry` and worker code can hold one
//! transparently in place of an in-process `Arc<dyn Topic>`.
//!
//! # Construction
//!
//! - [`SubprocessTopic::spawn`] — fork a binary and wire up its
//!   stdin/stdout.  The production path.
//! - [`SubprocessTopic::from_io`] — build from arbitrary
//!   `AsyncRead`/`AsyncWrite` halves.  Used by tests with
//!   `tokio::io::duplex`-backed pipes; symmetric with
//!   `topic-sdk::run_with_io`.
//!
//! # Notifications
//!
//! `topic_data_set` notifications from the topic are dispatched into
//! a [`TopicDataSink`] supplied at construction time.  v1 has the
//! sink fail silently with logging — events are the source of truth,
//! so a missed projection write is recoverable.

pub mod error;
pub mod sink;
pub mod subprocess;

pub use error::HostError;
pub use sink::{NoopSink, TopicDataSink};
pub use subprocess::SubprocessTopic;
