//! OWNER: Subagent A. Login / first-run setup / current user (API_REFERENCE §1).
use super::admin::common::{
    all_pages, audit_raw, check_password_policy, check_username, hash_password, ip_rate_check, unknown_user_failed,
    unknown_user_locked, verify_password, ApiJson, ClientInfo,
};
use crate::{auth::{self, AuthUser}, error::{hint_for, AppError, AppResult}, state::AppState, validate};
use axum::{
    extract::State,
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

pub fn public_router() -> Router<AppState> { Router::new().route("/login", post(login)) }   // /api/auth
pub fn setup_router() -> Router<AppState> { Router::new().route("/status", get(setup_status)).route("/admin", post(setup_admin)) } // /api/setup
pub fn me_router() -> Router<AppState> {                                                       // /api/me
    Router::new().route("/", get(me)).route("/logout", post(logout)).route("/password", post(change_password))
}

const MAX_SESSIONS_PER_USER: i64 = 10;
/// Advisory-lock key serialising first-admin creation (setup endpoint and CLI bootstrap).
pub const SETUP_LOCK_KEY: i64 = 0x5745_5245_5F41_444D; // "WERE_ADM"

const COOKIE_MAX_AGE: i64 = 2_592_000;

fn set_cookie_value(key: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("{}={key}; Secure; SameSite=Strict; Path=/; Max-Age={COOKIE_MAX_AGE}", auth::COOKIE_NAME)).expect("cookie")
}
fn clear_cookie_value() -> HeaderValue {
    HeaderValue::from_str(&format!("{}=; Secure; SameSite=Strict; Path=/; Max-Age=0", auth::COOKIE_NAME)).expect("cookie")
}

/// The Me object (never contains hashes). Admins implicitly have every page.
pub fn me_json(u: &AuthUser) -> Value {
    json!({
        "id": u.id, "username": u.username, "full_name": u.full_name, "is_admin": u.is_admin,
        "allowed_pages": if u.is_admin { all_pages() } else { u.allowed_pages.clone() },
        "subscribed_notifications": u.subscribed_notifications,
        "allowed_location_ids": if u.is_admin { Vec::new() } else { u.allowed_location_ids.clone() },
    })
}

fn json_strings(v: Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Insert a session for `user_id` (caller holds the user row lock / transaction), pruning to MAX_SESSIONS_PER_USER.
async fn create_session(conn: &mut PgConnection, user_id: Uuid, ci: &ClientInfo, ttl_hours: i64) -> AppResult<(String, DateTime<Utc>)> {
    let key = auth::generate_key();
    let expires_at = Utc::now() + Duration::hours(ttl_hours);
    // expired sessions and everything beyond the newest (MAX-1) ones go away, then the new one makes MAX
    sqlx::query("DELETE FROM user_sessions WHERE user_id = $1 AND (expires_at < now() OR id IN (
                    SELECT id FROM user_sessions WHERE user_id = $1 ORDER BY last_used_at DESC, created_at DESC OFFSET $2))")
        .bind(user_id).bind(MAX_SESSIONS_PER_USER - 1).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO user_sessions (user_id, key_hash, expires_at, user_agent, ip) VALUES ($1,$2,$3,$4,$5)")
        .bind(user_id).bind(auth::hash_key(&key)).bind(expires_at).bind(&ci.user_agent).bind(ci.ip_string())
        .execute(&mut *conn).await?;
    Ok((key, expires_at))
}

