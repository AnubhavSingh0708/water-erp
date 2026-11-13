//! Shared helpers of the auth/admin modules (owner A): enveloped extractors, password hashing, login limiter,
//! client info (IP / user-agent), audit helper and input validators.
use crate::{auth::{AuthUser, PAGES}, error::{AppError, AppResult}};
use argon2::{password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString}, Algorithm, Argon2, Params, Version};
use axum::{
    extract::{ConnectInfo, FromRequest, FromRequestParts, Request},
    http::{request::Parts, HeaderValue},
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::PgConnection;
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use uuid::Uuid;

// ---------------------------------------------------------------------------------------------------------------------
// Extractors that answer inside the error envelope
// ---------------------------------------------------------------------------------------------------------------------

/// `Json<T>` whose rejections use the API error envelope (400 BAD_REQUEST) instead of axum's plain-text 4xx.
pub struct ApiJson<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
    type Rejection = AppError;
    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(v)) => Ok(ApiJson(v)),
            Err(rej) => {
                use axum::extract::rejection::JsonRejection as R;
                let msg = match rej {
                    R::MissingJsonContentType(_) => "Content-Type must be application/json".to_string(),
                    R::JsonSyntaxError(_) => "Malformed JSON body".to_string(),
                    R::JsonDataError(e) => {
                        let t = e.body_text();
                        let t = t.strip_prefix("Failed to deserialize the JSON body into the target type: ").unwrap_or(&t);
                        format!("Invalid request body: {}", t.chars().take(300).collect::<String>())
                    }
                    _ => "Request body could not be read (too large?)".to_string(),
                };
                Err(AppError::bad(msg))
            }
        }
    }
}

/// `Query<T>` with enveloped rejections.
pub struct ApiQuery<T>(pub T);
impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for ApiQuery<T> {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Query::<T>::from_request_parts(parts, state).await
            .map(|q| ApiQuery(q.0)).map_err(|_| AppError::bad("Invalid query parameters"))
    }
}

/// `Path<T>` with enveloped rejections.
pub struct ApiPath<T>(pub T);
impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for ApiPath<T> {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Path::<T>::from_request_parts(parts, state).await
            .map(|p| ApiPath(p.0)).map_err(|_| AppError::bad("Invalid identifier in the URL"))
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Client info (never trusts X-Forwarded-For)
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct ClientInfo {
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
}
impl ClientInfo {
    pub fn ip_string(&self) -> Option<String> { self.ip.map(|i| i.to_string()) }
}

impl<S: Send + Sync> FromRequestParts<S> for ClientInfo {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let ip = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
        let user_agent = parts.headers.get("user-agent").map(sanitize_ua).filter(|s| !s.is_empty());
        Ok(ClientInfo { ip, user_agent })
    }
}

/// Lossy-decode, drop control characters, truncate by *characters* (never slices bytes).
pub fn sanitize_ua(v: &HeaderValue) -> String {
    String::from_utf8_lossy(v.as_bytes()).chars().filter(|c| !c.is_control()).take(200).collect()
}

// ---------------------------------------------------------------------------------------------------------------------
// Passwords
// ---------------------------------------------------------------------------------------------------------------------

const ARGON_M_KIB: u32 = 19 * 1024; // OWASP minimum profile: m=19 MiB, t=2, p=1
const ARGON_T: u32 = 2;
const ARGON_P: u32 = 1;

fn argon() -> Argon2<'static> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::new(ARGON_M_KIB, ARGON_T, ARGON_P, None).expect("argon2 params"))
}

/// At most 4 concurrent hash/verify jobs: bounds CPU and the 19 MiB-per-job memory under a login flood.
static HASH_PERMITS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(4));

pub fn hash_password_blocking(pw: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    argon().hash_password(pw.as_bytes(), &salt).map(|h| h.to_string())
        .map_err(|e| AppError::Internal(anyhow::anyhow!("argon2 hash: {e}")))
}

/// Verify against a stored PHC string. An unparseable stored hash never matches (after doing equivalent work).
pub fn verify_password_blocking(pw: &str, stored: &str) -> bool {
    match PasswordHash::new(stored) {
        Ok(h) => argon().verify_password(pw.as_bytes(), &h).is_ok(),
        Err(_) => { let _ = argon().verify_password(pw.as_bytes(), &dummy_hash()); false }
    }
}

