//! Authentication core: key generation/hashing, the Axum auth middleware and permission guards.
//!
//! Flow: login (routes/auth.rs) mints a random 32-byte key, stores only SHA-256(key) in `user_sessions`, returns the key
//! once; the browser keeps it in a JS-readable cookie and echoes it in `X-Auth-Key` (or `Authorization: Bearer`).
//! This middleware hashes the presented key, loads session+user, rejects expired sessions, and slides
//! `last_used_at`/`expires_at` forward (DB write throttled to once per `TOUCH_INTERVAL_SECS`).
use crate::{error::AppError, state::AppState};
use axum::{
    extract::{FromRequestParts, Request, State},
    http::{header, request::Parts},
    middleware::Next,
    response::Response,
};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

pub const TOUCH_INTERVAL_SECS: i64 = 60;
pub const COOKIE_NAME: &str = "erp_key";

/// Every page key that may appear in `users.allowed_pages`. `admin` is implicit for admins.
pub const PAGES: &[&str] = &["dashboard", "raw-materials", "production", "qc", "warehouse", "qr-labels", "loading", "jars", "maintenance", "scan", "admin"];

#[derive(Clone, Debug)]
pub struct AuthUser {
    pub id: Uuid,
    pub username: String,
    pub full_name: String,
    pub is_admin: bool,
    pub allowed_pages: Vec<String>,
    pub subscribed_notifications: Vec<String>,
    /// Godowns this user may work in; empty = all godowns.
    pub allowed_location_ids: Vec<Uuid>,
    pub session_id: Uuid,
}

impl AuthUser {
    /// Admins can open every page; others only what is in `allowed_pages`.
    pub fn can_access(&self, page: &str) -> bool {
        self.is_admin || self.allowed_pages.iter().any(|p| p == page)
    }
    /// Admins and users with no explicit godown list may use every godown.
    pub fn can_use_location(&self, location: Uuid) -> bool {
        self.is_admin || self.allowed_location_ids.is_empty() || self.allowed_location_ids.contains(&location)
    }
    /// Same rule as an error-returning check for handlers: `user.require_location(loc)?`.
    pub fn require_location(&self, location: Uuid) -> Result<(), AppError> {
        if self.can_use_location(location) { Ok(()) } else { Err(AppError::forbidden("You do not have access to this godown")) }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AuthUser {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<AuthUser>().cloned().ok_or(AppError::Unauthorized)
    }
}

/// 32 random bytes from the OS CSPRNG, hex-encoded (64 chars).
pub fn generate_key() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

/// hex(SHA-256(key)) – what is stored in `user_sessions.key_hash`.
pub fn hash_key(key: &str) -> String { hex::encode(Sha256::digest(key.as_bytes())) }

fn extract_key(parts: &axum::http::HeaderMap) -> Option<String> {
    if let Some(v) = parts.get("x-auth-key").and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if !v.is_empty() { return Some(v.to_string()); }
    }
    let a = parts.get(header::AUTHORIZATION)?.to_str().ok()?;
    a.strip_prefix("Bearer ").map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn json_strings(v: serde_json::Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Resolve a raw key to a user, sliding the session. Used by the middleware (and handy in tests).
pub async fn authenticate(state: &AppState, key: &str) -> Result<AuthUser, AppError> {
    let hash = hash_key(key);
    let row = sqlx::query(
        "SELECT s.id AS sid, s.last_used_at, s.expires_at, u.id AS uid, u.username, u.full_name, u.is_admin, u.is_active,
                u.allowed_pages, u.subscribed_notifications, u.allowed_location_ids
         FROM user_sessions s JOIN users u ON u.id = s.user_id WHERE s.key_hash = $1")
        .bind(&hash).fetch_optional(&state.db).await?;
    let row = row.ok_or(AppError::Unauthorized)?;
    let now = Utc::now();
    let expires_at: DateTime<Utc> = row.get("expires_at");
    let last_used: DateTime<Utc> = row.get("last_used_at");
    if expires_at <= now || !row.get::<bool, _>("is_active") { return Err(AppError::Unauthorized); }
    let sid: Uuid = row.get("sid");
    if (now - last_used).num_seconds() >= TOUCH_INTERVAL_SECS {
        let ttl = Duration::hours(state.settings.get().session_ttl_hours);
        sqlx::query("UPDATE user_sessions SET last_used_at = $2, expires_at = $3 WHERE id = $1")
            .bind(sid).bind(now).bind(now + ttl).execute(&state.db).await?;
    }
    Ok(AuthUser {
        id: row.get("uid"), username: row.get("username"), full_name: row.get("full_name"), is_admin: row.get("is_admin"),
        allowed_pages: json_strings(row.get("allowed_pages")),
        subscribed_notifications: json_strings(row.get("subscribed_notifications")),
        allowed_location_ids: json_strings(row.get("allowed_location_ids")).iter().filter_map(|x| x.parse().ok()).collect(),
        session_id: sid,
    })
}

/// Axum middleware: requires a valid key, inserts `AuthUser` into request extensions.
pub async fn auth_middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Result<Response, AppError> {
    let key = extract_key(req.headers()).ok_or(AppError::Unauthorized)?;
    let user = authenticate(&state, &key).await?;
    req.extensions_mut().insert(user);
    Ok(next.run(req).await)
}

/// Guard: caller must be allowed on at least one of `pages` (Admins always pass). 403 otherwise.
pub async fn page_guard(pages: &'static [&'static str], req: Request, next: Next) -> Result<Response, AppError> {
    let u = req.extensions().get::<AuthUser>().ok_or(AppError::Unauthorized)?;
    if pages.iter().any(|p| u.can_access(p)) { Ok(next.run(req).await) }
    else { Err(AppError::forbidden(format!("You do not have access to this page ({})", pages.join("/")))) }
}

/// Guard: Admin only.
pub async fn admin_guard(req: Request, next: Next) -> Result<Response, AppError> {
    let u = req.extensions().get::<AuthUser>().ok_or(AppError::Unauthorized)?;
    if u.is_admin { Ok(next.run(req).await) } else { Err(AppError::forbidden("Administrator access required")) }
}

/// Wrap a router so every route in it requires one of `pages`.
pub fn guarded(pages: &'static [&'static str], r: axum::Router<AppState>) -> axum::Router<AppState> {
    r.layer(axum::middleware::from_fn(move |req, next| page_guard(pages, req, next)))
}