fn session_response(status: StatusCode, key: &str, expires_at: DateTime<Utc>, me: Value) -> Response {
    let mut r = (status, Json(json!({ "key": key, "expires_at": expires_at, "user": me }))).into_response();
    r.headers_mut().insert(header::SET_COOKIE, set_cookie_value(key));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// 401 with the contract's fixed message (identical for unknown user, wrong password, inactive user).
fn bad_credentials() -> Response {
    let mut body = json!({ "code": "UNAUTHORIZED", "message": "Invalid username or password" });
    if let Some(h) = hint_for("UNAUTHORIZED") { body["hint"] = json!(h); }
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": body }))).into_response()
}

fn locked_error(lockout_minutes: i64) -> AppError {
    AppError::Locked(format!("Too many failed attempts. This account is temporarily locked - try again in about {lockout_minutes} minutes."))
}

// ---------------------------------------------------------------------------------------------------------------------
// POST /api/auth/login
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginReq { username: String, password: String }

async fn login(State(st): State<AppState>, ci: ClientInfo, ApiJson(b): ApiJson<LoginReq>) -> AppResult<Response> {
    ip_rate_check(ci.ip)?;
    if b.username.trim().is_empty() { return Err(AppError::field("username", "is required")); }
    if b.password.is_empty() { return Err(AppError::field("password", "is required")); }
    // absurd inputs can never be valid: same generic answer, no hashing work
    if b.username.chars().count() > 100 || b.password.chars().count() > 256 || b.username.chars().any(|c| c.is_control()) { return Ok(bad_credentials()); }
    let uname = b.username.trim().to_lowercase();
    let s = st.settings.get();

    let row = sqlx::query("SELECT id, password_hash, is_active, locked_until FROM users WHERE lower(username) = $1")
        .bind(&uname).fetch_optional(&st.db).await?;

    let Some(row) = row else {
        if unknown_user_locked(&uname) { return Err(locked_error(s.lockout_minutes)); }
        let _ = verify_password(&b.password, None).await?;
        if unknown_user_failed(&uname, s.max_failed_logins, s.lockout_minutes) { return Err(locked_error(s.lockout_minutes)); }
        return Ok(bad_credentials());
    };
    let uid: Uuid = row.get("id");
    let hash: String = row.get("password_hash");
    let is_active: bool = row.get("is_active");
    let locked_until: Option<DateTime<Utc>> = row.get("locked_until");
    if locked_until.map_or(false, |t| t > Utc::now()) { return Err(locked_error(s.lockout_minutes)); }

    let ok = verify_password(&b.password, Some(&hash)).await?;
    if !ok || !is_active {
        // atomic counter; locks when the threshold is reached (counter restarts after the lock is set)
        let locked_now: bool = sqlx::query_scalar(
            "UPDATE users SET failed_login_count = CASE WHEN failed_login_count + 1 >= $2 THEN 0 ELSE failed_login_count + 1 END,
                              locked_until = CASE WHEN failed_login_count + 1 >= $2 THEN now() + make_interval(mins => $3) ELSE locked_until END
             WHERE id = $1 RETURNING COALESCE(locked_until > now(), false)")
            .bind(uid).bind(s.max_failed_logins as i32).bind(s.lockout_minutes as i32).fetch_one(&st.db).await?;
        if locked_now {
            let mut c = st.db.acquire().await?;
            audit_raw(&mut c, Some(uid), Some(&uname), ci.ip_string(), "LOGIN_LOCKED", "user", Some(uid.to_string()),
                      json!({ "lockout_minutes": s.lockout_minutes })).await?;
            return Err(locked_error(s.lockout_minutes));
        }
        return Ok(bad_credentials());
    }

    let mut tx = st.db.begin().await?;
    // re-check under the row lock: deactivation / lockout may have happened while we were hashing
    let u = sqlx::query("SELECT username, full_name, is_admin, is_active, locked_until, allowed_pages, subscribed_notifications, allowed_location_ids FROM users WHERE id = $1 FOR UPDATE")
        .bind(uid).fetch_optional(&mut *tx).await?;
    let Some(u) = u else { return Ok(bad_credentials()) };
    if !u.get::<bool, _>("is_active") { return Ok(bad_credentials()); }
    if u.get::<Option<DateTime<Utc>>, _>("locked_until").map_or(false, |t| t > Utc::now()) { return Err(locked_error(s.lockout_minutes)); }
    sqlx::query("UPDATE users SET failed_login_count = 0, locked_until = NULL, last_login_at = now() WHERE id = $1").bind(uid).execute(&mut *tx).await?;
    let (key, expires_at) = create_session(&mut tx, uid, &ci, s.session_ttl_hours).await?;
    let username: String = u.get("username");
    audit_raw(&mut tx, Some(uid), Some(&username), ci.ip_string(), "LOGIN", "user", Some(uid.to_string()), json!({})).await?;
    tx.commit().await?;
    let me = AuthUser {
        id: uid, username, full_name: u.get("full_name"), is_admin: u.get("is_admin"),
        allowed_pages: json_strings(u.get("allowed_pages")), subscribed_notifications: json_strings(u.get("subscribed_notifications")),
        allowed_location_ids: json_strings(u.get("allowed_location_ids")).iter().filter_map(|x| x.parse().ok()).collect(), session_id: Uuid::nil(),
    };
    Ok(session_response(StatusCode::OK, &key, expires_at, me_json(&me)))
}

// ---------------------------------------------------------------------------------------------------------------------
// Setup (first Admin)
// ---------------------------------------------------------------------------------------------------------------------

async fn setup_status(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let any_admin: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE is_admin)").fetch_one(&st.db).await?;
    if !any_admin { ensure_setup_token(&st.db).await?; }
    Ok(Json(json!({ "setup_required": !any_admin, "token_required": !any_admin })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupReq { username: String, full_name: String, password: String, #[serde(default)] setup_token: Option<String> }

// ---- one-time first-run setup token (stops a LAN neighbour from claiming the Admin account before the owner) ----------
// Keyed by database name so that several databases in one process (tests) do not share a token; in production there is one entry.
static SETUP_TOKENS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

async fn db_key(db: &sqlx::PgPool) -> AppResult<String> {
    Ok(sqlx::query_scalar("SELECT current_database()").fetch_one(db).await?)
}

/// Returns the token (generating + logging it once if missing) while no Admin exists; `None` once an Admin exists.
async fn ensure_setup_token(db: &sqlx::PgPool) -> AppResult<Option<String>> {
    let any: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE is_admin)").fetch_one(db).await?;
    let key = db_key(db).await?;
    let mut m = SETUP_TOKENS.lock().unwrap_or_else(|e| e.into_inner());
    if any { m.remove(&key); return Ok(None); }
    if let Some(t) = m.get(&key) { return Ok(Some(t.clone())); }
    let mut b = [0u8; 18];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    let t = hex::encode(b); // 36 chars from the OS CSPRNG
    tracing::warn!("FIRST-RUN SETUP TOKEN: {t}  (enter it on /setup.html to create the first administrator)");
    m.insert(key, t.clone());
    Ok(Some(t))
}

/// Called at start-up: if no Admin exists, create and log (once) the one-time setup token.
pub async fn announce_setup_token(db: &sqlx::PgPool) -> anyhow::Result<()> {
    ensure_setup_token(db).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// The currently valid token, if any (for tests and tooling inside the process; never exposed over HTTP).
pub async fn peek_setup_token(db: &sqlx::PgPool) -> Option<String> {
    let key = db_key(db).await.ok()?;
    SETUP_TOKENS.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned()
}

fn token_matches(given: &str, expected: &str) -> bool {
    use sha2::{Digest, Sha256};
    let (a, b) = (Sha256::digest(given.as_bytes()), Sha256::digest(expected.as_bytes()));
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn admin_pages_json() -> Value { json!(all_pages().into_iter().filter(|p| p != "admin").collect::<Vec<_>>()) }

async fn setup_admin(State(st): State<AppState>, ci: ClientInfo, ApiJson(b): ApiJson<SetupReq>) -> AppResult<Response> {
    ip_rate_check(ci.ip)?; // every attempt (including wrong tokens) counts towards the per-IP limit
    let setup_disabled = || AppError::conflict("SETUP_DISABLED", "Setup has already been completed. Sign in instead.");
    // cheap pre-check before the expensive hash; the authoritative check is inside the locked transaction
    let any: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE is_admin)").fetch_one(&st.db).await?;
    if any { return Err(setup_disabled()); }
    let expected = ensure_setup_token(&st.db).await?.ok_or_else(setup_disabled)?;
    if !b.setup_token.as_deref().map_or(false, |g| g.len() <= 200 && token_matches(g.trim(), &expected)) {
        return Err(AppError::forbidden("A valid setup token is required. It is printed in the server log at first start (FIRST-RUN SETUP TOKEN)."));
    }
    let username = check_username("username", &b.username)?;
    let full_name = validate::required_text("full_name", &b.full_name, 120)?;
    check_password_policy("password", &b.password, &username)?;
    let hash = hash_password(&b.password).await?;

    let mut tx = st.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(SETUP_LOCK_KEY).execute(&mut *tx).await?;
    let any: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE is_admin)").fetch_one(&mut *tx).await?;
    if any { return Err(setup_disabled()); }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (username, full_name, password_hash, is_admin, allowed_pages, subscribed_notifications, last_login_at)
         VALUES ($1,$2,$3,true,$4,'[]'::jsonb, now()) RETURNING id")
        .bind(&username).bind(&full_name).bind(&hash).bind(admin_pages_json()).fetch_one(&mut *tx).await?;
    let (key, expires_at) = create_session(&mut tx, id, &ci, st.settings.get().session_ttl_hours).await?;
    audit_raw(&mut tx, Some(id), Some(&username), ci.ip_string(), "SETUP_ADMIN", "user", Some(id.to_string()), json!({ "via": "setup-screen" })).await?;
    tx.commit().await?;
    if let Ok(k) = db_key(&st.db).await { SETUP_TOKENS.lock().unwrap_or_else(|e| e.into_inner()).remove(&k); }
    let me = AuthUser { id, username, full_name, is_admin: true, allowed_pages: vec![], subscribed_notifications: vec![], allowed_location_ids: vec![], session_id: Uuid::nil() };
    Ok(session_response(StatusCode::CREATED, &key, expires_at, me_json(&me)))
}

/// Create the first Admin if none exists (CLI `--setup-admin`). Idempotent; never overwrites an existing user.
pub async fn bootstrap_admin(db: &sqlx::PgPool, username: &str, password: &str) -> anyhow::Result<()> {
    let username = check_username("username", username).map_err(|e| anyhow::anyhow!("invalid admin username: {e}"))?;
    check_password_policy("password", password, &username).map_err(|_| anyhow::anyhow!("ERP_ADMIN_PASSWORD must be 10-128 characters and differ from the username"))?;
    let any: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE is_admin)").fetch_one(db).await?;
    if any { tracing::info!("--setup-admin: an administrator already exists, nothing to do"); return Ok(()); }
    let hash = hash_password(password).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(SETUP_LOCK_KEY).execute(&mut *tx).await?;
    let any: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE is_admin)").fetch_one(&mut *tx).await?;
    if any { return Ok(()); }
    let taken: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE lower(username) = lower($1))").bind(&username).fetch_one(&mut *tx).await?;
    if taken { anyhow::bail!("user '{username}' already exists and is not an administrator; refusing to modify it"); }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (username, full_name, password_hash, is_admin, allowed_pages) VALUES ($1,$1,$2,true,$3) RETURNING id")
        .bind(&username).bind(&hash).bind(admin_pages_json()).fetch_one(&mut *tx).await?;
    audit_raw(&mut tx, Some(id), Some(&username), None, "SETUP_ADMIN", "user", Some(id.to_string()), json!({ "via": "cli" })).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    tx.commit().await?;
    if let Ok(k) = db_key(db).await { SETUP_TOKENS.lock().unwrap_or_else(|e| e.into_inner()).remove(&k); }
    tracing::info!(%username, "first administrator created");
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// /api/me
// ---------------------------------------------------------------------------------------------------------------------

async fn me(u: AuthUser) -> Json<Value> { Json(me_json(&u)) }

async fn logout(State(st): State<AppState>, u: AuthUser, ci: ClientInfo) -> AppResult<Response> {
    let mut tx = st.db.begin().await?;
    sqlx::query("DELETE FROM user_sessions WHERE id = $1").bind(u.session_id).execute(&mut *tx).await?;
    audit_raw(&mut tx, Some(u.id), Some(&u.username), ci.ip_string(), "LOGOUT", "user", Some(u.id.to_string()), json!({})).await?;
    tx.commit().await?;
    let mut r = StatusCode::NO_CONTENT.into_response();
    r.headers_mut().insert(header::SET_COOKIE, clear_cookie_value());
    Ok(r)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordReq { current_password: String, new_password: String }

async fn change_password(State(st): State<AppState>, u: AuthUser, ci: ClientInfo, ApiJson(b): ApiJson<PasswordReq>) -> AppResult<StatusCode> {
    ip_rate_check(ci.ip)?;
    let s = st.settings.get();
    check_password_policy("new_password", &b.new_password, &u.username)?;
    if b.current_password.chars().count() > 256 { return Err(AppError::field("current_password", "is incorrect")); }
    if b.new_password == b.current_password { return Err(AppError::field("new_password", "must be different from the current password")); }
    let row = sqlx::query("SELECT password_hash, locked_until FROM users WHERE id = $1").bind(u.id).fetch_one(&st.db).await?;
    if row.get::<Option<DateTime<Utc>>, _>("locked_until").map_or(false, |t| t > Utc::now()) { return Err(locked_error(s.lockout_minutes)); }
    let ok = verify_password(&b.current_password, Some(&row.get::<String, _>("password_hash"))).await?;
    if !ok {
        // a stolen session key must not be able to brute-force the password: failures count towards the same lockout
        sqlx::query(
            "UPDATE users SET failed_login_count = CASE WHEN failed_login_count + 1 >= $2 THEN 0 ELSE failed_login_count + 1 END,
                              locked_until = CASE WHEN failed_login_count + 1 >= $2 THEN now() + make_interval(mins => $3) ELSE locked_until END
             WHERE id = $1")
            .bind(u.id).bind(s.max_failed_logins as i32).bind(s.lockout_minutes as i32).execute(&st.db).await?;
        return Err(AppError::field("current_password", "is incorrect"));
    }
    let hash = hash_password(&b.new_password).await?;
    let mut tx = st.db.begin().await?;
    sqlx::query("UPDATE users SET password_hash = $2, failed_login_count = 0, updated_at = now() WHERE id = $1").bind(u.id).bind(&hash).execute(&mut *tx).await?;
    let revoked = sqlx::query("DELETE FROM user_sessions WHERE user_id = $1 AND id <> $2").bind(u.id).bind(u.session_id).execute(&mut *tx).await?.rows_affected();
    audit_raw(&mut tx, Some(u.id), Some(&u.username), ci.ip_string(), "PASSWORD_CHANGE", "user", Some(u.id.to_string()), json!({ "other_sessions_revoked": revoked })).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
