//! Admin: settings, page / notification catalogues, audit log query (API_REFERENCE §2).
use super::common::{audit, ApiJson, ApiQuery, ClientInfo};
use crate::{auth::{AuthUser, PAGES}, error::{AppError, AppResult}, settings, state::AppState, validate::{self, Paged}};
use axum::{extract::State, routing::get, Json, Router};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/settings", get(get_settings).put(put_settings))
        .route("/pages", get(pages))
        .route("/notification-types", get(notification_types))
        .route("/audit", get(audit_query))
}

fn page_label(k: &str) -> &'static str {
    match k {
        "dashboard" => "Dashboard", "raw-materials" => "Raw materials", "production" => "Production", "qc" => "Lab QC",
        "warehouse" => "Warehouse", "qr-labels" => "QR labels", "loading" => "Loading & dispatch", "jars" => "Returnable containers",
        "maintenance" => "Maintenance & fleet", "admin" => "Administration", _ => "",
    }
}
async fn pages() -> Json<Value> { Json(json!(PAGES.iter().map(|k| json!({ "key": k, "label": page_label(k) })).collect::<Vec<_>>())) }
async fn notification_types() -> Json<Value> {
    Json(json!(crate::alerts::EVENT_TYPES.iter().map(|(c, l)| json!({ "code": c, "label": l })).collect::<Vec<_>>()))
}

async fn get_settings(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let rows = settings::list(&st.db).await?;
    Ok(Json(json!(rows.into_iter().map(|(key, value, description)| json!({ "key": key, "value": value, "description": description })).collect::<Vec<_>>())))
}

// ---- validation mirror of settings::SCHEMA (plus stricter rules for free-text keys) ------------------------------------

enum Kind { Str, Int(i64, i64), Bool, Num }
const SCHEMA: &[(&str, Kind)] = &[
    ("plant.name", Kind::Str), ("plant.code", Kind::Str), ("plant.qr_base_url", Kind::Str),
    ("auth.session_ttl_hours", Kind::Int(1, 24 * 90)), ("auth.session_inactivity_purge_days", Kind::Int(1, 365)),
    ("auth.max_failed_logins", Kind::Int(1, 100)), ("auth.lockout_minutes", Kind::Int(1, 1440)),
    ("inventory.burn_rate_lookback_days", Kind::Int(1, 365)), ("inventory.safety_stock_days", Kind::Int(0, 90)),
    ("inventory.worker_interval_minutes", Kind::Int(1, 1440)),
    ("jars.overdue_days", Kind::Int(1, 365)), ("alerts.expiry_lead_days", Kind::Int(0, 365)),
    ("loading.default_crate_set_size", Kind::Int(1, 1000)), ("loading.strict_qc_lock", Kind::Bool),
    ("features.qr_labels", Kind::Bool), ("plant.utc_offset_minutes", Kind::Int(-720, 840)),
    ("entry.large_adjust_pct", Kind::Int(1, 1000)), ("production.scrap_warn_pct", Kind::Int(0, 100)),
    ("loading.void_window_hours", Kind::Int(1, 720)), ("settle.damage_warn_pct", Kind::Int(0, 100)), ("jars.destroy_warn_pct", Kind::Int(0, 100)),
    ("lab.ph_min", Kind::Num), ("lab.ph_max", Kind::Num), ("lab.tds_min", Kind::Num), ("lab.tds_max", Kind::Num),
];

fn check_value(key: &str, kind: &Kind, v: &Value) -> Result<(), String> {
    match kind {
        Kind::Bool => if v.is_boolean() { Ok(()) } else { Err("must be true or false".into()) },
        Kind::Int(lo, hi) => match v.as_i64() {
            Some(n) if n >= *lo && n <= *hi => Ok(()),
            _ => Err(format!("must be a whole number between {lo} and {hi}")),
        },
        Kind::Num => match v.as_f64() {
            Some(n) if n.is_finite() && (0.0..=100_000.0).contains(&n) => {
                if key.starts_with("lab.ph") && n > 14.0 { Err("pH must be between 0 and 14".into()) } else { Ok(()) }
            }
            _ => Err("must be a number between 0 and 100000".into()),
        },
        Kind::Str => {
            let Some(s) = v.as_str() else { return Err("must be text".into()) };
            if s.chars().any(|c| c.is_control()) { return Err("contains invalid characters".into()); }
            match key {
                "plant.name" => if s.trim().is_empty() || s.chars().count() > 120 { Err("must be 1-120 characters".into()) } else { Ok(()) },
                "plant.code" => if (1..=12).contains(&s.chars().count()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { Ok(()) }
                                else { Err("must be 1-12 characters: letters, digits, '-' or '_'".into()) },
                "plant.qr_base_url" => {
                    if s.is_empty() { return Ok(()); }
                    let ok = s.len() <= 200 && (s.starts_with("https://") || s.starts_with("http://")) && s.len() > 8
                        && s.chars().all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '<' | '>' | '\\' | '`' | '{' | '}' | '|' | '^'))
                        && !s.contains('?') && !s.contains('#') && !s.contains('@');
                    if ok { Ok(()) } else { Err("must be empty or a plain http(s) URL such as https://erp.example.com (no query, no credentials)".into()) }
                }
                _ => Ok(()),
            }
        }
    }
}

fn as_f64(v: &Value) -> f64 { v.as_f64().unwrap_or(0.0) }

