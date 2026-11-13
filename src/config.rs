//! Server start-up flags (clap). Everything operational that may change live is in `system_settings` (see settings.rs).
use clap::{Parser, Subcommand, ValueEnum};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode { Local, Domain }

#[derive(Parser, Debug, Clone)]
#[command(name = "water-erp", version, about = "Packaged mineral water plant ERP")]
pub struct Cli {
    /// `local`: plant LAN (self-signed/local-CA TLS auto-generated unless --tls-cert/--tls-key). `domain`: public HTTPS (needs --domain, --tls-cert, --tls-key).
    #[arg(long, value_enum, env = "ERP_MODE")]
    pub mode: Option<Mode>,
    /// Public host name (domain mode, required) or LAN host/IP override (local mode): used in QR URLs, VAPID subject, Origin checks.
    #[arg(long, env = "ERP_DOMAIN")]
    pub domain: Option<String>,
    #[arg(long, env = "ERP_BIND", default_value = "0.0.0.0:8443")]
    pub bind: SocketAddr,
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,
    #[arg(long, env = "ERP_TLS_CERT")]
    pub tls_cert: Option<PathBuf>,
    #[arg(long, env = "ERP_TLS_KEY")]
    pub tls_key: Option<PathBuf>,
    #[arg(long, env = "ERP_STATIC_DIR", default_value = "static")]
    pub static_dir: PathBuf,
    /// Where auto-generated local TLS material is kept (local mode).
    #[arg(long, env = "ERP_DATA_DIR", default_value = "data")]
    pub data_dir: PathBuf,
    /// VAPID private key: base64url of the raw 32-byte P-256 scalar (see `gen-vapid`).
    #[arg(long, env = "VAPID_PRIVATE_KEY", hide_env_values = true)]
    pub vapid_private_key: Option<String>,
    /// VAPID public key: base64url of the 65-byte uncompressed P-256 point.
    #[arg(long, env = "VAPID_PUBLIC_KEY")]
    pub vapid_public_key: Option<String>,
    /// VAPID subject (mailto: or https: URL). Defaults to https://<domain>.
    #[arg(long, env = "VAPID_SUBJECT")]
    pub vapid_subject: Option<String>,
    /// Create the first Admin with this username if no Admin exists. Password is read from env ERP_ADMIN_PASSWORD (never a flag).
    #[arg(long)]
    pub setup_admin: Option<String>,
    /// Serve plain HTTP (local mode only; for development/tests behind a trusted proxy).
    #[arg(long, hide = true)]
    pub insecure_http: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Print a fresh VAPID key pair (export as VAPID_PRIVATE_KEY / VAPID_PUBLIC_KEY).
    GenVapid,
    /// Insert demo master data + demo users (requires DATABASE_URL). Development only.
    SeedDemo,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub mode: Mode,
    pub domain: Option<String>,
    pub bind: SocketAddr,
    pub database_url: String,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub static_dir: PathBuf,
    pub data_dir: PathBuf,
    pub vapid_private_key: Option<String>,
    pub vapid_public_key: Option<String>,
    pub vapid_subject: String,
    pub setup_admin: Option<String>,
    pub insecure_http: bool,
}

impl Config {
    pub fn from_cli(cli: &Cli) -> anyhow::Result<Config> {
        let mode = cli.mode.ok_or_else(|| anyhow::anyhow!("--mode <local|domain> is required"))?;
        let database_url = cli.database_url.clone().ok_or_else(|| anyhow::anyhow!("--database-url (or DATABASE_URL) is required"))?;
        if mode == Mode::Domain {
            if cli.domain.is_none() { anyhow::bail!("--domain is required in domain mode"); }
            if cli.tls_cert.is_none() || cli.tls_key.is_none() { anyhow::bail!("--tls-cert and --tls-key are required in domain mode"); }
            if cli.insecure_http { anyhow::bail!("--insecure-http is not allowed in domain mode"); }
        }
        if cli.tls_cert.is_some() != cli.tls_key.is_some() { anyhow::bail!("--tls-cert and --tls-key must be given together"); }
        let host = cli.domain.clone().unwrap_or_else(|| "localhost".into());
        let vapid_subject = cli.vapid_subject.clone().unwrap_or_else(|| format!("https://{host}"));
        Ok(Config {
            mode, domain: cli.domain.clone(), bind: cli.bind, database_url,
            tls_cert: cli.tls_cert.clone(), tls_key: cli.tls_key.clone(),
            static_dir: cli.static_dir.clone(), data_dir: cli.data_dir.clone(),
            vapid_private_key: cli.vapid_private_key.clone(), vapid_public_key: cli.vapid_public_key.clone(),
            vapid_subject, setup_admin: cli.setup_admin.clone(), insecure_http: cli.insecure_http,
        })
    }
}
