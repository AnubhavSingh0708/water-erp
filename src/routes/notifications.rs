//! `/api/notifications/*` – notification types, own subscriptions, Web Push registration, inbox. Any logged-in user.
//! OWNER: Subagent C2.
//!
//! Privacy rules: a user only ever sees / marks alerts whose `event_type` is in THEIR current `subscribed_notifications`;
//! push endpoints are registered, listed and removed strictly per owner; endpoints are SSRF-validated (see `push.rs`).
use crate::{
    alerts, audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    push::{self, PushOutcome},
    state::AppState,
    validate::PageQuery,
};
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::{collections::{BTreeSet, HashMap}, sync::Mutex, time::{Duration, Instant}};
use uuid::Uuid;

pub mod extract;
use extract::{ApiJson, ApiPath, ApiQuery};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/types", get(types))
        .route("/subscriptions", get(get_subscriptions).put(put_subscriptions))
        .route("/push/key", get(push_key))
        .route("/push/subscribe", post(push_subscribe))
        .route("/push/unsubscribe", post(push_unsubscribe))
        .route("/push/test", post(push_test))
        .route("/inbox", get(inbox))
        .route("/inbox/unread-count", get(unread_count))
        .route("/inbox/read-all", post(read_all))
        .route("/inbox/{id}/read", post(mark_read))
}

// ---------------------------------------------------------------------------------------------------------------
// types + subscriptions
// ---------------------------------------------------------------------------------------------------------------

async fn types(_u: AuthUser) -> Json<Value> {
    Json(Value::Array(alerts::EVENT_TYPES.iter().map(|(c, l)| json!({ "code": c, "label": l })).collect()))
}

async fn current_subscriptions(state: &AppState, user: Uuid) -> AppResult<Vec<String>> {
    let v: Value = sqlx::query_scalar("SELECT subscribed_notifications FROM users WHERE id = $1").bind(user).fetch_one(&state.db).await?;
    Ok(v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default())
}

async fn get_subscriptions(State(s): State<AppState>, u: AuthUser) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "subscribed": current_subscriptions(&s, u.id).await? })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionsReq { subscribed: Vec<String> }

async fn put_subscriptions(State(s): State<AppState>, u: AuthUser, ApiJson(b): ApiJson<SubscriptionsReq>) -> AppResult<Json<Value>> {
    if b.subscribed.len() > 100 { return Err(AppError::field("subscribed", "too many entries")); }
    if let Some(bad) = b.subscribed.iter().find(|c| !alerts::is_valid_event(c)) {
        return Err(AppError::field("subscribed", &format!("unknown notification type {}", bad.chars().take(40).collect::<String>())));
    }
    let set: BTreeSet<String> = b.subscribed.into_iter().collect();
    let list: Vec<String> = set.into_iter().collect();
    // only the caller's own row; the WHERE clause is the id from the session, never from the body
    sqlx::query("UPDATE users SET subscribed_notifications = $2, updated_at = now() WHERE id = $1").bind(u.id).bind(json!(list)).execute(&s.db).await?;
    Ok(Json(json!({ "subscribed": list })))
}

// ---------------------------------------------------------------------------------------------------------------
// Web Push registration
// ---------------------------------------------------------------------------------------------------------------

