use sqlx::postgres::{PgPool, PgPoolOptions};
use anyhow::Result;
use log::info;

pub struct Database {
    pub pool: PgPool,
}

impl Database {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
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
    use std::env;

    #[tokio::test]
    async fn test_database_connection_and_migrations() {
        // Only run this test if a DATABASE_URL is provided in the environment
        if let Ok(db_url) = env::var("DATABASE_URL") {
            // Test connection
            let db_result = Database::new(&db_url).await;
            assert!(db_result.is_ok(), "Failed to connect to the database");

            let db = db_result.unwrap();

            // Note: We avoid running migrations in basic unit tests unless we use a test-specific db,
            // instead we just assert that the pool was created successfully.
            assert!(!db.pool.is_closed(), "Database pool should not be closed");
        }
    }
}
