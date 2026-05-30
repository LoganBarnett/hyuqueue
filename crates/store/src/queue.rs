//! Queue operations against the `queue_items` table.
//!
//! A queue is identified by a free-form `queue_name` string;
//! callers should use the constants from `hyuqueue_core::queue`
//! (`INTAKE`, `HUMAN`, `OUTTAKE`, `ERRORS`) for the reserved
//! system names.
//!
//! See `tasks.org` "Queues are FIFO containers; there are no
//! named buckets" for the conceptual model.
//!
//! # Transactions
//!
//! Single-statement operations take an `impl SqliteExecutor<'_>`
//! and work with either a pool, a transaction, or a raw connection.
//! Multi-statement operations (`move_item`, `dequeue`) take a
//! `&mut SqliteConnection` so the caller controls the transaction
//! scope.  Compose with other DB work like:
//!
//! ```ignore
//! let mut tx = db.pool().begin().await?;
//! queue::force_claim(&mut *tx, "human", id, worker, lease).await?;
//! events::append(&mut *tx, &ack_event).await?;
//! queue::move_item(&mut *tx, id, "human", "outtake", worker).await?;
//! tx.commit().await?;
//! ```
//!
//! [`dequeue_one`] is a convenience wrapper that handles
//! `BEGIN IMMEDIATE` and commit on a fresh transaction — use it
//! when the dequeue is the entire unit of work.

use crate::db::Db;
use chrono::{DateTime, Duration, Utc};
use hyuqueue_core::queue::SourceFilter;
use sqlx::{Acquire, SqliteConnection, SqliteExecutor};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum QueueError {
  #[error("Database error while {context}: {source}")]
  Db {
    context: &'static str,
    #[source]
    source: sqlx::Error,
  },

  #[error(
    "Worker '{worker_id}' does not hold the lease on item '{item_id}' \
     in queue '{queue_name}'"
  )]
  ClaimMismatch {
    queue_name: String,
    item_id: Uuid,
    worker_id: String,
  },
}

/// A row from `queue_items` — pairs an item id with its dispatch
/// metadata.  Returned by `list` and (indirectly) by `dequeue`.
#[derive(Debug, Clone)]
pub struct QueueEntry {
  pub queue_name: String,
  pub item_id: Uuid,
  pub priority: i64,
  pub enqueued_at: DateTime<Utc>,
  pub claimed_by: Option<String>,
  pub claimed_at: Option<DateTime<Utc>>,
  pub lease_expires_at: Option<DateTime<Utc>>,
}

/// Priority value used by `urgent_enqueue`.  Higher than the default
/// (0) so urgent items dispatch before regular items regardless of
/// age.  Exposed as a constant so callers can use the same value
/// when calling `enqueue` directly.
pub const URGENT_PRIORITY: i64 = 1000;

// ── enqueue ──────────────────────────────────────────────────────────

/// Add an item to a queue at the given priority.  No-op if the item
/// is already in the queue (PRIMARY KEY conflict).
pub async fn enqueue<'e, E: SqliteExecutor<'e>>(
  executor: E,
  queue_name: &str,
  item_id: Uuid,
  priority: i64,
) -> Result<(), QueueError> {
  let now = Utc::now().to_rfc3339();
  sqlx::query(
    "INSERT INTO queue_items
       (queue_name, item_id, priority, enqueued_at)
     VALUES (?, ?, ?, ?)
     ON CONFLICT (queue_name, item_id) DO NOTHING",
  )
  .bind(queue_name)
  .bind(item_id.to_string())
  .bind(priority)
  .bind(now)
  .execute(executor)
  .await
  .map_err(|source| QueueError::Db {
    context: "enqueuing item",
    source,
  })?;
  Ok(())
}

/// Sugar for `enqueue` with [`URGENT_PRIORITY`].
pub async fn urgent_enqueue<'e, E: SqliteExecutor<'e>>(
  executor: E,
  queue_name: &str,
  item_id: Uuid,
) -> Result<(), QueueError> {
  enqueue(executor, queue_name, item_id, URGENT_PRIORITY).await
}

// ── dequeue ──────────────────────────────────────────────────────────

