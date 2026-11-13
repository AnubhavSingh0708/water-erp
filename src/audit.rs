use crate::auth::AuthUser;
use serde_json::Value;
use sqlx::PgExecutor;

/// Append an immutable audit record. Failures are logged, never propagated (auditing must not break the operation).
pub async fn log<'e>(db: impl PgExecutor<'e>, user: Option<&AuthUser>, action: &str, entity: &str, entity_id: Option<String>, details: Value) {
    let r = sqlx::query("INSERT INTO audit_logs (user_id, username, action, entity, entity_id, details) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(user.map(|u| u.id)).bind(user.map(|u| u.username.clone()))
        .bind(action).bind(entity).bind(entity_id).bind(details)
        .execute(db).await;
    if let Err(e) = r { tracing::error!(error = ?e, action, "audit insert failed"); }
}
