//! TLS (pure-Rust rustls). OWNER: Subagent A.
//! domain mode: load --tls-cert/--tls-key. local mode: use them if given, otherwise generate (and persist under
//! `<data_dir>/tls/`) a local CA + server certificate with rcgen covering localhost, 127.0.0.1, --domain and LAN IPs.
use crate::config::Config;
use axum_server::tls_rustls::RustlsConfig;

pub async fn rustls_config(cfg: &Config) -> anyhow::Result<RustlsConfig> {
    match (&cfg.tls_cert, &cfg.tls_key) {
        (Some(c), Some(k)) => Ok(RustlsConfig::from_pem_file(c, k).await?),
        _ => anyhow::bail!("TODO(A): local-mode certificate generation not implemented yet"),
    }
}