/// Atomically claim the next available item in `queue_name` for
/// `worker_id`, with a lease lasting `lease`.  Returns `None` if no
/// item is available (queue empty or all items claimed by other
/// workers with unexpired leases).
///
/// Caller must pass a connection (or transaction) that has been set
/// up with `BEGIN IMMEDIATE` if there's any chance of contention —
/// SQLite's default `BEGIN DEFERRED` doesn't serialize the
/// select-then-update race.  For the common "dequeue is the whole
/// unit of work" case use [`dequeue_one`] which handles the BEGIN
/// IMMEDIATE / COMMIT.
pub async fn dequeue(
  conn: &mut SqliteConnection,
  queue_name: &str,
  worker_id: &str,
  lease: Duration,
) -> Result<Option<QueueEntry>, QueueError> {
  let now = Utc::now();
  let now_str = now.to_rfc3339();

  let row: Option<(
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
  )> = sqlx::query_as(
    "SELECT item_id, priority, enqueued_at,
            claimed_by, claimed_at, lease_expires_at
     FROM queue_items
     WHERE queue_name = ?
       AND (claimed_by IS NULL OR lease_expires_at < ?)
     ORDER BY priority DESC, enqueued_at ASC
     LIMIT 1",
  )
  .bind(queue_name)
  .bind(&now_str)
  .fetch_optional(&mut *conn)
  .await
  .map_err(|source| QueueError::Db {
    context: "selecting next queue item",
    source,
  })?;

  let Some((item_id_str, priority, enqueued_at, _, _, _)) = row else {
    return Ok(None);
  };

  let lease_expires_at = (now + lease).to_rfc3339();
  sqlx::query(
    "UPDATE queue_items
     SET claimed_by = ?, claimed_at = ?, lease_expires_at = ?
     WHERE queue_name = ? AND item_id = ?",
  )
  .bind(worker_id)
  .bind(&now_str)
  .bind(&lease_expires_at)
  .bind(queue_name)
  .bind(&item_id_str)
  .execute(&mut *conn)
  .await
  .map_err(|source| QueueError::Db {
    context: "claiming queue item",
    source,
  })?;

  Ok(Some(QueueEntry {
    queue_name: queue_name.to_string(),
    item_id: parse_uuid(&item_id_str)?,
    priority,
    enqueued_at: parse_rfc3339(&enqueued_at)?,
    claimed_by: Some(worker_id.to_string()),
    claimed_at: Some(now),
    lease_expires_at: Some(now + lease),
  }))
}

/// Convenience wrapper: dequeue in its own race-safe transaction
/// (`BEGIN IMMEDIATE` + `COMMIT`).  Use when the dequeue is the
/// entire unit of work — i.e., you don't need to compose it with
/// other DB operations.
pub async fn dequeue_one(
  db: &Db,
  queue_name: &str,
  worker_id: &str,
  lease: Duration,
) -> Result<Option<QueueEntry>, QueueError> {
  let mut conn =
    db.pool().acquire().await.map_err(|source| QueueError::Db {
      context: "acquiring connection for dequeue",
      source,
    })?;
  // SQLite lacks `SELECT ... FOR UPDATE SKIP LOCKED`; serialize the
  // claim race via `BEGIN IMMEDIATE` so two callers can't both win.
  sqlx::query("BEGIN IMMEDIATE")
    .execute(&mut *conn)
    .await
    .map_err(|source| QueueError::Db {
      context: "starting dequeue transaction",
      source,
    })?;

  let result = dequeue(&mut conn, queue_name, worker_id, lease).await;

  let commit_sql = match &result {
    Ok(_) => "COMMIT",
    Err(_) => "ROLLBACK",
  };
  if let Err(commit_err) = sqlx::query(commit_sql).execute(&mut *conn).await {
    // If commit itself fails we lose the work, but the dequeue
    // result is what callers care about.  Log via the error
    // context attached to the original result if any; otherwise
    // surface the commit failure.
    if result.is_ok() {
      return Err(QueueError::Db {
        context: "committing dequeue transaction",
        source: commit_err,
      });
    }
  }
  result
}

// ── lease management ─────────────────────────────────────────────────

