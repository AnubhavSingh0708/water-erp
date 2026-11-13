use crate::{config::Config, settings::SettingsStore, state::AppState};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{sync::Arc, time::Duration};

/// Connect with retry/backoff (up to ~60 s) so the service survives starting before PostgreSQL is ready.
pub async fn connect_db(url: &str) -> anyhow::Result<PgPool> {
    let mut delay = Duration::from_millis(500);
    let mut last = None;
    for attempt in 1..=12 {
        match PgPoolOptions::new().max_connections(20).acquire_timeout(Duration::from_secs(10)).connect(url).await {
            Ok(p) => return Ok(p),
            Err(e) => {
                tracing::warn!(attempt, error = %e, "database not reachable yet");
                last = Some(e);
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(8));
            }
        }
    }
    Err(anyhow::anyhow!("could not connect to PostgreSQL: {}", last.unwrap()))
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(15)).user_agent("water-erp-push/1").build().expect("http client")
}

pub async fn build_state(cfg: Config, pool: PgPool) -> anyhow::Result<AppState> {
    let settings = SettingsStore::load(&pool).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(AppState { db: pool, cfg: Arc::new(cfg), settings, http: http_client(), events: crate::events::EventHub::new(), shutdown: crate::state::Shutdown::new() })
}
