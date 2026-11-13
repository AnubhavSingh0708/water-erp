//! Admin: users + sessions (API_REFERENCE §2).
use super::common::{audit, check_locations, check_pages, check_password_policy, check_subscriptions, check_username, hash_password, ApiJson, ApiPath, ApiQuery, ClientInfo};
use crate::{auth::AuthUser, error::{AppError, AppResult}, state::AppState, validate};
use axum::{extract::State, http::StatusCode, routing::{delete, get, post}, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/users", get(list_users).post(create_user))
        .route("/users/{id}", get(get_user).put(update_user).delete(delete_user))
        .route("/users/{id}/password", post(reset_password))
        .route("/users/{id}/unlock", post(unlock_user))
        .route("/users/{id}/revoke-sessions", post(revoke_user_sessions))
        .route("/sessions", get(list_sessions))
        .route("/sessions/revoke-all", post(revoke_all))
        .route("/sessions/{id}", delete(revoke_session))
}

#[derive(Serialize, sqlx::FromRow)]
struct UserObj {
    id: Uuid,
    username: String,
    full_name: String,
    is_admin: bool,
    is_active: bool,
    allowed_pages: Value,
    subscribed_notifications: Value,
    allowed_location_ids: Value,
    failed_login_count: i32,
    locked_until: Option<DateTime<Utc>>,
    is_locked: bool,
    last_login_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    active_sessions: i64,
}

const USER_SELECT: &str = "SELECT u.id, u.username, u.full_name, u.is_admin, u.is_active, u.allowed_pages, u.subscribed_notifications, u.allowed_location_ids,
        u.failed_login_count, u.locked_until, COALESCE(u.locked_until > now(), false) AS is_locked, u.last_login_at, u.created_at, u.updated_at,
        (SELECT count(*) FROM user_sessions s WHERE s.user_id = u.id AND s.expires_at > now()) AS active_sessions
    FROM users u";

async fn fetch_user(conn: &mut PgConnection, id: Uuid) -> AppResult<UserObj> {
    sqlx::query_as::<_, UserObj>(&format!("{USER_SELECT} WHERE u.id = $1")).bind(id).fetch_optional(&mut *conn).await?
        .ok_or_else(|| AppError::not_found("User"))
}

#[derive(Deserialize)]
struct ListQuery { q: Option<String>, active: Option<bool> }