/// Extend the lease on an item the worker has claimed.  Returns
/// `ClaimMismatch` if `worker_id` doesn't currently hold the
/// claim (either lease expired, another worker took it, or the
/// worker_id was wrong).
pub async fn renew_lease<'e, E: SqliteExecutor<'e>>(
  executor: E,
  queue_name: &str,
  item_id: Uuid,
  worker_id: &str,
  new_duration: Duration,
) -> Result<(), QueueError> {
  let new_expires_at = (Utc::now() + new_duration).to_rfc3339();
  let rows = sqlx::query(
    "UPDATE queue_items
     SET lease_expires_at = ?
     WHERE queue_name = ? AND item_id = ? AND claimed_by = ?",
  )
  .bind(new_expires_at)
  .bind(queue_name)
  .bind(item_id.to_string())
  .bind(worker_id)
  .execute(executor)
  .await
  .map_err(|source| QueueError::Db {
    context: "renewing lease",
    source,
  })?;
  if rows.rows_affected() == 0 {
    return Err(QueueError::ClaimMismatch {
      queue_name: queue_name.to_string(),
      item_id,
      worker_id: worker_id.to_string(),
    });
  }
  Ok(())
}

/// Release the worker's claim, returning the item to the queue for
/// any worker to pick up.  Returns `ClaimMismatch` if the worker
/// doesn't currently hold the claim.
pub async fn release<'e, E: SqliteExecutor<'e>>(
  executor: E,
  queue_name: &str,
  item_id: Uuid,
  worker_id: &str,
) -> Result<(), QueueError> {
  let rows = sqlx::query(
    "UPDATE queue_items
     SET claimed_by = NULL, claimed_at = NULL, lease_expires_at = NULL
     WHERE queue_name = ? AND item_id = ? AND claimed_by = ?",
  )
  .bind(queue_name)
  .bind(item_id.to_string())
  .bind(worker_id)
  .execute(executor)
  .await
  .map_err(|source| QueueError::Db {
    context: "releasing claim",
    source,
  })?;
  if rows.rows_affected() == 0 {
    return Err(QueueError::ClaimMismatch {
      queue_name: queue_name.to_string(),
      item_id,
      worker_id: worker_id.to_string(),
    });
  }
  Ok(())
}

/// Remove the item from the queue.  Requires the worker to hold the
/// claim — returns `ClaimMismatch` otherwise.
pub async fn complete<'e, E: SqliteExecutor<'e>>(
  executor: E,
  queue_name: &str,
  item_id: Uuid,
  worker_id: &str,
) -> Result<(), QueueError> {
  let rows = sqlx::query(
    "DELETE FROM queue_items
     WHERE queue_name = ? AND item_id = ? AND claimed_by = ?",
  )
  .bind(queue_name)
  .bind(item_id.to_string())
  .bind(worker_id)
  .execute(executor)
  .await
  .map_err(|source| QueueError::Db {
    context: "completing queue item",
    source,
  })?;
  if rows.rows_affected() == 0 {
    return Err(QueueError::ClaimMismatch {
      queue_name: queue_name.to_string(),
      item_id,
      worker_id: worker_id.to_string(),
    });
  }
  Ok(())
}

/// Force-claim a specific item id regardless of who currently holds
/// the lease.  Used by trusted operator-side actions (HTTP API
/// representing the local human, primarily) where the request is
/// authoritative and we want to bypass the lease coordination
/// designed for multi-worker contention.
///
/// Workers should use [`dequeue`] (which respects existing claims
/// and provides lease semantics) instead.
pub async fn force_claim<'e, E: SqliteExecutor<'e>>(
  executor: E,
  queue_name: &str,
  item_id: Uuid,
  worker_id: &str,
  lease: Duration,
) -> Result<(), QueueError> {
  let now = Utc::now();
  let result = sqlx::query(
    "UPDATE queue_items
     SET claimed_by = ?, claimed_at = ?, lease_expires_at = ?
     WHERE queue_name = ? AND item_id = ?",
  )
  .bind(worker_id)
  .bind(now.to_rfc3339())
  .bind((now + lease).to_rfc3339())
  .bind(queue_name)
  .bind(item_id.to_string())
  .execute(executor)
  .await
  .map_err(|source| QueueError::Db {
    context: "force-claiming queue item",
    source,
  })?;
  if result.rows_affected() == 0 {
    return Err(QueueError::Db {
      context: "force-claiming queue item",
      source: sqlx::Error::RowNotFound,
    });
  }
  Ok(())
}

