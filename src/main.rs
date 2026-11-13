use anyhow::Context;
use clap::Parser;
use water_erp::{config::{Cli, Command, Config, Mode}, push, routes, seed, server, tls, workers};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,sqlx=warn,tower_http=info".into()))
        .init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();

    if let Some(Command::GenVapid) = cli.command { return push::print_new_vapid_keys(); }
    if let Some(Command::SeedDemo) = cli.command {
        let url = cli.database_url.clone().context("DATABASE_URL required")?;
        let pool = server::connect_db(&url).await?;
        server::migrate(&pool).await?;
        return seed::run(&pool).await;
    }

    let cfg = Config::from_cli(&cli)?;
    let pool = server::connect_db(&cfg.database_url).await.context("connecting to PostgreSQL")?;
    server::migrate(&pool).await.context("running migrations")?;
    let state = server::build_state(cfg.clone(), pool).await?;

    if let Some(user) = &cfg.setup_admin {
        let pw = std::env::var("ERP_ADMIN_PASSWORD").context("--setup-admin needs ERP_ADMIN_PASSWORD in the environment")?;
        water_erp::routes::admin::bootstrap_admin(&state.db, user, &pw).await?;
    }

    workers::spawn_all(state.clone());
    let app = routes::build_router(state.clone());
    let shutdown = state.shutdown.clone();
    tokio::spawn({
        let shutdown = shutdown.clone();
        async move { wait_for_signal().await; tracing::info!("shutdown signal received: draining"); shutdown.trigger(); }
    });

    if cfg.insecure_http {
        tracing::warn!("serving PLAIN HTTP on {} (insecure-http)", cfg.bind);
        let l = tokio::net::TcpListener::bind(cfg.bind).await?;
        let sd = shutdown.clone();
        axum::serve(l, app).with_graceful_shutdown(async move { sd.wait().await }).await?;
    } else {
        let rustls_cfg = tls::rustls_config(&cfg).await?;
        tracing::info!("serving HTTPS on {} ({:?} mode)", cfg.bind, if cfg.mode == Mode::Local { "local" } else { "domain" });
        let handle = axum_server::Handle::new();
        let (h, sd) = (handle.clone(), shutdown.clone());
        tokio::spawn(async move { sd.wait().await; h.graceful_shutdown(Some(std::time::Duration::from_secs(25))); });
        axum_server::bind_rustls(cfg.bind, rustls_cfg).handle(handle).serve(app.into_make_service()).await?;
    }
    // Order: stop accepting + drain requests (above) -> let workers notice the signal -> close DB pool.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    state.db.close().await;
    tracing::info!("shutdown complete");
    Ok(())
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    { let _ = tokio::signal::ctrl_c().await; }
}
