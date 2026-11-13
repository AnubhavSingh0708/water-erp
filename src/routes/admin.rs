use crate::state::AppState;
use axum::Router;
pub fn router() -> Router<AppState> { Router::new() }

/// Create the first Admin if none exists (CLI `--setup-admin`). OWNER: A.
pub async fn bootstrap_admin(_db: &sqlx::PgPool, _username: &str, _password: &str) -> anyhow::Result<()> {
    anyhow::bail!("TODO(A): bootstrap_admin not implemented yet")
}
