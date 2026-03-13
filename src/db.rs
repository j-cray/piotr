use sqlx::sqlite::{SqlitePool, SqlitePoolOptions, SqliteConnectOptions};
use anyhow::Result;
use tracing::info;
use std::str::FromStr;

pub struct Database {
    pub pool: SqlitePool,
}

impl Database {
    pub async fn new(database_url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        Ok(Self { pool })
    }

    pub async fn run_migrations(&self) -> Result<()> {
        info!("Running database migrations...");
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await?;
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
        assert!(migration_result.is_ok(), "Failed to run migrations: {:?}", migration_result.err());

        assert!(!db.pool.is_closed(), "Database pool should not be closed");
    }

    #[tokio::test]
    async fn test_database_connection_invalid_url() {
        let invalid_url = "not_a_sqlite_url://localhost/mydb";
        let result = Database::new(invalid_url).await;

        assert!(result.is_err(), "Expected an error for malformed URL scheme");
    }
}