async fn push_key(State(s): State<AppState>, _u: AuthUser) -> Json<Value> {
    match push::VapidKeys::from_config(&s.cfg) {
        Some(k) => Json(json!({ "public_key": k.public_b64, "enabled": true })),
        None => Json(json!({ "public_key": null, "enabled": false })),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Keys { p256dh: String, auth: String }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscribeReq {
    endpoint: String,
    keys: Keys,
    /// `PushSubscription.toJSON()` includes it; ignored.
    #[serde(default, rename = "expirationTime")]
    _expiration_time: Option<Value>,
}

fn clean_ua(h: &HeaderMap) -> Option<String> {
    h.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).map(|s| s.chars().filter(|c| !c.is_control()).take(300).collect())
}

async fn push_subscribe(State(s): State<AppState>, u: AuthUser, headers: HeaderMap, ApiJson(b): ApiJson<SubscribeReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let endpoint = b.endpoint.trim().to_string();
    push::validate_endpoint(&endpoint).map_err(|m| AppError::field("endpoint", &m))?;
    // keys: p256dh = 65-byte uncompressed P-256 point, auth = 16 bytes, both base64url
    let dh = URL_SAFE_NO_PAD.decode(b.keys.p256dh.trim().trim_end_matches('=')).map_err(|_| AppError::field("keys.p256dh", "must be base64url"))?;
    if dh.len() != 65 || p256::PublicKey::from_sec1_bytes(&dh).is_err() { return Err(AppError::field("keys.p256dh", "is not a valid P-256 public key")); }
    let auth = URL_SAFE_NO_PAD.decode(b.keys.auth.trim().trim_end_matches('=')).map_err(|_| AppError::field("keys.auth", "must be base64url"))?;
    if auth.len() != 16 { return Err(AppError::field("keys.auth", "must be 16 bytes")); }
    let (dh_s, auth_s) = (URL_SAFE_NO_PAD.encode(&dh), URL_SAFE_NO_PAD.encode(&auth));

    let mut tx = s.db.begin().await?;
    // The same browser may be used by several people on a shared device: a re-registration moves the endpoint to the caller.
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO push_subscriptions (user_id, endpoint, p256dh, auth, user_agent) VALUES ($1,$2,$3,$4,$5)
         ON CONFLICT (endpoint) DO UPDATE SET user_id = EXCLUDED.user_id, p256dh = EXCLUDED.p256dh, auth = EXCLUDED.auth,
             user_agent = EXCLUDED.user_agent, active = true, deactivated_at = NULL, last_error = NULL
         RETURNING id")
        .bind(u.id).bind(&endpoint).bind(&dh_s).bind(&auth_s).bind(clean_ua(&headers)).fetch_one(&mut *tx).await?;
    // bound the number of subscriptions per user (oldest dropped)
    sqlx::query(
        "DELETE FROM push_subscriptions WHERE user_id = $1 AND id IN (
             SELECT id FROM push_subscriptions WHERE user_id = $1 ORDER BY created_at DESC, id DESC OFFSET $2)")
        .bind(u.id).bind(push::MAX_SUBSCRIPTIONS_PER_USER).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "enabled": push::VapidKeys::from_config(&s.cfg).is_some() }))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnsubscribeReq { endpoint: String }

