//! `Notifier` implementation backed by an mpsc channel.
//!
//! Both response writes and `topic_data_set` notifications need to
//! reach the topic's stdout.  Sharing a writer behind a mutex risks
//! interleaving partial writes; sending serialized lines through a
//! channel to a single writer task avoids that without explicit
//! locking, and gives notifications a natural FIFO ordering relative
//! to responses.

use async_trait::async_trait;
use hyuqueue_core::topic::{Notifier, TopicCtxError};
use hyuqueue_topic_proto::envelope::Notification;
use hyuqueue_topic_proto::method;
use hyuqueue_topic_proto::payload::TopicDataSetParams;
use hyuqueue_topic_proto::version::JsonRpcVersion;
use tokio::sync::mpsc::UnboundedSender;

pub(crate) struct ChannelNotifier {
  tx: UnboundedSender<String>,
}

impl ChannelNotifier {
  pub(crate) fn new(tx: UnboundedSender<String>) -> Self {
    Self { tx }
  }
}

#[async_trait]
impl Notifier for ChannelNotifier {
  async fn set_data(
    &self,
    key: &str,
    value: serde_json::Value,
  ) -> Result<(), TopicCtxError> {
    let payload = TopicDataSetParams {
      key: key.to_string(),
      value,
    };
    let params = serde_json::to_value(payload)?;
    let notification = Notification {
      jsonrpc: JsonRpcVersion,
      method: method::TOPIC_DATA_SET.to_string(),
      params,
    };
    let line = serde_json::to_string(&notification)?;
    self.tx.send(line).map_err(|e| {
      TopicCtxError::NotificationDelivery(format!("writer channel closed: {e}"))
    })
  }
}