/// Atomically move an item from one queue to another.  Requires the
/// worker to hold the claim on the source queue.  Multi-statement;
/// caller controls the transaction scope by passing a connection
/// (typically `&mut *tx`).
pub async fn move_item(
  conn: &mut SqliteConnection,
  item_id: Uuid,
  from_queue: &str,
  to_queue: &str,
  worker_id: &str,
) -> Result<(), QueueError> {
  let deleted = sqlx::query(
    "DELETE FROM queue_items
     WHERE queue_name = ? AND item_id = ? AND claimed_by = ?",
  )
  .bind(from_queue)
  .bind(item_id.to_string())
  .bind(worker_id)
  .execute(&mut *conn)
  .await
  .map_err(|source| QueueError::Db {
    context: "removing from source queue",
    source,
  })?;
  if deleted.rows_affected() == 0 {
    return Err(QueueError::ClaimMismatch {
      queue_name: from_queue.to_string(),
      item_id,
      worker_id: worker_id.to_string(),
    });
  }
  let now = Utc::now().to_rfc3339();
  sqlx::query(
    "INSERT INTO queue_items
       (queue_name, item_id, priority, enqueued_at)
     VALUES (?, ?, 0, ?)
     ON CONFLICT (queue_name, item_id) DO NOTHING",
  )
  .bind(to_queue)
  .bind(item_id.to_string())
  .bind(now)
  .execute(&mut *conn)
  .await
  .map_err(|source| QueueError::Db {
    context: "inserting into destination queue",
    source,
  })?;
  Ok(())
}

/// Convenience: [`move_item`] in its own transaction.  Use when the
/// move is the entire unit of work.
pub async fn move_item_one(
  db: &Db,
  item_id: Uuid,
  from_queue: &str,
  to_queue: &str,
  worker_id: &str,
) -> Result<(), QueueError> {
  let mut tx = db.pool().begin().await.map_err(|source| QueueError::Db {
    context: "starting move transaction",
    source,
  })?;
  move_item(&mut tx, item_id, from_queue, to_queue, worker_id).await?;
  tx.commit().await.map_err(|source| QueueError::Db {
    context: "committing move",
    source,
  })?;
  Ok(())
}

// ── observability ────────────────────────────────────────────────────

/// List queue entries (with optional filter on the underlying item's
/// source).  Shows all entries — claimed or not — so callers can see
/// what workers are working on.
pub async fn list<'e, E: SqliteExecutor<'e>>(
  executor: E,
  queue_name: &str,
  filter: &SourceFilter,
) -> Result<Vec<QueueEntry>, QueueError> {
  let source_filter = filter.source_instance_id.as_deref();
  let rows: Vec<(
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
  )> = sqlx::query_as(
    "SELECT q.item_id, q.priority, q.enqueued_at,
            q.claimed_by, q.claimed_at, q.lease_expires_at
     FROM queue_items q
     INNER JOIN items i ON i.id = q.item_id
     WHERE q.queue_name = ?
       AND (? IS NULL OR i.source_instance_id = ?)
     ORDER BY q.priority DESC, q.enqueued_at ASC",
  )
  .bind(queue_name)
  .bind(source_filter)
  .bind(source_filter)
  .fetch_all(executor)
  .await
  .map_err(|source| QueueError::Db {
    context: "listing queue",
    source,
  })?;

  rows
    .into_iter()
    .map(
      |(
        item_id_str,
        priority,
        enqueued_at,
        claimed_by,
        claimed_at,
        lease_expires_at,
      )| {
        Ok(QueueEntry {
          queue_name: queue_name.to_string(),
          item_id: parse_uuid(&item_id_str)?,
          priority,
          enqueued_at: parse_rfc3339(&enqueued_at)?,
          claimed_by,
          claimed_at: claimed_at.as_deref().map(parse_rfc3339).transpose()?,
          lease_expires_at: lease_expires_at
            .as_deref()
            .map(parse_rfc3339)
            .transpose()?,
        })
      },
    )
    .collect()
}

