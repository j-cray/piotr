use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::fs;
use std::path::Path;
use std::str::FromStr;
use tracing::info;

pub struct Database {
    pub pool: SqlitePool,
}

impl Database {
    pub async fn new(database_url: &str) -> Result<Self> {
        // For file-based SQLite URLs (e.g., "sqlite://data/piotr.db"), ensure
        // the parent directory exists before attempting to create/open the DB.
        if let Some(path_str) = database_url.strip_prefix("sqlite://") {
            // Ignore in-memory and other non-file special cases that would not
            // correspond to a filesystem path.
            if !path_str.is_empty() && !path_str.starts_with(':') {
                let path = Path::new(path_str);
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        fs::create_dir_all(parent)?;
                    }
                }
            }
        }

        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        Ok(Self { pool })
    }

    pub async fn run_migrations(&self) -> Result<()> {
        info!("Running database migrations...");
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        info!("Migrations completed.");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_database_connection_and_migrations() {
        let db_result = Database::new("sqlite::memory:").await;
        assert!(db_result.is_ok(), "Failed to connect to the database");

        let db = db_result.unwrap();

        // With an in-memory database, we can and should test the migrations.
        let migration_result = db.run_migrations().await;
        assert!(
            migration_result.is_ok(),
            "Failed to run migrations: {:?}",
            migration_result.err()
        );

        assert!(!db.pool.is_closed(), "Database pool should not be closed");
    }

    #[tokio::test]
    async fn test_database_connection_invalid_url() {
        let invalid_url = "not_a_sqlite_url://localhost/mydb";
        let result = Database::new(invalid_url).await;

        assert!(
            result.is_err(),
            "Expected an error for malformed URL scheme"
        );
    }
}
