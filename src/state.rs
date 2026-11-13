use crate::{config::Config, events::EventHub, settings::SettingsStore};
use axum::http::{header, HeaderMap};
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cfg: Arc<Config>,
    pub settings: SettingsStore,
    /// Outbound HTTP (Web Push). rustls only.
    pub http: reqwest::Client,
    /// Realtime change fan-out (SSE). See events.rs.
    pub events: EventHub,
    /// Graceful-shutdown signal. Workers: `tokio::select! { _ = sleep(..) => .., _ = state.shutdown.wait() => break }`.
    pub shutdown: Shutdown,
}

#[derive(Clone)]
pub struct Shutdown(Arc<watch::Sender<bool>>);
impl Shutdown {
    pub fn new() -> Self { Shutdown(Arc::new(watch::channel(false).0)) }
    pub fn trigger(&self) { self.0.send_replace(true); }
    pub fn is_triggered(&self) -> bool { *self.0.borrow() }
    /// Resolves when shutdown has been requested.
    pub async fn wait(&self) {
        let mut rx = self.0.subscribe();
        while !*rx.borrow_and_update() { if rx.changed().await.is_err() { return; } }
    }
}
impl Default for Shutdown { fn default() -> Self { Self::new() } }

impl AppState {
    /// Base URL embedded in QR labels: settings override -> --domain -> request Host header -> localhost.
    pub fn base_url(&self, headers: &HeaderMap) -> String {
        let s = self.settings.get();
        let o = s.qr_base_url.trim().trim_end_matches('/');
        if !o.is_empty() { return o.to_string(); }
        let scheme = if self.cfg.insecure_http { "http" } else { "https" };
        if let Some(d) = &self.cfg.domain {
            let port = self.cfg.bind.port();
            return if port == 443 || self.cfg.mode == crate::config::Mode::Domain { format!("https://{d}") } else { format!("{scheme}://{d}:{port}") };
        }
        if let Some(h) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) { return format!("{scheme}://{h}"); }
        format!("{scheme}://localhost:{}", self.cfg.bind.port())
    }
}
