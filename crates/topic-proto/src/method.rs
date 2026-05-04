//! Method-name constants for the topic ↔ host protocol.
//!
//! Using constants instead of bare string literals at call/dispatch
//! sites prevents typos and makes the full set of methods discoverable
//! in one place.

/// Host → topic.  Handshake.  Topic returns id, display_name,
/// declared activities, and which optional methods it implements.
pub const INIT: &str = "init";

/// Host → topic.  Poll the topic for new items.
pub const INGEST: &str = "ingest";

/// Host → topic.  Execute an activity invocation locally.
pub const EXECUTE: &str = "execute";

/// Topic → host (notification).  Persist a key/value into the topic's
/// `topic_data` projection.  Notifications carry no `id` and expect no
/// response.
pub const TOPIC_DATA_SET: &str = "topic_data_set";

/// Host → topic.  Request graceful shutdown before SIGTERM.
pub const SHUTDOWN: &str = "shutdown";