async fn list_users(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQuery>) -> AppResult<Json<Vec<UserObj>>> {
    let pat = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(validate::like_pattern(&validate::text("q", s, 100)?)),
        None => None,
    };
    let rows = sqlx::query_as::<_, UserObj>(&format!(
        "{USER_SELECT} WHERE ($1::text IS NULL OR u.username ILIKE $1 OR u.full_name ILIKE $1)
           AND ($2::bool IS NULL OR u.is_active = $2) ORDER BY lower(u.username) LIMIT 500"))
        .bind(pat).bind(q.active).fetch_all(&st.db).await?;
    Ok(Json(rows))
}

async fn get_user(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<UserObj>> {
    let mut c = st.db.acquire().await?;
    Ok(Json(fetch_user(&mut c, id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateUser {
    username: String,
    full_name: String,
    password: String,
    #[serde(default)] is_admin: bool,
    #[serde(default)] allowed_pages: Vec<String>,
    #[serde(default)] subscribed_notifications: Vec<String>,
    #[serde(default)] allowed_location_ids: Vec<Uuid>,
}

async fn create_user(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiJson(b): ApiJson<CreateUser>) -> AppResult<(StatusCode, Json<UserObj>)> {
    let username = check_username("username", &b.username)?;
    let full_name = validate::required_text("full_name", &b.full_name, 120)?;
    check_password_policy("password", &b.password, &username)?;
    let pages = check_pages("allowed_pages", &b.allowed_pages)?;
    let subs = check_subscriptions("subscribed_notifications", &b.subscribed_notifications)?;
    let hash = hash_password(&b.password).await?;
    let mut tx = st.db.begin().await?;
    let locs = check_locations(&mut tx, "allowed_location_ids", &b.allowed_location_ids).await?;
    let id: Uuid = match sqlx::query_scalar(
        "INSERT INTO users (username, full_name, password_hash, is_admin, allowed_pages, subscribed_notifications, allowed_location_ids) VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id")
        .bind(&username).bind(&full_name).bind(&hash).bind(b.is_admin).bind(json!(pages)).bind(json!(subs)).bind(json!(locs)).fetch_one(&mut *tx).await {
        Ok(id) => id,
        Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("23505") => return Err(AppError::field("username", "is already taken")),
        Err(e) => return Err(e.into()),
    };
    audit(&mut tx, &actor, &ci, "USER_CREATE", "user", Some(id.to_string()),
          json!({ "username": username, "is_admin": b.is_admin, "allowed_pages": pages, "subscribed_notifications": subs, "allowed_location_ids": locs })).await?;
    let u = fetch_user(&mut tx, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(u)))
}

/// Locks every active admin row (stable order) and fails with LAST_ADMIN if `target` is the only one.
async fn ensure_not_last_admin(tx: &mut Transaction<'_, Postgres>, target: Uuid) -> AppResult<()> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE is_admin AND is_active ORDER BY id FOR UPDATE").fetch_all(&mut **tx).await?;
    if ids.iter().all(|i| *i == target) {
        return Err(AppError::conflict("LAST_ADMIN", "This is the last active administrator. Create or promote another administrator first."));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateUser {
    full_name: Option<String>,
    is_admin: Option<bool>,
    is_active: Option<bool>,
    allowed_pages: Option<Vec<String>>,
    subscribed_notifications: Option<Vec<String>>,
    allowed_location_ids: Option<Vec<Uuid>>,
}

async fn update_user(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<UpdateUser>) -> AppResult<Json<UserObj>> {
    let full_name = b.full_name.as_deref().map(|s| validate::required_text("full_name", s, 120)).transpose()?;
    let pages = b.allowed_pages.as_deref().map(|p| check_pages("allowed_pages", p)).transpose()?;
    let subs = b.subscribed_notifications.as_deref().map(|p| check_subscriptions("subscribed_notifications", p)).transpose()?;
    let mut tx = st.db.begin().await?;
    // unlocked peek decides whether the admin set must be locked (always before the target row => stable lock order)
    let peek: Option<(bool, bool)> = sqlx::query_as("SELECT is_admin, is_active FROM users WHERE id = $1").bind(id).fetch_optional(&mut *tx).await?;
    let (peek_admin, peek_active) = peek.ok_or_else(|| AppError::not_found("User"))?;
    if peek_admin && peek_active { sqlx::query("SELECT id FROM users WHERE is_admin AND is_active ORDER BY id FOR UPDATE").execute(&mut *tx).await?; }
    let before = fetch_locked(&mut tx, id).await?;
    let locs = match b.allowed_location_ids.as_deref() { Some(l) => Some(check_locations(&mut tx, "allowed_location_ids", l).await?), None => None };
    let new_admin = b.is_admin.unwrap_or(before.is_admin);
    let new_active = b.is_active.unwrap_or(before.is_active);
    if before.is_admin && before.is_active && (!new_admin || !new_active) { ensure_not_last_admin(&mut tx, id).await?; }
    sqlx::query("UPDATE users SET full_name = COALESCE($2, full_name), is_admin = $3, is_active = $4,
                                  allowed_pages = COALESCE($5, allowed_pages), subscribed_notifications = COALESCE($6, subscribed_notifications),
                                  allowed_location_ids = COALESCE($7, allowed_location_ids), updated_at = now()
                 WHERE id = $1")
        .bind(id).bind(&full_name).bind(new_admin).bind(new_active).bind(pages.as_ref().map(|p| json!(p))).bind(subs.as_ref().map(|p| json!(p))).bind(locs.as_ref().map(|p| json!(p)))
        .execute(&mut *tx).await?;
    let mut revoked = 0;
    if before.is_active && !new_active {
        revoked = sqlx::query("DELETE FROM user_sessions WHERE user_id = $1").bind(id).execute(&mut *tx).await?.rows_affected();
    }
    let after = fetch_user(&mut tx, id).await?;
    audit(&mut tx, &actor, &ci, "USER_UPDATE", "user", Some(id.to_string()), json!({
        "username": after.username,
        "before": { "full_name": before.full_name, "is_admin": before.is_admin, "is_active": before.is_active, "allowed_pages": before.allowed_pages, "subscribed_notifications": before.subs, "allowed_location_ids": before.locs },
        "after": { "full_name": after.full_name, "is_admin": after.is_admin, "is_active": after.is_active, "allowed_pages": after.allowed_pages, "subscribed_notifications": after.subscribed_notifications, "allowed_location_ids": after.allowed_location_ids },
        "sessions_revoked": revoked,
    })).await?;
    tx.commit().await?;
    Ok(Json(after))
}

struct Locked { username: String, full_name: String, is_admin: bool, is_active: bool, allowed_pages: Value, subs: Value, locs: Value }
async fn fetch_locked(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> AppResult<Locked> {
    let r: Option<(String, String, bool, bool, Value, Value, Value)> = sqlx::query_as(
        "SELECT username, full_name, is_admin, is_active, allowed_pages, subscribed_notifications, allowed_location_ids FROM users WHERE id = $1 FOR UPDATE")
        .bind(id).fetch_optional(&mut **tx).await?;
    let r = r.ok_or_else(|| AppError::not_found("User"))?;
    Ok(Locked { username: r.0, full_name: r.1, is_admin: r.2, is_active: r.3, allowed_pages: r.4, subs: r.5, locs: r.6 })
}

async fn delete_user(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    let mut tx = st.db.begin().await?;
    let peek: Option<(bool, bool)> = sqlx::query_as("SELECT is_admin, is_active FROM users WHERE id = $1").bind(id).fetch_optional(&mut *tx).await?;
    let (peek_admin, peek_active) = peek.ok_or_else(|| AppError::not_found("User"))?;
    if peek_admin && peek_active { sqlx::query("SELECT id FROM users WHERE is_admin AND is_active ORDER BY id FOR UPDATE").execute(&mut *tx).await?; }
    let u = fetch_locked(&mut tx, id).await?;
    if u.is_admin && u.is_active { ensure_not_last_admin(&mut tx, id).await?; }
    sqlx::query("UPDATE users SET is_active = false, updated_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
    let revoked = sqlx::query("DELETE FROM user_sessions WHERE user_id = $1").bind(id).execute(&mut *tx).await?.rows_affected();
    audit(&mut tx, &actor, &ci, "USER_DEACTIVATE", "user", Some(id.to_string()), json!({ "username": u.username, "sessions_revoked": revoked })).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetPassword { new_password: String }

async fn reset_password(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<ResetPassword>) -> AppResult<StatusCode> {
    let username: String = sqlx::query_scalar("SELECT username FROM users WHERE id = $1").bind(id).fetch_optional(&st.db).await?
        .ok_or_else(|| AppError::not_found("User"))?;
    check_password_policy("new_password", &b.new_password, &username)?;
    let hash = hash_password(&b.new_password).await?;
    let mut tx = st.db.begin().await?;
    let r = sqlx::query("UPDATE users SET password_hash = $2, failed_login_count = 0, locked_until = NULL, updated_at = now() WHERE id = $1")
        .bind(id).bind(&hash).execute(&mut *tx).await?;
    if r.rows_affected() == 0 { return Err(AppError::not_found("User")); }
    // everything the old password opened is closed (the admin's own current session is kept when resetting themselves)
    let revoked = sqlx::query("DELETE FROM user_sessions WHERE user_id = $1 AND id <> $2").bind(id).bind(if id == actor.id { actor.session_id } else { Uuid::nil() })
        .execute(&mut *tx).await?.rows_affected();
    audit(&mut tx, &actor, &ci, "USER_PASSWORD_RESET", "user", Some(id.to_string()), json!({ "username": username, "sessions_revoked": revoked })).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unlock_user(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<UserObj>> {
    let mut tx = st.db.begin().await?;
    let username: Option<String> = sqlx::query_scalar("UPDATE users SET failed_login_count = 0, locked_until = NULL, updated_at = now() WHERE id = $1 RETURNING username")
        .bind(id).fetch_optional(&mut *tx).await?;
    let username = username.ok_or_else(|| AppError::not_found("User"))?;
    audit(&mut tx, &actor, &ci, "USER_UNLOCK", "user", Some(id.to_string()), json!({ "username": username })).await?;
    let u = fetch_user(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(u))
}

async fn revoke_user_sessions(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<Value>> {
    let mut tx = st.db.begin().await?;
    let username: Option<String> = sqlx::query_scalar("SELECT username FROM users WHERE id = $1").bind(id).fetch_optional(&mut *tx).await?;
    let username = username.ok_or_else(|| AppError::not_found("User"))?;
    let n = sqlx::query("DELETE FROM user_sessions WHERE user_id = $1").bind(id).execute(&mut *tx).await?.rows_affected();
    audit(&mut tx, &actor, &ci, "SESSIONS_REVOKE_USER", "user", Some(id.to_string()), json!({ "username": username, "revoked": n })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "revoked": n })))
}

// ---------------------------------------------------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Serialize, sqlx::FromRow)]
struct SessionObj {
    id: Uuid,
    user_id: Uuid,
    username: String,
    created_at: DateTime<Utc>,
    last_used_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    user_agent: Option<String>,
    ip: Option<String>,
    allowed_location_ids: Value,
    is_current: bool,
}

#[derive(Deserialize)]
struct SessionQuery { user_id: Option<Uuid> }

async fn list_sessions(State(st): State<AppState>, actor: AuthUser, ApiQuery(q): ApiQuery<SessionQuery>) -> AppResult<Json<Vec<SessionObj>>> {
    let rows = sqlx::query_as::<_, SessionObj>(
        "SELECT s.id, s.user_id, u.username, s.created_at, s.last_used_at, s.expires_at, s.user_agent, s.ip, u.allowed_location_ids, (s.id = $1) AS is_current
         FROM user_sessions s JOIN users u ON u.id = s.user_id
         WHERE s.expires_at > now() AND ($2::uuid IS NULL OR s.user_id = $2)
         ORDER BY s.last_used_at DESC LIMIT 500")
        .bind(actor.session_id).bind(q.user_id).fetch_all(&st.db).await?;
    Ok(Json(rows))
}

async fn revoke_session(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    let mut tx = st.db.begin().await?;
    let row: Option<(Uuid, String)> = sqlx::query_as("DELETE FROM user_sessions s USING users u WHERE s.id = $1 AND u.id = s.user_id RETURNING u.id, u.username")
        .bind(id).fetch_optional(&mut *tx).await?;
    let (uid, uname) = row.ok_or_else(|| AppError::not_found("Session"))?;
    audit(&mut tx, &actor, &ci, "SESSION_REVOKE", "session", Some(id.to_string()), json!({ "user_id": uid, "username": uname })).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_all(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo) -> AppResult<Json<Value>> {
    let mut tx = st.db.begin().await?;
    let n = sqlx::query("DELETE FROM user_sessions WHERE id <> $1").bind(actor.session_id).execute(&mut *tx).await?.rows_affected();
    audit(&mut tx, &actor, &ci, "SESSIONS_REVOKE_ALL", "session", None, json!({ "revoked": n })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "revoked": n })))
}
