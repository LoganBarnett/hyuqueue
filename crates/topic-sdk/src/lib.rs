//! hyuqueue-topic-sdk — author-side library for building hyuqueue
//! topics that run as out-of-process subprocesses.
//!
//! # Usage
//!
//! ```ignore
//! use hyuqueue_topic_sdk::run;
//! use my_topic::MyTopic;
//!
//! #[tokio::main]
//! async fn main() {
//!     if let Err(e) = run(MyTopic::new()).await {
//!         eprintln!("topic error: {e}");
//!         std::process::exit(1);
//!     }
//! }
//! ```
//!
//! `run` owns the JSON-RPC loop on stdin/stdout, dispatches incoming
//! requests to the topic's trait methods, and serializes responses
//! back.  Topics that need to persist state call
//! [`hyuqueue_core::topic::TopicCtx::set_data`] from inside their
//! `ingest` or `execute` implementations; the SDK turns those calls
//! into `topic_data_set` notifications on stdout.

pub mod error;
pub mod notifier;
pub mod runner;

pub use error::SdkError;
pub use runner::{run, run_with_io};