async fn push_unsubscribe(State(s): State<AppState>, u: AuthUser, ApiJson(b): ApiJson<UnsubscribeReq>) -> AppResult<StatusCode> {
    if b.endpoint.len() > push::MAX_ENDPOINT_LEN { return Err(AppError::field("endpoint", "is too long")); }
    // strictly the caller's own row; unknown / foreign endpoints are indistinguishable (idempotent 204)
    sqlx::query("DELETE FROM push_subscriptions WHERE endpoint = $1 AND user_id = $2").bind(b.endpoint.trim()).bind(u.id).execute(&s.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

static TEST_LIMIT: Mutex<Option<HashMap<Uuid, Vec<Instant>>>> = Mutex::new(None);

/// Max 5 test pushes per user per minute (each one triggers outbound requests).
fn test_rate_ok(user: Uuid) -> bool {
    let mut g = TEST_LIMIT.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    if map.len() > 5000 { map.retain(|_, v| v.iter().any(|t| now.duration_since(*t) < Duration::from_secs(60))); }
    let hits = map.entry(user).or_default();
    hits.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
    if hits.len() >= 5 { return false; }
    hits.push(now);
    true
}

async fn push_test(State(s): State<AppState>, u: AuthUser) -> AppResult<Json<Value>> {
    let Some(sender) = push::WebPushSender::from_state(&s) else {
        return Err(AppError::conflict("FEATURE_DISABLED", "Web Push is not configured on this server (no VAPID keys)."));
    };
    if !test_rate_ok(u.id) { return Err(AppError::Locked("Too many test notifications. Wait a minute.".into())); }
    let r = send_test_with(&s, &sender, u.id).await?;
    audit::log(&s.db, Some(&u), "push.test", "push_subscriptions", None, json!({ "sent": r["sent"], "failed": r["failed"], "removed": r["removed"] })).await;
    Ok(Json(r))
}

/// Sends the test notification to the user's OWN active subscriptions only (injectable sender for tests).
pub async fn send_test_with(state: &AppState, sender: &dyn push::PushSender, user: Uuid) -> AppResult<Value> {
    let targets = push::targets_for_users(state, &[user]).await?;
    let payload = push::build_payload("Test notification", "Web Push is working on this device.", "/", "test", None);
    let total = targets.len();
    let res = push::deliver(state, sender, targets, &payload).await;
    let sent = res.iter().filter(|(_, o)| *o == PushOutcome::Delivered).count();
    let removed = res.iter().filter(|(_, o)| *o == PushOutcome::Gone).count();
    Ok(json!({ "subscriptions": total, "sent": sent, "removed": removed, "failed": total - sent - removed }))
}

// ---------------------------------------------------------------------------------------------------------------
// inbox
// ---------------------------------------------------------------------------------------------------------------

/// Alerts visible to `$1` (user id): ONLY event types in that user's current subscriptions. Resolved alerts stay visible for
/// 30 days (open ones always).
const VISIBLE: &str = "a.event_type IN (SELECT jsonb_array_elements_text(u.subscribed_notifications) FROM users u WHERE u.id = $1)
                       AND (a.resolved_at IS NULL OR a.created_at > now() - interval '30 days')";

#[derive(Deserialize, Default)]
struct InboxQuery { unread: Option<String>, limit: Option<i64>, offset: Option<i64> }

async fn inbox(State(s): State<AppState>, u: AuthUser, ApiQuery(q): ApiQuery<InboxQuery>) -> AppResult<Json<Value>> {
    let unread = matches!(q.unread.as_deref(), Some("1") | Some("true") | Some("yes"));
    let p = PageQuery { limit: q.limit, offset: q.offset };
    let unread_sql = if unread { " AND r.alert_id IS NULL" } else { "" };
    let rows = sqlx::query(&format!(
        "SELECT a.id, a.event_type, a.severity, a.title, a.body, a.url, a.created_at, (r.alert_id IS NOT NULL) AS read, (a.resolved_at IS NOT NULL) AS resolved
         FROM alerts a LEFT JOIN alert_reads r ON r.alert_id = a.id AND r.user_id = $1
         WHERE {VISIBLE}{unread_sql} ORDER BY a.created_at DESC, a.id DESC LIMIT $2 OFFSET $3"))
        .bind(u.id).bind(p.limit()).bind(p.offset()).fetch_all(&s.db).await?;
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM alerts a LEFT JOIN alert_reads r ON r.alert_id = a.id AND r.user_id = $1 WHERE {VISIBLE}{unread_sql}"))
        .bind(u.id).fetch_one(&s.db).await?;
    let items: Vec<Value> = rows.iter().map(|r| json!({
        "id": r.get::<Uuid, _>("id"), "event_type": r.get::<String, _>("event_type"), "severity": r.get::<String, _>("severity"),
        "title": r.get::<String, _>("title"), "body": r.get::<String, _>("body"), "url": r.get::<String, _>("url"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"), "read": r.get::<bool, _>("read"), "resolved": r.get::<bool, _>("resolved"),
    })).collect();
    Ok(Json(json!({ "items": items, "total": total })))
}

async fn unread_count(State(s): State<AppState>, u: AuthUser) -> AppResult<Json<Value>> {
    let n: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM alerts a LEFT JOIN alert_reads r ON r.alert_id = a.id AND r.user_id = $1 WHERE {VISIBLE} AND r.alert_id IS NULL"))
        .bind(u.id).fetch_one(&s.db).await?;
    Ok(Json(json!({ "count": n })))
}

async fn mark_read(State(s): State<AppState>, u: AuthUser, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    // only alerts inside the caller's subscriptions can be marked: anything else is a plain 404 (no existence probing)
    let visible: bool = sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM alerts a WHERE a.id = $2 AND {VISIBLE})")).bind(u.id).bind(id).fetch_one(&s.db).await?;
    if !visible { return Err(AppError::not_found("notification")); }
    sqlx::query("INSERT INTO alert_reads (alert_id, user_id) VALUES ($1,$2) ON CONFLICT DO NOTHING").bind(id).bind(u.id).execute(&s.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn read_all(State(s): State<AppState>, u: AuthUser) -> AppResult<Json<Value>> {
    let r = sqlx::query(&format!(
        "INSERT INTO alert_reads (alert_id, user_id)
         SELECT a.id, $1 FROM alerts a WHERE {VISIBLE} ON CONFLICT DO NOTHING"))
        .bind(u.id).execute(&s.db).await?;
    Ok(Json(json!({ "marked": r.rows_affected() })))
}
