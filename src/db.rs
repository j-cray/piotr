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
