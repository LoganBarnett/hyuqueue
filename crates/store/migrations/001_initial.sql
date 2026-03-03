-- hyuqueue initial schema
-- SQLite with JSON columns for flexible data.
-- item_events is the source of truth; items is a materialized projection.

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- ── Queues ────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS queues (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL UNIQUE,
  tags       TEXT NOT NULL DEFAULT '[]',  -- JSON string[]
  config     TEXT NOT NULL DEFAULT '{}',  -- JSON
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

-- ── Items (projection) ────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS items (
  id               TEXT PRIMARY KEY,
  queue_id         TEXT NOT NULL REFERENCES queues(id),
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
  state            TEXT NOT NULL DEFAULT 'intake_pending',
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_items_queue_id ON items(queue_id);
CREATE INDEX IF NOT EXISTS idx_items_state    ON items(state);
CREATE INDEX IF NOT EXISTS idx_items_source   ON items(source);

-- ── Item events (source of truth) ────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS item_events (
  id         TEXT PRIMARY KEY,
  item_id    TEXT NOT NULL REFERENCES items(id),
  event_type TEXT NOT NULL,
  actor      TEXT NOT NULL,
  locality   TEXT NOT NULL DEFAULT 'local',
  payload    TEXT NOT NULL DEFAULT '{}',  -- JSON
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_item_events_item_id ON item_events(item_id);
CREATE INDEX IF NOT EXISTS idx_item_events_type    ON item_events(event_type);

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