async fn put_settings(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiJson(body): ApiJson<Map<String, Value>>) -> AppResult<Json<Value>> {
    if body.is_empty() { return Err(AppError::bad("No settings supplied")); }
    if body.len() > 60 { return Err(AppError::bad("Too many settings in one request")); }
    // 1. validate everything first (collect all field errors)
    let mut errs: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in &body {
        match SCHEMA.iter().find(|(sk, _)| sk == k) {
            None => { errs.insert(k.chars().take(80).collect(), "unknown setting".into()); }
            Some((_, kind)) => if let Err(m) = check_value(k, kind, v) { errs.insert(k.clone(), m); },
        }
    }
    // cross-field rules against the *resulting* state
    let current: BTreeMap<String, Value> = settings::list(&st.db).await?.into_iter().map(|(k, v, _)| (k, v)).collect();
    let merged = |k: &str| body.get(k).or_else(|| current.get(k)).cloned().unwrap_or(Value::Null);
    if errs.is_empty() {
        if as_f64(&merged("lab.ph_min")) >= as_f64(&merged("lab.ph_max")) { errs.insert("lab.ph_min".into(), "must be lower than lab.ph_max".into()); }
        if as_f64(&merged("lab.tds_min")) >= as_f64(&merged("lab.tds_max")) { errs.insert("lab.tds_min".into(), "must be lower than lab.tds_max".into()); }
    }
    if !errs.is_empty() { return Err(AppError::Validation(errs)); }

    // 2. apply serially (advisory lock) with rollback to the snapshot if anything still fails => all-or-nothing
    let mut lock = st.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('system_settings_update'))").execute(&mut *lock).await?;
    let mut changes = Map::new();
    let mut applied: Vec<(String, Value)> = vec![];
    let mut failure: Option<AppError> = None;
    for (k, v) in &body {
        let old = current.get(k).cloned().unwrap_or(Value::Null);
        if &old == v { continue; }
        match settings::update(&st.db, &st.settings, k, v.clone(), Some(actor.id)).await {
            Ok(()) => { applied.push((k.clone(), old.clone())); changes.insert(k.clone(), json!({ "old": old, "new": v })); }
            Err(e) => { failure = Some(e); break; }
        }
    }
    if let Some(e) = failure {
        for (k, old) in applied.iter().rev() {
            let _ = sqlx::query("UPDATE system_settings SET value = $2, updated_at = now() WHERE key = $1").bind(k).bind(old).execute(&st.db).await;
        }
        let _ = st.settings.reload(&st.db).await;
        return Err(e);
    }
    if !changes.is_empty() {
        let mut c = st.db.acquire().await?;
        audit(&mut c, &actor, &ci, "SETTINGS_UPDATE", "settings", None, json!({ "changes": changes })).await?;
    }
    lock.commit().await?;
    get_settings(State(st)).await
}

// ---- audit -----------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct AuditQuery {
    user: Option<String>, action: Option<String>, entity: Option<String>,
    from: Option<NaiveDate>, to: Option<NaiveDate>,
    limit: Option<i64>, offset: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
struct AuditRow {
    id: i64, at: DateTime<Utc>, user_id: Option<Uuid>, username: Option<String>,
    action: String, entity: String, entity_id: Option<String>, details: Value, ip: Option<String>,
}

fn opt_pattern(field: &str, v: &Option<String>) -> AppResult<Option<String>> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Ok(Some(validate::like_pattern(&validate::text(field, s, 100)?))),
        None => Ok(None),
    }
}

async fn audit_query(State(st): State<AppState>, ApiQuery(q): ApiQuery<AuditQuery>) -> AppResult<Json<Paged<AuditRow>>> {
    let pq = validate::PageQuery { limit: q.limit, offset: q.offset };
    let (user_id, user_pat) = match q.user.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => match Uuid::parse_str(s) {
            Ok(id) => (Some(id), None),
            Err(_) => (None, Some(validate::like_pattern(&validate::text("user", s, 100)?))),
        },
        None => (None, None),
    };
    let (action, entity) = (opt_pattern("action", &q.action)?, opt_pattern("entity", &q.entity)?);
    // dates are plant-local days
    let off = Duration::minutes(st.settings.get().utc_offset_minutes);
    let day_start = |d: NaiveDate| -> DateTime<Utc> { DateTime::from_naive_utc_and_offset(d.and_hms_opt(0, 0, 0).unwrap() - off, Utc) };
    let from = q.from.map(day_start);
    let to = q.to.map(|d| day_start(d) + Duration::days(1));
    const WHERE: &str = "WHERE ($1::uuid IS NULL OR user_id = $1) AND ($2::text IS NULL OR username ILIKE $2)
        AND ($3::text IS NULL OR action ILIKE $3) AND ($4::text IS NULL OR entity ILIKE $4)
        AND ($5::timestamptz IS NULL OR at >= $5) AND ($6::timestamptz IS NULL OR at < $6)";
    let total: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM audit_logs {WHERE}"))
        .bind(user_id).bind(&user_pat).bind(&action).bind(&entity).bind(from).bind(to).fetch_one(&st.db).await?;
    let items = sqlx::query_as::<_, AuditRow>(&format!(
        "SELECT id, at, user_id, username, action, entity, entity_id, details, ip FROM audit_logs {WHERE} ORDER BY id DESC LIMIT $7 OFFSET $8"))
        .bind(user_id).bind(&user_pat).bind(&action).bind(&entity).bind(from).bind(to).bind(pq.limit()).bind(pq.offset())
        .fetch_all(&st.db).await?;
    Ok(Json(Paged { items, total }))
}
