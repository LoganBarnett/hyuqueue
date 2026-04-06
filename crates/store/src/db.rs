use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
  #[error("Failed to connect to database at '{path}': {source}")]
  Connect {
    path: String,
    #[source]
    source: sqlx::Error,
  },

  #[error("Failed to run database migrations: {0}")]
  Migration(#[source] sqlx::Error),
}

/// Thin wrapper around SqlitePool, the single handle to the database.
#[derive(Clone)]
pub struct Db(pub SqlitePool);

impl Db {
  /// Open (or create) the SQLite database at `path` and run migrations.
  pub async fn open(path: &str) -> Result<Self, DbError> {
    let url = if path == ":memory:" {
      ":memory:".to_string()
    } else {
      format!("sqlite:{path}?mode=rwc")
    };

    let pool = SqlitePoolOptions::new()
      .max_connections(5)
      .connect(&url)
      .await
      .map_err(|source| DbError::Connect {
        path: path.to_string(),
        source,
      })?;

    run_migrations(&pool).await.map_err(DbError::Migration)?;

    Ok(Db(pool))
  }

  pub fn pool(&self) -> &SqlitePool {
    &self.0
  }
}

/// Split SQL into statements, respecting BEGIN/END blocks (used by
/// CREATE TRIGGER).  Naive `;` splitting breaks trigger bodies because
/// they contain embedded semicolons.
fn split_sql_statements(sql: &str) -> Vec<String> {
  let mut stmts = Vec::new();
  let mut current = String::new();
  let mut in_trigger = false;

  for line in sql.lines() {
    let trimmed = line.trim();

    // Skip empty lines and comments.
    if trimmed.is_empty() || trimmed.starts_with("--") {
      continue;
    }

    let upper = trimmed.to_uppercase();

    // Detect BEGIN (start of trigger body).
    if upper.ends_with("BEGIN") {
      in_trigger = true;
    }

    current.push_str(line);
    current.push('\n');

    // Inside a trigger: look for END; to close the block.
    if in_trigger {
      if upper.starts_with("END") {
        in_trigger = false;
        let stmt = current.trim().trim_end_matches(';').to_string();
        if !stmt.is_empty() {
          stmts.push(stmt);
        }
        current.clear();
      }
      continue;
    }

    // Outside a trigger: semicolons terminate statements.
    if trimmed.ends_with(';') {
      let stmt = current.trim().trim_end_matches(';').to_string();
      if !stmt.is_empty() {
        stmts.push(stmt);
      }
      current.clear();
    }
  }

  // Trailing statement without a semicolon.
  let tail = current.trim().to_string();
  if !tail.is_empty() {
    stmts.push(tail);
  }

  stmts
}

async fn run_migrations(pool: &SqlitePool) -> Result<(), sqlx::Error> {
  let sql = include_str!("../migrations/001_initial.sql");
  for stmt in split_sql_statements(sql) {
    sqlx::query(&stmt).execute(pool).await?;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_split_handles_triggers() {
    let sql = "\
CREATE TABLE t (id INT);

CREATE TRIGGER tr AFTER INSERT ON t BEGIN
  INSERT INTO log VALUES (new.id);
END;

CREATE TABLE t2 (id INT);";

    let stmts = split_sql_statements(sql);
    assert_eq!(stmts.len(), 3);
    assert!(stmts[0].contains("CREATE TABLE t"));
    assert!(stmts[1].contains("CREATE TRIGGER"));
    assert!(stmts[1].contains("END"));
    assert!(stmts[2].contains("CREATE TABLE t2"));
  }

  #[test]
  fn test_split_handles_unicode_comments() {
    let sql = "\
-- ── Header ───────────────────────────────
PRAGMA journal_mode = WAL;

CREATE TABLE t (id INT);";

    let stmts = split_sql_statements(sql);
    assert_eq!(stmts.len(), 2);
    assert!(stmts[0].contains("PRAGMA"));
    assert!(stmts[1].contains("CREATE TABLE"));
  }
}
