-- hyuqueue initial schema
-- SQLite with JSON columns for flexible data.
-- events is the source of truth; items and topic_data are projections.

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- ── Items (projection) ────────────────────────────────────────────────────────
-- An item is just identity + content + provenance.  Where it is in the
-- system (intake-pending, awaiting-human, etc.) is determined by queue
-- membership — see queue_items below.  There is no `state` enum and no
-- `queue_id`; queues are FIFO containers, not named buckets.

CREATE TABLE IF NOT EXISTS items (
  id               TEXT PRIMARY KEY,
  title            TEXT NOT NULL,
  body             TEXT,
  source_topic_id  TEXT,
  -- Required: identifies the origin system ("email", "jira", "slack", etc.)
  source           TEXT NOT NULL,
  -- JSON: {queue_addr: str, item_id: str} — null if item is local
  delegate_from    TEXT,
  -- JSON: DelegateRef[] — full provenance trail
  delegate_chain   TEXT NOT NULL DEFAULT '[]',
  -- JSON: Activity[] — item-scoped activities from source topic
  capabilities     TEXT NOT NULL DEFAULT '[]',
  -- JSON: arbitrary source-specific data
  metadata         TEXT NOT NULL DEFAULT '{}',
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_items_source ON items(source);

-- ── Queue membership ──────────────────────────────────────────────────────────
-- Reserved system queue names: "intake", "human", "outtake", "errors".
-- The queue_name column is a free-form string; the application enforces
-- which names are valid.  Items live in zero or more queues; queue
-- membership is what determines where an item is in the system.

CREATE TABLE IF NOT EXISTS queue_items (
  queue_name       TEXT NOT NULL,
  item_id          TEXT NOT NULL REFERENCES items(id),
  -- Higher priority dequeues first; FIFO within a priority bucket.
  priority         INTEGER NOT NULL DEFAULT 0,
  enqueued_at      TEXT NOT NULL,
  -- Worker that currently holds the lease (NULL when available).
  claimed_by       TEXT,
  claimed_at       TEXT,
  -- Item becomes available again to other workers when this passes.
  lease_expires_at TEXT,
  PRIMARY KEY (queue_name, item_id)
);

CREATE INDEX IF NOT EXISTS idx_queue_items_dispatch
  ON queue_items (queue_name, priority DESC, enqueued_at);

-- ── Events (source of truth) ─────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS events (
  id         TEXT PRIMARY KEY,
  event_type TEXT NOT NULL,
  actor      TEXT NOT NULL,
  locality   TEXT NOT NULL DEFAULT 'local',
  payload    TEXT NOT NULL DEFAULT '{}',  -- JSON
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);

-- ── Topic data (projection) ─────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS topic_data (
  topic_id   TEXT NOT NULL,
  key        TEXT NOT NULL,
  value      TEXT NOT NULL DEFAULT '{}',
  updated_at TEXT NOT NULL,
  PRIMARY KEY (topic_id, key)
);

-- ── Source policies ───────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS source_policies (
  id                   TEXT PRIMARY KEY,
  source_pattern       TEXT NOT NULL,
  system_prompt        TEXT NOT NULL,
  examples             TEXT NOT NULL DEFAULT '[]',  -- JSON PolicyExample[]
  confidence_threshold REAL NOT NULL DEFAULT 0.8,
  created_at           TEXT NOT NULL,
  updated_at           TEXT NOT NULL
);

-- ── Outbound signals ──────────────────────────────────────────────────────────
-- Upstream signals queued for delivery to remote hyuqueue instances.

CREATE TABLE IF NOT EXISTS outbound_signals (
  id                TEXT PRIMARY KEY,
  item_id           TEXT NOT NULL REFERENCES items(id),
  target_queue_addr TEXT NOT NULL,
  activity_id       TEXT NOT NULL,
  payload           TEXT NOT NULL DEFAULT '{}',  -- JSON
  status            TEXT NOT NULL DEFAULT 'pending',  -- pending | delivered | failed
  attempts          INTEGER NOT NULL DEFAULT 0,
  last_attempt_at   TEXT,
  created_at        TEXT NOT NULL,
  updated_at        TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_outbound_signals_status ON outbound_signals(status);

-- ── Full-text search ──────────────────────────────────────────────────────────

CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
  title,
  body,
  source,
  content = 'items',
  content_rowid = 'rowid'
);

-- Keep FTS index in sync with items table.
CREATE TRIGGER IF NOT EXISTS items_ai AFTER INSERT ON items BEGIN
  INSERT INTO items_fts(rowid, title, body, source)
  VALUES (new.rowid, new.title, new.body, new.source);
END;

CREATE TRIGGER IF NOT EXISTS items_au AFTER UPDATE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, title, body, source)
  VALUES ('delete', old.rowid, old.title, old.body, old.source);
  INSERT INTO items_fts(rowid, title, body, source)
  VALUES (new.rowid, new.title, new.body, new.source);
END;

CREATE TRIGGER IF NOT EXISTS items_ad AFTER DELETE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, title, body, source)
  VALUES ('delete', old.rowid, old.title, old.body, old.source);
END;