/// Count of available (unclaimed or expired-lease) items in the
/// queue.  Excludes items currently being processed by a worker.
pub async fn depth<'e, E: SqliteExecutor<'e>>(
  executor: E,
  queue_name: &str,
) -> Result<i64, QueueError> {
  let now = Utc::now().to_rfc3339();
  let (count,): (i64,) = sqlx::query_as(
    "SELECT COUNT(*) FROM queue_items
     WHERE queue_name = ?
       AND (claimed_by IS NULL OR lease_expires_at < ?)",
  )
  .bind(queue_name)
  .bind(now)
  .fetch_one(executor)
  .await
  .map_err(|source| QueueError::Db {
    context: "counting queue depth",
    source,
  })?;
  Ok(count)
}

// ── helpers for callers ──────────────────────────────────────────────

/// Acquire a connection and start a `BEGIN IMMEDIATE` transaction.
/// Use this when a caller composes [`dequeue`] (or other ops with
/// claim-race semantics) with other DB work.  Caller is responsible
/// for committing or rolling back the returned transaction.
pub async fn begin_immediate_tx(
  db: &Db,
) -> Result<sqlx::Transaction<'_, sqlx::Sqlite>, QueueError> {
  // sqlx's `begin()` uses `BEGIN DEFERRED`.  We need `IMMEDIATE` so
  // the select-then-update in `dequeue` is race-safe.  Acquire a
  // connection, issue `BEGIN IMMEDIATE`, and hand back a
  // `Transaction` that owns the connection so the caller gets
  // proper RAII commit/rollback.
  let mut tx = db.pool().begin().await.map_err(|source| QueueError::Db {
    context: "beginning transaction",
    source,
  })?;
  // sqlx tx is in DEFERRED mode; upgrade by hand.  This sends
  // ROLLBACK + BEGIN IMMEDIATE atomically on the same connection.
  sqlx::query("ROLLBACK; BEGIN IMMEDIATE")
    .execute(tx.acquire().await.map_err(|source| QueueError::Db {
      context: "acquiring connection for BEGIN IMMEDIATE",
      source,
    })?)
    .await
    .map_err(|source| QueueError::Db {
      context: "upgrading transaction to IMMEDIATE",
      source,
    })?;
  Ok(tx)
}

// ── parse helpers ────────────────────────────────────────────────────

fn parse_uuid(s: &str) -> Result<Uuid, QueueError> {
  Uuid::parse_str(s).map_err(|_| QueueError::Db {
    context: "parsing UUID from queue row",
    source: sqlx::Error::Decode(
      format!("invalid UUID in queue_items: {s}").into(),
    ),
  })
}

fn parse_rfc3339(s: &str) -> Result<DateTime<Utc>, QueueError> {
  DateTime::parse_from_rfc3339(s)
    .map(|d| d.with_timezone(&Utc))
    .map_err(|_| QueueError::Db {
      context: "parsing timestamp from queue row",
      source: sqlx::Error::Decode(
        format!("invalid RFC3339 timestamp in queue_items: {s}").into(),
      ),
    })
}

// ── tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use hyuqueue_core::item::Item;
  use std::time::Duration as StdDuration;

  async fn fresh_db() -> Db {
    Db::open(":memory:").await.unwrap()
  }

  async fn insert_item(db: &Db, source_instance_id: &str) -> Uuid {
    let now = Utc::now();
    let item = Item {
      id: Uuid::new_v4(),
      title: format!("test item ({source_instance_id})"),
      body: None,
      source_instance_id: Some(source_instance_id.to_string()),
      external_id: None,
      delegate_from: None,
      delegate_chain: vec![],
      capabilities: vec![],
      metadata: serde_json::json!({}),
      created_at: now,
      updated_at: now,
    };
    crate::items::insert(db.pool(), &item).await.unwrap();
    item.id
  }

  #[tokio::test]
  async fn enqueue_and_dequeue_roundtrip() {
    let db = fresh_db().await;
    let item = insert_item(&db, "test").await;
    enqueue(db.pool(), "intake", item, 0).await.unwrap();
    let entry = dequeue_one(&db, "intake", "worker-1", Duration::seconds(30))
      .await
      .unwrap()
      .expect("expected an entry");
    assert_eq!(entry.item_id, item);
    assert_eq!(entry.claimed_by.as_deref(), Some("worker-1"));
  }

  #[tokio::test]
  async fn dequeue_empty_returns_none() {
    let db = fresh_db().await;
    let entry = dequeue_one(&db, "intake", "worker-1", Duration::seconds(30))
      .await
      .unwrap();
    assert!(entry.is_none());
  }

  #[tokio::test]
  async fn dequeue_skips_claimed_items() {
    let db = fresh_db().await;
    let item = insert_item(&db, "test").await;
    enqueue(db.pool(), "intake", item, 0).await.unwrap();
    let _ = dequeue_one(&db, "intake", "worker-1", Duration::seconds(30))
      .await
      .unwrap()
      .expect("first claim");
    let entry = dequeue_one(&db, "intake", "worker-2", Duration::seconds(30))
      .await
      .unwrap();
    assert!(entry.is_none(), "second worker should see no available item");
  }

  #[tokio::test]
  async fn dequeue_picks_up_expired_lease() {
    let db = fresh_db().await;
    let item = insert_item(&db, "test").await;
    enqueue(db.pool(), "intake", item, 0).await.unwrap();
    let _ = dequeue_one(&db, "intake", "worker-1", Duration::milliseconds(1))
      .await
      .unwrap()
      .expect("first claim");
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    let entry = dequeue_one(&db, "intake", "worker-2", Duration::seconds(30))
      .await
      .unwrap()
      .expect("worker-2 reclaims expired");
    assert_eq!(entry.item_id, item);
    assert_eq!(entry.claimed_by.as_deref(), Some("worker-2"));
  }

  #[tokio::test]
  async fn urgent_enqueue_dispatches_before_regular() {
    let db = fresh_db().await;
    let regular = insert_item(&db, "test").await;
    let urgent = insert_item(&db, "test").await;
    enqueue(db.pool(), "human", regular, 0).await.unwrap();
    tokio::time::sleep(StdDuration::from_millis(5)).await;
    urgent_enqueue(db.pool(), "human", urgent).await.unwrap();
    let entry = dequeue_one(&db, "human", "worker-1", Duration::seconds(30))
      .await
      .unwrap()
      .expect("expected an entry");
    assert_eq!(entry.item_id, urgent);
  }

  #[tokio::test]
  async fn release_returns_item_to_queue() {
    let db = fresh_db().await;
    let item = insert_item(&db, "test").await;
    enqueue(db.pool(), "intake", item, 0).await.unwrap();
    let _ = dequeue_one(&db, "intake", "worker-1", Duration::seconds(30))
      .await
      .unwrap()
      .expect("first claim");
    release(db.pool(), "intake", item, "worker-1")
      .await
      .unwrap();
    let entry = dequeue_one(&db, "intake", "worker-2", Duration::seconds(30))
      .await
      .unwrap()
      .expect("worker-2 reclaims released");
    assert_eq!(entry.item_id, item);
    assert_eq!(entry.claimed_by.as_deref(), Some("worker-2"));
  }

  #[tokio::test]
  async fn complete_removes_item_from_queue() {
    let db = fresh_db().await;
    let item = insert_item(&db, "test").await;
    enqueue(db.pool(), "intake", item, 0).await.unwrap();
    let _ = dequeue_one(&db, "intake", "worker-1", Duration::seconds(30))
      .await
      .unwrap()
      .expect("first claim");
    complete(db.pool(), "intake", item, "worker-1")
      .await
      .unwrap();
    assert_eq!(depth(db.pool(), "intake").await.unwrap(), 0);
  }

  #[tokio::test]
  async fn worker_id_mismatch_returns_error() {
    let db = fresh_db().await;
    let item = insert_item(&db, "test").await;
    enqueue(db.pool(), "intake", item, 0).await.unwrap();
    let _ = dequeue_one(&db, "intake", "worker-1", Duration::seconds(30))
      .await
      .unwrap()
      .expect("first claim");

    let err = complete(db.pool(), "intake", item, "worker-2")
      .await
      .unwrap_err();
    assert!(matches!(err, QueueError::ClaimMismatch { .. }));

    let err = release(db.pool(), "intake", item, "worker-2")
      .await
      .unwrap_err();
    assert!(matches!(err, QueueError::ClaimMismatch { .. }));

    let err =
      renew_lease(db.pool(), "intake", item, "worker-2", Duration::seconds(60))
        .await
        .unwrap_err();
    assert!(matches!(err, QueueError::ClaimMismatch { .. }));
  }

  #[tokio::test]
  async fn renew_lease_extends_expiry() {
    let db = fresh_db().await;
    let item = insert_item(&db, "test").await;
    enqueue(db.pool(), "intake", item, 0).await.unwrap();
    let _ = dequeue_one(&db, "intake", "worker-1", Duration::milliseconds(50))
      .await
      .unwrap()
      .expect("first claim");
    renew_lease(db.pool(), "intake", item, "worker-1", Duration::seconds(30))
      .await
      .unwrap();
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    let entry = dequeue_one(&db, "intake", "worker-2", Duration::seconds(30))
      .await
      .unwrap();
    assert!(entry.is_none(), "renewed lease should still be held");
  }

  #[tokio::test]
  async fn move_item_atomic_across_queues() {
    let db = fresh_db().await;
    let item = insert_item(&db, "test").await;
    enqueue(db.pool(), "intake", item, 0).await.unwrap();
    let _ = dequeue_one(&db, "intake", "worker-1", Duration::seconds(30))
      .await
      .unwrap()
      .expect("first claim");
    move_item_one(&db, item, "intake", "human", "worker-1")
      .await
      .unwrap();
    assert_eq!(depth(db.pool(), "intake").await.unwrap(), 0);
    assert_eq!(depth(db.pool(), "human").await.unwrap(), 1);
  }

  #[tokio::test]
  async fn move_item_composes_with_other_work_in_one_tx() {
    let db = fresh_db().await;
    let item = insert_item(&db, "test").await;
    enqueue(db.pool(), "intake", item, 0).await.unwrap();
    let _ = dequeue_one(&db, "intake", "worker-1", Duration::seconds(30))
      .await
      .unwrap()
      .expect("first claim");
    // Prepare a second item ahead of the tx — SQLite's pool would
    // deadlock if we issued a write on a fresh connection while
    // holding a write tx on another.
    let other = insert_item(&db, "test").await;

    // Compose move_item + a second enqueue inside one transaction.
    let mut tx = db.pool().begin().await.unwrap();
    move_item(&mut tx, item, "intake", "human", "worker-1")
      .await
      .unwrap();
    enqueue(&mut *tx, "human", other, 0).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(depth(db.pool(), "human").await.unwrap(), 2);
  }

  #[tokio::test]
  async fn depth_excludes_claimed_items() {
    let db = fresh_db().await;
    let a = insert_item(&db, "test").await;
    let b = insert_item(&db, "test").await;
    enqueue(db.pool(), "intake", a, 0).await.unwrap();
    enqueue(db.pool(), "intake", b, 0).await.unwrap();
    assert_eq!(depth(db.pool(), "intake").await.unwrap(), 2);
    let _ = dequeue_one(&db, "intake", "worker-1", Duration::seconds(30))
      .await
      .unwrap()
      .expect("claim one");
    assert_eq!(depth(db.pool(), "intake").await.unwrap(), 1);
  }

  #[tokio::test]
  async fn list_with_source_filter() {
    let db = fresh_db().await;
    let email = insert_item(&db, "email").await;
    let jira = insert_item(&db, "jira").await;
    enqueue(db.pool(), "human", email, 0).await.unwrap();
    enqueue(db.pool(), "human", jira, 0).await.unwrap();

    let all = list(db.pool(), "human", &SourceFilter::default())
      .await
      .unwrap();
    assert_eq!(all.len(), 2);

    let only_email = list(
      db.pool(),
      "human",
      &SourceFilter {
        source_instance_id: Some("email".to_string()),
      },
    )
    .await
    .unwrap();
    assert_eq!(only_email.len(), 1);
    assert_eq!(only_email[0].item_id, email);
  }
}
