use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
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

    run_migrations(&pool)
      .await
      .map_err(DbError::Migration)?;

    Ok(Db(pool))
  }

  pub fn pool(&self) -> &SqlitePool {
    &self.0
  }
}

async fn run_migrations(pool: &SqlitePool) -> Result<(), sqlx::Error> {
  let sql = include_str!("../migrations/001_initial.sql");
  // SQLite requires statements to be executed one at a time.
  for stmt in sql.split(';') {
    let stmt = stmt.trim();
    if !stmt.is_empty() {
      sqlx::query(stmt).execute(pool).await?;
    }
  }
  Ok(())
}
