//! Outbound signal worker — delivers upstream signals to remote instances.
//!
//! When a human invokes an Upstream activity on a delegated item, the signal
//! is enqueued in outbound_signals. This worker drains that queue by POSTing
//! to the originating instance's /api/v1/push endpoint.

use hyuqueue_store::{Db, signals};
use serde_json::json;
use tokio::time::{Duration, sleep};
use tracing::{error, info, warn};
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const BATCH_SIZE: i64 = 20;
const MAX_ATTEMPTS: i64 = 5;

pub async fn run(db: Db) {
  let http = reqwest::Client::new();
  info!("Outbound signal worker started");

  loop {
    match deliver_batch(&db, &http).await {
      Ok(delivered) if delivered > 0 => {
        info!(count = delivered, "Outbound worker delivered signals");
      }
      Ok(_) => {}
      Err(e) => {
        error!("Outbound worker error: {e}");
      }
    }
    sleep(POLL_INTERVAL).await;
  }
}

async fn deliver_batch(
  db: &Db,
  http: &reqwest::Client,
) -> Result<usize, Box<dyn std::error::Error>> {
  let pending = signals::pending(db, BATCH_SIZE).await?;
  let count = pending.len();

  for signal in pending {
    let signal_id = Uuid::parse_str(&signal.id).unwrap_or_default();
    let attempts = signal.attempts + 1;

    let payload: serde_json::Value =
      serde_json::from_str(&signal.payload).unwrap_or(json!({}));

    let url = format!(
      "{}/api/v1/items/{}/action",
      signal.target_queue_addr.trim_end_matches('/'),
      signal.item_id
    );

    let body = json!({
      "activity_id": signal.activity_id,
      "params": payload,
    });

    match http.post(&url).json(&body).send().await {
      Ok(resp) if resp.status().is_success() => {
        info!(
          signal_id = %signal_id,
          target = %signal.target_queue_addr,
          "Upstream signal delivered"
        );
        let _ = signals::mark_delivered(db, signal_id).await;
      }
      Ok(resp) => {
        warn!(
          signal_id = %signal_id,
          status = %resp.status(),
          attempt = attempts,
          "Upstream signal delivery failed"
        );
        if attempts >= MAX_ATTEMPTS {
          let _ = signals::mark_failed(db, signal_id, attempts).await;
        }
      }
      Err(e) => {
        warn!(
          signal_id = %signal_id,
          attempt = attempts,
          "Upstream signal HTTP error: {e}"
        );
        if attempts >= MAX_ATTEMPTS {
          let _ = signals::mark_failed(db, signal_id, attempts).await;
        }
      }
    }
  }

  Ok(count)
}
