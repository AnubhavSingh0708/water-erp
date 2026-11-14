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

/// A listening socket with `SO_KEEPALIVE` switched on. Accepted connections inherit it (Linux), so the kernel probes idle peers using the
/// host's `net.ipv4.tcp_keepalive_time/intvl/probes` (set `tcp_keepalive_time=60` in the unit/sysctl): a phone that left the Wi-Fi without
/// closing the socket (an SSE stream, a half-open request) is reaped in about a minute instead of holding a slot until TCP gives up.
/// Without this call those sysctls have no effect on our sockets. (Per-socket intervals would need `socket2`, which is not a direct dependency.)
pub fn new_listen_socket(addr: std::net::SocketAddr) -> std::io::Result<tokio::net::TcpSocket> {
    let sock = if addr.is_ipv4() { tokio::net::TcpSocket::new_v4()? } else { tokio::net::TcpSocket::new_v6()? };
    #[cfg(unix)]
    sock.set_reuseaddr(true)?;
    sock.set_keepalive(true)?;
    sock.bind(addr)?;
    Ok(sock)
}

/// Bind + listen (backlog 1024) with keep-alive probing enabled; see `new_listen_socket`.
pub fn bind_listener(addr: std::net::SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    new_listen_socket(addr)?.listen(1024)
}

pub async fn build_state(cfg: Config, pool: PgPool) -> anyhow::Result<AppState> {
    let settings = SettingsStore::load(&pool).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(AppState { db: pool, cfg: Arc::new(cfg), settings, http: http_client(), events: crate::events::EventHub::new(), shutdown: crate::state::Shutdown::new() })
}
