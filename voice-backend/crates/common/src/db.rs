//! PostgreSQL connection pool and embedded migrations.

use std::time::{Duration, Instant};

use sqlx::{migrate::Migrator, postgres::PgPoolOptions, PgPool};

use crate::config::DatabaseConfig;

/// Migrations from the workspace `migrations/` directory, embedded at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Opens the connection pool and verifies one connection.
pub async fn connect(cfg: &DatabaseConfig) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(cfg.url.expose())
        .await
}

/// Opens the pool, retrying for up to `wait` while the database starts.
pub async fn connect_with_retry(
    cfg: &DatabaseConfig,
    wait: Duration,
) -> Result<PgPool, sqlx::Error> {
    let deadline = Instant::now() + wait;
    loop {
        match connect(cfg).await {
            Ok(pool) => return Ok(pool),
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(error = %e, "database not ready, retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Cheap readiness probe.
pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1").execute(pool).await.map(|_| ())
}
