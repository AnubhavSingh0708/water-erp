use crate::{config::Config, settings::SettingsStore};
use axum::http::{header, HeaderMap};
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cfg: Arc<Config>,
    pub settings: SettingsStore,
    /// Outbound HTTP (Web Push). rustls only.
    pub http: reqwest::Client,
}

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