fn dummy_hash() -> PasswordHash<'static> { PasswordHash::new(DUMMY_HASH.as_str()).expect("dummy hash") }

/// Hash of a random value with the *current* parameters: verifying against it costs the same as a real verify.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    let mut b = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    hash_password_blocking(&hex::encode(b)).expect("dummy hash")
});

pub async fn hash_password(pw: &str) -> AppResult<String> {
    let _p = HASH_PERMITS.acquire().await.map_err(|_| AppError::Unavailable)?;
    let pw = pw.to_string();
    tokio::task::spawn_blocking(move || hash_password_blocking(&pw)).await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("hash task: {e}")))?
}

/// `stored = None` (unknown user) verifies against the dummy hash and always returns false.
pub async fn verify_password(pw: &str, stored: Option<&str>) -> AppResult<bool> {
    let _p = HASH_PERMITS.acquire().await.map_err(|_| AppError::Unavailable)?;
    let pw = pw.to_string();
    let stored = stored.map(str::to_string);
    tokio::task::spawn_blocking(move || match stored {
        Some(s) => verify_password_blocking(&pw, &s),
        None => { let _ = argon().verify_password(pw.as_bytes(), &dummy_hash()); false }
    }).await.map_err(|e| AppError::Internal(anyhow::anyhow!("verify task: {e}")))
}

/// Password policy: 10..=128 characters, no control characters, not equal to the username (case-insensitive).
pub fn check_password_policy(field: &str, pw: &str, username: &str) -> AppResult<()> {
    let n = pw.chars().count();
    if n < 10 { return Err(AppError::field(field, "must be at least 10 characters")); }
    if n > 128 { return Err(AppError::field(field, "must be at most 128 characters")); }
    if pw.chars().any(|c| c.is_control()) { return Err(AppError::field(field, "contains invalid characters")); }
    if pw.trim().is_empty() { return Err(AppError::field(field, "must not be blank")); }
    if pw.to_lowercase() == username.to_lowercase() { return Err(AppError::field(field, "must not be the same as the username")); }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// Validators
// ---------------------------------------------------------------------------------------------------------------------

/// 3..=40 chars of `[A-Za-z0-9._-]`.
pub fn check_username(field: &str, u: &str) -> AppResult<String> {
    let t = u.trim();
    let n = t.chars().count();
    if !(3..=40).contains(&n) { return Err(AppError::field(field, "must be 3-40 characters")); }
    if !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
        return Err(AppError::field(field, "may only contain letters, digits, '.', '_' and '-'"));
    }
    Ok(t.to_string())
}

/// Page keys must be a subset of `PAGES`. The `admin` key is rejected: administration is a role (`is_admin`), not a page grant.
pub fn check_pages(field: &str, pages: &[String]) -> AppResult<Vec<String>> {
    if pages.len() > 50 { return Err(AppError::field(field, "too many entries")); }
    let mut out: Vec<String> = vec![];
    for p in pages {
        if p == "admin" { return Err(AppError::field(field, "the admin page is only available to administrators (use is_admin)")); }
        if !PAGES.contains(&p.as_str()) { return Err(AppError::field(field, &format!("unknown page '{}'", p.chars().take(40).collect::<String>()))); }
        if !out.contains(p) { out.push(p.clone()); }
    }
    Ok(out)
}

pub fn check_subscriptions(field: &str, subs: &[String]) -> AppResult<Vec<String>> {
    if subs.len() > 50 { return Err(AppError::field(field, "too many entries")); }
    let mut out: Vec<String> = vec![];
    for s in subs {
        if !crate::alerts::is_valid_event(s) { return Err(AppError::field(field, &format!("unknown notification type '{}'", s.chars().take(40).collect::<String>()))); }
        if !out.contains(s) { out.push(s.clone()); }
    }
    Ok(out)
}

pub fn all_pages() -> Vec<String> { PAGES.iter().map(|s| s.to_string()).collect() }

// ---------------------------------------------------------------------------------------------------------------------
// Audit (inside the caller's transaction: audit-or-fail)
// ---------------------------------------------------------------------------------------------------------------------

