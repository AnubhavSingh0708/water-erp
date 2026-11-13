//! OWNER: A. Deletes expired sessions and sessions untouched for `auth.session_inactivity_purge_days`.
use crate::state::AppState;
use sqlx::PgPool;
use std::time::Duration;

const INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Delete sessions that expired or have not been used for `days` days. Returns the number removed.
pub async fn purge_once(db: &PgPool, days: i64) -> sqlx::Result<u64> {
    let days = days.clamp(1, 3650) as i32;
    let r = sqlx::query("DELETE FROM user_sessions WHERE expires_at < now() OR last_used_at < now() - make_interval(days => $1)")
        .bind(days).execute(db).await?;
    Ok(r.rows_affected())
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        loop {
            // setting is read live every round
            let days = state.settings.get().session_inactivity_purge_days;
            match purge_once(&state.db, days).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(removed = n, "purged stale sessions"),
                Err(e) => tracing::warn!(error = %e, "session purge failed"),
            }
            tokio::select! {
                _ = tokio::time::sleep(INTERVAL) => {}
                _ = state.shutdown.wait() => break,
            }
        }
    });
}
