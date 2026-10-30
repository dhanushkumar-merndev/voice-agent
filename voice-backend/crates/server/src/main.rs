//! `voice-server` entry point: loads config, connects to Postgres and object
//! storage, runs migrations and serves HTTP until shutdown.

use std::time::Duration;

use anyhow::Context;
use common::{assets::Assets, storage::Storage, Config};
use mimalloc::MiMalloc;
use server::{app::build_router, bootstrap, observability};
use tokio::net::TcpListener;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// How long to wait for the database at startup (container may still be booting).
const DB_STARTUP_WAIT: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Finds the shared ../.env (dotenvy searches parent folders). A missing
    // file is fine: production passes real environment variables.
    let _ = dotenvy::dotenv();
    // Several crates pull in rustls; pick one crypto provider for the process.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = Config::from_env()?;
    observability::init_tracing(config.log_format);
    let metrics = observability::install_metrics().context("installing metrics recorder")?;
    observability::spawn_metrics_upkeep(metrics.clone());

    let assets = Assets::load(&config.paths.prompts_dir, &config.paths.config_dir)
        .context("loading prompts and config assets")?;
    warn_missing_providers(&config);

    let db = common::db::connect_with_retry(&config.database, DB_STARTUP_WAIT)
        .await
        .context("connecting to database")?;
    common::db::MIGRATOR
        .run(&db)
        .await
        .context("running database migrations")?;

    let storage = Storage::new(&config.s3);
    if config.is_local() {
        if let Err(e) = storage.ensure_bucket().await {
            tracing::warn!(error = %e, bucket = storage.bucket(), "could not create local bucket");
        }
    }
    if let Err(e) = storage.ping().await {
        tracing::warn!(error = %e, bucket = storage.bucket(), "object storage not reachable yet");
    }

    let grace = Duration::from_secs(config.limits.shutdown_grace_secs);
    let addr = config.bind_addr;
    let state = bootstrap::build_state(config, assets, db, metrics).await;
    bootstrap::spawn_housekeeping(&state);

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(
        %addr,
        env = ?state.config.app_env,
        providers = ?state.config.provider_mode,
        vad = state.vad.is_some(),
        knowledge_base = state.kb.available(),
        plivo = state.plivo.is_some(),
        public_base_url = %state.config.public_base_url,
        "voice-server listening"
    );

    let shutdown = state.lifecycle.clone().drain_on_signal(grace);
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown)
        .await
        .context("http server")?;

    tracing::info!("shutdown complete");
    Ok(())
}

/// Logs which provider features are unavailable because keys are missing.
fn warn_missing_providers(config: &Config) {
    let plivo = &config.plivo;
    if plivo.auth_id.is_none() || plivo.auth_token.is_none() || plivo.from_number.is_none() {
        tracing::warn!(
            "PLIVO_AUTH_ID / PLIVO_AUTH_TOKEN / PLIVO_FROM_NUMBER not set: real calls disabled"
        );
    }
    if config.sarvam.api_key.is_none()
        && config.provider_mode == common::config::ProviderMode::Sarvam
    {
        tracing::warn!("SARVAM_API_KEY not set: STT, TTS and LLM disabled");
    }
}