pub async fn audit_raw(conn: &mut PgConnection, user_id: Option<Uuid>, username: Option<&str>, ip: Option<String>,
                       action: &str, entity: &str, entity_id: Option<String>, details: Value) -> AppResult<()> {
    sqlx::query("INSERT INTO audit_logs (user_id, username, action, entity, entity_id, details, ip) VALUES ($1,$2,$3,$4,$5,$6,$7)")
        .bind(user_id).bind(username).bind(action).bind(entity).bind(entity_id).bind(details).bind(ip)
        .execute(&mut *conn).await?;
    Ok(())
}

pub async fn audit(conn: &mut PgConnection, actor: &AuthUser, ci: &ClientInfo, action: &str, entity: &str, entity_id: Option<String>, details: Value) -> AppResult<()> {
    audit_raw(conn, Some(actor.id), Some(&actor.username), ci.ip_string(), action, entity, entity_id, details).await
}

// ---------------------------------------------------------------------------------------------------------------------
// In-memory limiters (single process; DB lockout counters are the durable layer)
// ---------------------------------------------------------------------------------------------------------------------

const IP_LIMIT: usize = 30;
const IP_WINDOW: Duration = Duration::from_secs(60);
const MAP_CAP: usize = 20_000;

/// Bucket key: IPv4 address, IPv6 /64.
fn ip_key(ip: IpAddr) -> u128 {
    match ip { IpAddr::V4(v) => u32::from(v) as u128, IpAddr::V6(v) => (u128::from(v) >> 64) | (1u128 << 100) }
}

static IP_ATTEMPTS: LazyLock<Mutex<HashMap<u128, VecDeque<Instant>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record one login/setup attempt from `ip`; Err(Locked) when it exceeds 30 per minute. Unknown IP => not limited.
pub fn ip_rate_check(ip: Option<IpAddr>) -> AppResult<()> {
    ip_rate_check_n(ip, IP_LIMIT)
}
pub fn ip_rate_check_n(ip: Option<IpAddr>, limit: usize) -> AppResult<()> {
    let Some(ip) = ip else { return Ok(()) };
    let now = Instant::now();
    let mut map = IP_ATTEMPTS.lock().unwrap_or_else(|e| e.into_inner());
    if map.len() >= MAP_CAP {
        map.retain(|_, q| q.back().map_or(false, |t| now.duration_since(*t) < IP_WINDOW));
        if map.len() >= MAP_CAP { map.clear(); }
    }
    let q = map.entry(ip_key(ip)).or_default();
    while q.front().map_or(false, |t| now.duration_since(*t) >= IP_WINDOW) { q.pop_front(); }
    if q.len() >= limit {
        return Err(AppError::Locked("Too many sign-in attempts from this address. Wait a minute and try again.".into()));
    }
    q.push_back(now);
    Ok(())
}

#[derive(Default)]
struct Strikes { count: i64, locked_until: Option<Instant>, last: Option<Instant> }
static UNKNOWN_USER_STRIKES: LazyLock<Mutex<HashMap<String, Strikes>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Unknown usernames must lock out exactly like real accounts, otherwise the 429 reveals which names exist.
pub fn unknown_user_locked(name: &str) -> bool {
    let m = UNKNOWN_USER_STRIKES.lock().unwrap_or_else(|e| e.into_inner());
    m.get(name).and_then(|s| s.locked_until).map_or(false, |t| t > Instant::now())
}
/// Returns true when this failure triggers the lockout.
pub fn unknown_user_failed(name: &str, max_failed: i64, lockout_minutes: i64) -> bool {
    let now = Instant::now();
    let mut m = UNKNOWN_USER_STRIKES.lock().unwrap_or_else(|e| e.into_inner());
    if m.len() >= MAP_CAP {
        m.retain(|_, s| s.locked_until.map_or(false, |t| t > now) || s.last.map_or(false, |l| now.duration_since(l) < Duration::from_secs(3600)));
        if m.len() >= MAP_CAP { m.clear(); }
    }
    let s = m.entry(name.to_string()).or_default();
    if s.locked_until.map_or(false, |t| t <= now) { s.locked_until = None; s.count = 0; }
    s.count += 1;
    s.last = Some(now);
    if s.count >= max_failed {
        s.count = 0;
        s.locked_until = Some(now + Duration::from_secs(lockout_minutes.max(1) as u64 * 60));
        return true;
    }
    false
}
