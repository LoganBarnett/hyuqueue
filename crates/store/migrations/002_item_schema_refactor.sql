-- Item schema refactor: untangle topic-type / instance / external-id.
--
-- Today `items.source_topic_id` carries the host-assigned instance id
-- and `items.source` carries a topic-type-ish string set by the topic
-- itself.  Three distinct concepts were squeezed into two
-- confusingly-named fields, and there is no place to record a per-item
-- external identifier (RSS guid, email Message-ID, ticket key).
--
-- This migration:
--
--   1. Renames `source_topic_id` to `source_instance_id` to match
--      what it actually carries.
--   2. Adds `external_id` for the per-item upstream identifier.
--      Optional: absence means "no path back to origin" rather than
--      an error.
--   3. Drops the `source` column.  Topic type is now a runtime-derived
--      value (lookup `source_instance_id` against the live config),
--      not persisted per item.  Renaming a topic type no longer
--      strands every existing item under the old name.
--   4. Adds `UNIQUE (source_instance_id, external_id)` so the host
--      enforces per-source dedupe.  SQLite's default NULL-distinct
--      behavior means items without an external_id are not subject
--      to dedupe.
--
-- The items table is rebuilt via temp-swap (CREATE + INSERT + DROP +
-- RENAME) because SQLite cannot add a UNIQUE constraint to an
-- existing table.  Foreign keys are disabled around the swap so the
-- transient DROP of `items` does not cascade-delete `queue_items`
-- rows.  The FK from queue_items.item_id REFERENCES items(id)
-- resolves again by name once the rename completes.

PRAGMA foreign_keys = OFF;

-- FTS triggers and the virtual table reference the `source` column,
-- so they must be torn down before the rebuild and rebuilt with the
-- new shape afterwards.
DROP TRIGGER IF EXISTS items_ai;
DROP TRIGGER IF EXISTS items_au;
DROP TRIGGER IF EXISTS items_ad;
DROP TABLE IF EXISTS items_fts;
DROP INDEX IF EXISTS idx_items_source;

CREATE TABLE items_new (
  id                  TEXT PRIMARY KEY,
  title               TEXT NOT NULL,
  body                TEXT,
  -- Host-assigned instance id from [[topics]].id in config.toml.
  -- For pushed items, supplied by the caller.  Optional: pushed
  -- one-offs and server-generated items may have none.
  source_instance_id  TEXT,
  -- Per-item upstream identifier (RSS guid, email Message-ID,
  -- ticket key).  Optional — topics should populate when the source
  -- has a stable id; absence means no path back to origin.
  external_id         TEXT,
  delegate_from       TEXT,
  delegate_chain      TEXT NOT NULL DEFAULT '[]',
  capabilities        TEXT NOT NULL DEFAULT '[]',
  metadata            TEXT NOT NULL DEFAULT '{}',
  created_at          TEXT NOT NULL,
  updated_at          TEXT NOT NULL,
  -- NULL external_ids are distinct under SQLite's default semantics,
  -- so items without an upstream identifier are not deduped.
  UNIQUE (source_instance_id, external_id)
);

INSERT INTO items_new (
  id, title, body, source_instance_id, external_id,
  delegate_from, delegate_chain, capabilities, metadata,
  created_at, updated_at
)
SELECT
  id, title, body, source_topic_id, NULL,
  delegate_from, delegate_chain, capabilities, metadata,
  created_at, updated_at
FROM items;

DROP TABLE items;
ALTER TABLE items_new RENAME TO items;

CREATE INDEX IF NOT EXISTS idx_items_source_instance
  ON items(source_instance_id);

-- Rebuild FTS keyed by source_instance_id (the durable identifier)
-- rather than the removed source column.
CREATE VIRTUAL TABLE items_fts USING fts5(
  title,
  body,
  source_instance_id,
  content = 'items',
  content_rowid = 'rowid'
);

INSERT INTO items_fts (rowid, title, body, source_instance_id)
SELECT rowid, title, body, source_instance_id FROM items;

CREATE TRIGGER items_ai AFTER INSERT ON items BEGIN
  INSERT INTO items_fts(rowid, title, body, source_instance_id)
  VALUES (new.rowid, new.title, new.body, new.source_instance_id);
END;

CREATE TRIGGER items_au AFTER UPDATE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, title, body, source_instance_id)
  VALUES ('delete', old.rowid, old.title, old.body, old.source_instance_id);
  INSERT INTO items_fts(rowid, title, body, source_instance_id)
  VALUES (new.rowid, new.title, new.body, new.source_instance_id);
END;

CREATE TRIGGER items_ad AFTER DELETE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, title, body, source_instance_id)
  VALUES ('delete', old.rowid, old.title, old.body, old.source_instance_id);
END;

PRAGMA foreign_keys = ON;
