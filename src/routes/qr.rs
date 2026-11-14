//! OPTIONAL QR crate-set labels (`features.qr_labels`, default off; the router answers `FEATURE_DISABLED` while off).
//! Pure-Rust SVG QR codes, set generation with the partition invariant, search/lookup, printable sheet, and the set
//! state-machine hooks that the loading flows call for trip items with a `qr_set_id`.
//!
//! Set status machine (each transition under `FOR UPDATE`, batch row locked first):
//! `QUARANTINE->AVAILABLE` (QC approve, QC module) · `QUARANTINE|AVAILABLE->REJECTED` (QC reject, QC module) ·
//! `AVAILABLE->RESERVED` (added to trip) · `QUARANTINE->RESERVED` (only when `strict_qc_lock=false`) ·
//! `RESERVED->AVAILABLE|QUARANTINE` (removed from trip / trip cancelled) · `RESERVED->LOADED` (gate pass) ·
//! `LOADED->DELIVERED` (settle, nothing returned) · `LOADED->AVAILABLE` (settle with returns, `crates = returned`) ·
//! `LOADED->AVAILABLE|QUARANTINE` (gate-pass void).
use crate::{
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    routes::loading::common::*,
    state::AppState,
    validate,
};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get},
    Json, Router,
};
use chrono::{DateTime, NaiveDate, Utc};
use qrcode_generator::QrCodeEcc;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/batches/{batch_id}/sets", get(batch_sets).post(generate))
        .route("/batches/{batch_id}/sheet", get(sheet))
        .route("/sets", get(search))
        .route("/sets/lookup/{code}", get(lookup))
        .route("/sets/{id}", delete(delete_set))
        .route("/sets/{id}/qr", get(qr_of))
}

// ------------------------------------------------------------------------------------------------ state-machine hooks

/// Status a free set takes, following its batch's QC state.
pub fn free_status(qc_status: &str) -> &'static str {
    match qc_status { "APPROVED" => "AVAILABLE", "QUARANTINE" => "QUARANTINE", _ => "REJECTED" }
}

#[derive(FromRow, Debug)]
struct SetLock { status: String, trip_id: Option<Uuid> }

/// Lock a set. Lock order: batch (shared) first, then the set, to match QC decisions (batch -> sets).
async fn lock_set(tx: &mut PgConnection, set_id: Uuid) -> AppResult<(SetLock, String)> {
    let batch: Uuid = sqlx::query_scalar("SELECT batch_id FROM qr_crate_sets WHERE id = $1").bind(set_id).fetch_optional(&mut *tx).await?
        .ok_or_else(|| AppError::not_found("QR set"))?;
    let qc: String = sqlx::query_scalar("SELECT qc_status FROM production_batches WHERE id = $1 FOR SHARE").bind(batch).fetch_one(&mut *tx).await?;
    let s = sqlx::query_as::<_, SetLock>("SELECT status, trip_id FROM qr_crate_sets WHERE id = $1 FOR UPDATE").bind(set_id).fetch_one(&mut *tx).await?;
    Ok((s, qc))
}

/// Trip item removed / trip cancelled: `RESERVED -> AVAILABLE|QUARANTINE` (whatever the batch's QC state implies).
pub async fn release_set(tx: &mut PgConnection, set_id: Uuid) -> AppResult<()> {
    let (s, qc) = lock_set(tx, set_id).await?;
    if s.status == "RESERVED" {
        sqlx::query("UPDATE qr_crate_sets SET status = $2, trip_id = NULL WHERE id = $1").bind(set_id).bind(free_status(&qc)).execute(&mut *tx).await?;
    }
    Ok(())
}

/// Gate pass: the set must still be RESERVED by this very trip, then `RESERVED -> LOADED`.
pub async fn on_gate_pass(tx: &mut PgConnection, set_id: Uuid, trip_id: Uuid) -> AppResult<()> {
    let (s, _) = lock_set(tx, set_id).await?;
    if s.status != "RESERVED" || s.trip_id != Some(trip_id) {
        return Err(AppError::conflict_with("INVALID_STATE", "A QR set of this trip is no longer reserved for it - remove it and add it again",
            json!({ "set_id": set_id, "status": s.status })));
    }
    sqlx::query("UPDATE qr_crate_sets SET status = 'LOADED', loaded_at = now() WHERE id = $1").bind(set_id).execute(&mut *tx).await?;
    Ok(())
}

/// Gate-pass void: `LOADED -> AVAILABLE|QUARANTINE`.
pub async fn on_void(tx: &mut PgConnection, set_id: Uuid, trip_id: Uuid) -> AppResult<()> {
    let (s, qc) = lock_set(tx, set_id).await?;
    if s.status == "LOADED" && s.trip_id == Some(trip_id) {
        sqlx::query("UPDATE qr_crate_sets SET status = $2, trip_id = NULL, loaded_at = NULL WHERE id = $1").bind(set_id).bind(free_status(&qc)).execute(&mut *tx).await?;
    }
    Ok(())
}

/// Settlement: nothing returned -> `DELIVERED`; otherwise `AVAILABLE` with `crates = returned`.
pub async fn on_settle(tx: &mut PgConnection, set_id: Uuid, trip_id: Uuid, returned: i64) -> AppResult<()> {
    let (s, qc) = lock_set(tx, set_id).await?;
    if s.status != "LOADED" || s.trip_id != Some(trip_id) {
        return Err(AppError::conflict_with("INVALID_STATE", "A QR set of this trip is not in LOADED state", json!({ "set_id": set_id, "status": s.status })));
    }
    if returned == 0 {
        sqlx::query("UPDATE qr_crate_sets SET status = 'DELIVERED' WHERE id = $1").bind(set_id).execute(&mut *tx).await?;
    } else {
        sqlx::query("UPDATE qr_crate_sets SET status = $2, crates = $3, trip_id = NULL WHERE id = $1")
            .bind(set_id).bind(free_status(&qc)).bind(returned as i32).execute(&mut *tx).await?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------ codes, labels, SVG

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn new_code() -> String {
    let mut b = [0u8; 10];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b.iter().map(|x| CROCKFORD[(*x & 31) as usize] as char).collect()
}

/// `X,Y,Z,A,B,...,W` for the first 26 root sets, then `AA,AB,...` (bijective base-26).
pub fn label_for(index: usize) -> String {
    if index < 26 { return ((b'A' + ((index as u8 + 23) % 26)) as char).to_string(); }
    let mut n = index - 26;
    let mut len = 2usize;
    loop {
        let span = 26usize.pow(len as u32);
        if n < span { break; }
        n -= span;
        len += 1;
    }
    let mut out = vec![b'A'; len];
    for i in (0..len).rev() { out[i] = b'A' + (n % 26) as u8; n /= 26; }
    String::from_utf8(out).unwrap_or_default()
}

fn svg_for(url: &str) -> AppResult<String> {
    let s = qrcode_generator::to_svg_to_string(url, QrCodeEcc::Medium, 256, None::<&str>).map_err(|_| AppError::bad("The QR code could not be rendered"))?;
    // strip an XML prolog so the string can be inlined into an HTML page
    Ok(match s.find("<svg") { Some(i) => s[i..].to_string(), None => s })
}

fn qr_url(st: &AppState, headers: &HeaderMap, code: &str) -> String { format!("{}/loading.html?qr={code}", st.base_url(headers)) }

// ------------------------------------------------------------------------------------------------ SetObj

#[derive(FromRow)]
struct SetRow {
    id: Uuid, code: String, set_label: String, seq: i32, batch_id: Uuid, batch_no: String, product_id: Uuid, product_name: String,
    units_per_crate: i32, crates_per_pallet: Option<i32>, crates: i32, original_crates: i32, status: String, qc_status: String,
    parent_set_id: Option<Uuid>, trip_id: Option<Uuid>, trip_no: Option<String>, expiry_date: NaiveDate, created_at: DateTime<Utc>,
}
const SET_SELECT: &str =
    "SELECT s.id, s.code, s.set_label, s.seq, s.batch_id, b.batch_no, s.product_id, p.name AS product_name, p.units_per_crate, p.crates_per_pallet,
            s.crates, s.original_crates, s.status, b.qc_status, s.parent_set_id, s.trip_id, t.trip_no, b.expiry_date, s.created_at
     FROM qr_crate_sets s JOIN production_batches b ON b.id = s.batch_id JOIN products p ON p.id = s.product_id
     LEFT JOIN loading_trips t ON t.id = s.trip_id";

fn set_obj(r: &SetRow, strict: bool, url: &str, svg: Option<String>) -> Value {
    let (loadable, reason): (bool, Option<String>) = match (r.status.as_str(), r.qc_status.as_str()) {
        ("AVAILABLE", "APPROVED") => (true, None),
        ("QUARANTINE", "QUARANTINE") if !strict => (true, None),
        ("QUARANTINE", _) | ("AVAILABLE", _) if r.qc_status != "REJECTED" => (false, Some("Batch is not approved by Lab QC yet".into())),
        ("RESERVED", _) => (false, Some(format!("Already reserved by trip {}", r.trip_no.clone().unwrap_or_default()))),
        ("LOADED", _) => (false, Some(format!("Already loaded on trip {}", r.trip_no.clone().unwrap_or_default()))),
        ("DELIVERED", _) => (false, Some("Already delivered".into())),
        _ => (false, Some("Batch was rejected by Lab QC".into())),
    };
    let cpp = r.crates_per_pallet.map(|c| c as i64);
    let mut v = json!({
        "id": r.id, "code": r.code, "set_label": r.set_label, "seq": r.seq, "batch_id": r.batch_id, "batch_no": r.batch_no,
        "product_id": r.product_id, "product_name": r.product_name, "units_per_crate": r.units_per_crate, "crates": r.crates,
        "original_crates": r.original_crates, "status": r.status, "qc_status": r.qc_status, "parent_set_id": r.parent_set_id,
        "trip_id": r.trip_id, "trip_no": r.trip_no, "expiry_date": r.expiry_date, "loadable": loadable, "loadable_reason": reason,
        "qr_url": url, "pallets": cpp.map(|c| r.crates as i64 / c), "loose_crates": cpp.map(|c| r.crates as i64 % c), "created_at": r.created_at,
    });
    if let Some(s) = svg { v["svg"] = json!(s); }
    v
}

async fn fetch_sets(conn: &mut PgConnection, st: &AppState, headers: &HeaderMap, ids: &[Uuid], with_svg: bool) -> AppResult<Vec<Value>> {
    let rows = sqlx::query_as::<_, SetRow>(&format!("{SET_SELECT} WHERE s.id = ANY($1) ORDER BY b.created_at, s.seq, s.created_at, s.id")).bind(ids).fetch_all(&mut *conn).await?;
    let strict = st.settings.get().strict_qc_lock;
    rows.iter().map(|r| {
        let url = qr_url(st, headers, &r.code);
        let svg = if with_svg { Some(svg_for(&url)?) } else { None };
        Ok(set_obj(r, strict, &url, svg))
    }).collect()
}

fn require_labels_page(user: &AuthUser) -> AppResult<()> {
    if user.can_access("qr-labels") { Ok(()) } else { Err(AppError::forbidden("Creating or deleting QR labels needs access to the QR Labels page")) }
}

// ------------------------------------------------------------------------------------------------ generation

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenBody {
    mode: String,
    #[serde(default, deserialize_with = "de_opt_i64")] set_size: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64")] count: Option<i64>,
    sizes: Option<Vec<i64>>,
}

const MAX_SETS_PER_CALL: i64 = 500;

pub async fn generate(State(st): State<AppState>, user: AuthUser, headers: HeaderMap, ids: Ids, J(b): J<GenBody>) -> AppResult<Response> {
    require_labels_page(&user)?;
    let batch_id = *ids.0.first().ok_or_else(|| AppError::bad("Missing batch id"))?;
    let mut tx = st.db.begin().await?;
    let bt: Option<(String, Uuid, i32, String, Option<i32>, Uuid)> = sqlx::query_as(
        "SELECT b.batch_no, b.product_id, b.crates, b.qc_status, p.crates_per_pallet, b.location_id FROM production_batches b JOIN products p ON p.id = b.product_id
         WHERE b.id = $1 FOR UPDATE OF b").bind(batch_id).fetch_optional(&mut *tx).await?;
    let (batch_no, product_id, batch_crates, qc, cpp, batch_loc) = bt.ok_or_else(|| AppError::not_found("Batch"))?;
    user.require_location(batch_loc)?;
    if qc == "REJECTED" {
        return Err(AppError::conflict_with("QC_LOCKED", format!("Batch {batch_no} was rejected by Lab QC - no labels can be created"), json!({ "batch_id": batch_id, "qc_status": qc })));
    }
    let (roots_sum, max_seq): (i64, i32) = sqlx::query_as(
        "SELECT COALESCE(SUM(original_crates),0)::bigint, COALESCE(MAX(seq),0) FROM qr_crate_sets WHERE batch_id = $1 AND parent_set_id IS NULL")
        .bind(batch_id).fetch_one(&mut *tx).await?;
    let remaining = batch_crates as i64 - roots_sum;
    if remaining <= 0 { return Err(AppError::bad(format!("All {batch_crates} crates of batch {batch_no} already have labels"))); }

    let sizes: Vec<i64> = match b.mode.as_str() {
        "UNIFORM" => {
            let n = b.set_size.unwrap_or_else(|| cpp.map(|c| c as i64).unwrap_or(st.settings.get().default_crate_set_size));
            validate::count("set_size", n)?;
            let mut v = vec![n; (remaining / n) as usize];
            if remaining % n > 0 { v.push(remaining % n); }
            v
        }
        "COUNT" => {
            let c = b.count.ok_or_else(|| AppError::field("count", "is required for mode COUNT"))?;
            if c < 1 || c > remaining.min(MAX_SETS_PER_CALL) { return Err(AppError::field("count", &format!("must be between 1 and {}", remaining.min(MAX_SETS_PER_CALL)))); }
            let (base, extra) = (remaining / c, remaining % c);
            (0..c).map(|i| if i < extra { base + 1 } else { base }).collect()
        }
        "CUSTOM" => {
            let v = b.sizes.clone().ok_or_else(|| AppError::field("sizes", "is required for mode CUSTOM"))?;
            if v.is_empty() { return Err(AppError::field("sizes", "give at least one size")); }
            if v.iter().any(|x| *x < 1 || *x > 1_000_000) { return Err(AppError::field("sizes", "every size must be a whole number of crates >= 1")); }
            let sum: i64 = v.iter().sum();
            if sum > remaining {
                return Err(AppError::bad(format!("Sizes add up to {sum} crates but only {remaining} of batch {batch_no} are not labelled yet")));
            }
            v
        }
        _ => return Err(AppError::field("mode", "must be UNIFORM, COUNT or CUSTOM")),
    };
    if sizes.len() as i64 > MAX_SETS_PER_CALL {
        return Err(AppError::field("sizes", &format!("at most {MAX_SETS_PER_CALL} sets per request - use a bigger set size")));
    }
    let status = if qc == "APPROVED" { "AVAILABLE" } else { "QUARANTINE" };
    let mut created = vec![];
    for (i, size) in sizes.iter().enumerate() {
        let seq = max_seq + 1 + i as i32;
        let label = label_for((seq - 1) as usize);
        let mut id = None;
        for _ in 0..8 {
            let r: Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO qr_crate_sets (code, batch_id, product_id, set_label, seq, original_crates, crates, status, created_by)
                 VALUES ($1,$2,$3,$4,$5,$6,$6,$7,$8) ON CONFLICT (code) DO NOTHING RETURNING id")
                .bind(new_code()).bind(batch_id).bind(product_id).bind(&label).bind(seq).bind(*size as i32).bind(status).bind(user.id)
                .fetch_optional(&mut *tx).await?;
            if r.is_some() { id = r; break; }
        }
        created.push(id.ok_or_else(|| AppError::Internal(anyhow::anyhow!("could not allocate a unique QR code")))?);
    }
    let out = fetch_sets(&mut tx, &st, &headers, &created, true).await?;
    audit::log(&mut *tx, Some(&user), "QR_SETS_CREATED", "production_batch", Some(batch_id.to_string()),
        json!({ "batch_no": batch_no, "mode": b.mode, "sets": sizes.len(), "crates": sizes.iter().sum::<i64>() })).await;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(Value::Array(out))).into_response())
}

// ------------------------------------------------------------------------------------------------ read endpoints

/// 403 unless the caller may use the godown that produced the batch (404 for an unknown batch).
async fn require_batch_location(conn: &mut PgConnection, user: &AuthUser, batch_id: Uuid) -> AppResult<()> {
    let loc: Uuid = sqlx::query_scalar("SELECT location_id FROM production_batches WHERE id = $1").bind(batch_id).fetch_optional(&mut *conn).await?
        .ok_or_else(|| AppError::not_found("Batch"))?;
    user.require_location(loc)
}

pub async fn batch_sets(State(st): State<AppState>, user: AuthUser, headers: HeaderMap, ids: Ids) -> AppResult<Json<Value>> {
    let batch_id = *ids.0.first().ok_or_else(|| AppError::bad("Missing batch id"))?;
    let mut conn = st.db.acquire().await?;
    require_batch_location(&mut conn, &user, batch_id).await?;
    let found: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM qr_crate_sets WHERE batch_id = $1 ORDER BY seq, created_at, id LIMIT 1000").bind(batch_id).fetch_all(&mut *conn).await?;
    Ok(Json(Value::Array(fetch_sets(&mut conn, &st, &headers, &found, false).await?)))
}

pub async fn sheet(State(st): State<AppState>, user: AuthUser, headers: HeaderMap, ids: Ids) -> AppResult<Json<Value>> {
    let batch_id = *ids.0.first().ok_or_else(|| AppError::bad("Missing batch id"))?;
    let mut conn = st.db.acquire().await?;
    require_batch_location(&mut conn, &user, batch_id).await?;
    let b: Option<(String, Uuid, i32, String, NaiveDate, NaiveDate, String, i32, Option<i32>)> = sqlx::query_as(
        "SELECT b.batch_no, b.product_id, b.crates, b.qc_status, b.mfg_date, b.expiry_date, p.name, p.units_per_crate, p.crates_per_pallet
         FROM production_batches b JOIN products p ON p.id = b.product_id WHERE b.id = $1").bind(batch_id).fetch_optional(&mut *conn).await?;
    let (batch_no, product_id, crates, qc, mfg, exp, pname, upc, cpp) = b.ok_or_else(|| AppError::not_found("Batch"))?;
    let found: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM qr_crate_sets WHERE batch_id = $1 ORDER BY seq, created_at, id LIMIT 1000").bind(batch_id).fetch_all(&mut *conn).await?;
    let sets = fetch_sets(&mut conn, &st, &headers, &found, true).await?;
    Ok(Json(json!({
        "plant_name": st.settings.get().plant_name,
        "batch": { "id": batch_id, "batch_no": batch_no, "crates": crates, "qc_status": qc, "mfg_date": mfg, "expiry_date": exp },
        "product": { "id": product_id, "name": pname, "units_per_crate": upc, "crates_per_pallet": cpp },
        "sets": sets,
    })))
}

#[derive(Deserialize, Default)]
pub struct SearchQ { status: Option<String>, batch_id: Option<Uuid>, q: Option<String>, limit: Option<i64>, offset: Option<i64> }

pub async fn search(State(st): State<AppState>, user: AuthUser, headers: HeaderMap, Q(q): Q<SearchQ>) -> AppResult<Json<Value>> {
    let scope: Option<Vec<Uuid>> = location_scope(&user);
    let status = match q.status.as_deref().map(str::trim) {
        None | Some("") | Some("ALL") => None,
        Some(s) if ["QUARANTINE", "AVAILABLE", "RESERVED", "LOADED", "DELIVERED", "REJECTED"].contains(&s) => Some(s.to_string()),
        Some(_) => return Err(AppError::field("status", "unknown set status")),
    };
    let pat = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => { validate::text("q", s, 100)?; Some(validate::like_pattern(s)) }
        None => None,
    };
    let page = validate::PageQuery { limit: q.limit, offset: q.offset };
    let filter = "WHERE ($1::text IS NULL OR s.status = $1) AND ($2::uuid IS NULL OR s.batch_id = $2)
                  AND ($3::text IS NULL OR s.code ILIKE $3 OR s.set_label ILIKE $3 OR b.batch_no ILIKE $3)
                  AND ($6::uuid[] IS NULL OR b.location_id = ANY($6))";
    let rows = sqlx::query_as::<_, SetRow>(&format!("{SET_SELECT} {filter} ORDER BY b.created_at DESC, s.seq ASC, s.id ASC LIMIT $4 OFFSET $5"))
        .bind(&status).bind(q.batch_id).bind(&pat).bind(page.limit()).bind(page.offset()).bind(&scope).fetch_all(&st.db).await?;
    let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM qr_crate_sets s JOIN production_batches b ON b.id = s.batch_id {filter}"))
        .bind(&status).bind(q.batch_id).bind(&pat).bind(0i64).bind(0i64).bind(&scope).fetch_one(&st.db).await?;
    let strict = st.settings.get().strict_qc_lock;
    let items: Vec<Value> = rows.iter().map(|r| set_obj(r, strict, &qr_url(&st, &headers, &r.code), None)).collect();
    Ok(Json(json!({ "items": items, "total": total })))
}

pub async fn lookup(State(st): State<AppState>, user: AuthUser, headers: HeaderMap, axum::extract::Path(code): axum::extract::Path<String>) -> AppResult<Json<Value>> {
    let code = code.trim().to_ascii_uppercase();
    if code.is_empty() || code.len() > 32 || !code.chars().all(|c| c.is_ascii_alphanumeric()) { return Err(AppError::not_found("QR set")); }
    let mut conn = st.db.acquire().await?;
    let row = sqlx::query_as::<_, SetRow>(&format!("{SET_SELECT} WHERE s.code = $1")).bind(&code).fetch_optional(&mut *conn).await?.ok_or_else(|| AppError::not_found("QR set"))?;
    require_batch_location(&mut conn, &user, row.batch_id).await?;
    let strict = st.settings.get().strict_qc_lock;
    Ok(Json(set_obj(&row, strict, &qr_url(&st, &headers, &row.code), None)))
}

pub async fn qr_of(State(st): State<AppState>, user: AuthUser, headers: HeaderMap, ids: Ids) -> AppResult<Json<Value>> {
    let id = *ids.0.first().ok_or_else(|| AppError::bad("Missing id"))?;
    let (code, batch_id): (String, Uuid) = sqlx::query_as("SELECT code, batch_id FROM qr_crate_sets WHERE id = $1").bind(id).fetch_optional(&st.db).await?.ok_or_else(|| AppError::not_found("QR set"))?;
    require_batch_location(&mut *st.db.acquire().await?, &user, batch_id).await?;
    let url = qr_url(&st, &headers, &code);
    Ok(Json(json!({ "code": code, "url": url, "svg": svg_for(&url)? })))
}

/// Delete an unused label: root set, never loaded, no children, never on a trip, status QUARANTINE/AVAILABLE (frees the allocation).
pub async fn delete_set(State(st): State<AppState>, user: AuthUser, ids: Ids) -> AppResult<Response> {
    require_labels_page(&user)?;
    let id = *ids.0.first().ok_or_else(|| AppError::bad("Missing id"))?;
    let mut tx = st.db.begin().await?;
    let batch: Uuid = sqlx::query_scalar("SELECT batch_id FROM qr_crate_sets WHERE id = $1").bind(id).fetch_optional(&mut *tx).await?.ok_or_else(|| AppError::not_found("QR set"))?;
    let bloc: Uuid = sqlx::query_scalar("SELECT location_id FROM production_batches WHERE id = $1 FOR UPDATE").bind(batch).fetch_one(&mut *tx).await?;
    user.require_location(bloc)?;
    let s: Option<(String, Option<Uuid>, String, i32, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT status, parent_set_id, set_label, original_crates, loaded_at FROM qr_crate_sets WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    let (status, parent, label, crates, loaded_at) = s.ok_or_else(|| AppError::not_found("QR set"))?;
    let children: i64 = sqlx::query_scalar("SELECT count(*) FROM qr_crate_sets WHERE parent_set_id = $1").bind(id).fetch_one(&mut *tx).await?;
    let used: i64 = sqlx::query_scalar("SELECT count(*) FROM trip_items WHERE qr_set_id = $1").bind(id).fetch_one(&mut *tx).await?;
    if parent.is_some() || children > 0 || used > 0 || loaded_at.is_some() || !(status == "QUARANTINE" || status == "AVAILABLE") {
        return Err(AppError::conflict_with("INVALID_STATE", format!("Label {label} was already used on a trip (or is a split label) and cannot be deleted"), json!({ "status": status })));
    }
    sqlx::query("DELETE FROM qr_crate_sets WHERE id = $1").bind(id).execute(&mut *tx).await?;
    audit::log(&mut *tx, Some(&user), "QR_SET_DELETED", "qr_crate_set", Some(id.to_string()), json!({ "batch_id": batch, "label": label, "crates": crates })).await;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ------------------------------------------------------------------------------------------------ add a set to a trip

#[derive(FromRow)]
struct SetForLoad { id: Uuid, batch_id: Uuid, product_id: Uuid, set_label: String, crates: i32, status: String }

/// `POST /api/loading/trips/{id}/items {qr, crates?}` (feature on). Trip is already locked by the caller.
pub async fn add_set_to_trip(tx: &mut PgConnection, st: &AppState, headers: &HeaderMap, _user: &AuthUser, trip: &TripLock, code_or_id: &str, crates: Option<i64>, fx: &mut Fx) -> AppResult<Value> {
    let key = code_or_id.trim();
    if key.is_empty() || key.len() > 64 { return Err(AppError::field("qr", "is not a valid label code")); }
    let sid: Option<Uuid> = match Uuid::parse_str(key) {
        Ok(u) => sqlx::query_scalar("SELECT id FROM qr_crate_sets WHERE id = $1").bind(u).fetch_optional(&mut *tx).await?,
        Err(_) => sqlx::query_scalar("SELECT id FROM qr_crate_sets WHERE code = $1").bind(key.to_ascii_uppercase()).fetch_optional(&mut *tx).await?,
    };
    let sid = sid.ok_or_else(|| AppError::not_found("QR label"))?;
    // lock order: batch -> set
    let batch_id: Uuid = sqlx::query_scalar("SELECT batch_id FROM qr_crate_sets WHERE id = $1").bind(sid).fetch_one(&mut *tx).await?;
    let batches = lock_batches(tx, &[batch_id]).await?;
    let bt = batches.first().ok_or_else(|| AppError::not_found("Batch"))?.clone();
    let set = sqlx::query_as::<_, SetForLoad>(
        "SELECT id, batch_id, product_id, set_label, crates, status FROM qr_crate_sets WHERE id = $1 FOR UPDATE")
        .bind(sid).fetch_one(&mut *tx).await?;
    let strict = st.settings.get().strict_qc_lock;
    // the batch is the source of truth for QC (the set status may lag behind a concurrent decision)
    let zone = load_zone(&bt, strict)?;
    match set.status.as_str() {
        "AVAILABLE" | "QUARANTINE" => {}
        "REJECTED" => return Err(qc_locked(&bt)),
        other => return Err(invalid_state(format!("Label {} is {other} and cannot be added to a trip", set.set_label), other)),
    }
    let n = crates.unwrap_or(set.crates as i64);
    if n < 1 || n > set.crates as i64 { return Err(AppError::field("crates", &format!("must be between 1 and {} (the crates on this label)", set.crates))); }
    let av = lot_avail(tx, trip.location_id, bt.product_id, bt.id, zone).await?;
    if Decimal::from(n) > av.free { return Err(insufficient(&bt, av.free, n)); }
    let loaded: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(crates),0)::bigint FROM trip_items WHERE trip_id = $1").bind(trip.id).fetch_one(&mut *tx).await?;
    let cap = trip.capacity_crates as i64;
    if loaded + n > cap {
        return Err(AppError::conflict_with("CAPACITY_EXCEEDED", format!("Vehicle capacity is {cap} crates; {loaded} are loaded and you are adding {n}"),
            json!({ "capacity": cap, "loaded": loaded, "adding": n, "over_by": loaded + n - cap })));
    }
    if let Some(w) = capacity_warning(loaded + n, trip.capacity_crates) { fx.warn(w); }
    if bt.qc_status != "APPROVED" { fx.warn(format!("Batch {} is still in QC quarantine - these crates are not yet approved for sale", bt.batch_no)); }

    // partial load: the scanned label keeps `n` crates and travels; the rest becomes a new residual label
    let mut residual: Option<Uuid> = None;
    if n < set.crates as i64 {
        let root_label = set.set_label.split("-R").next().unwrap_or(&set.set_label).to_string();
        let prefix = format!("{root_label}-R");
        let k: i64 = sqlx::query_scalar("SELECT count(*) FROM qr_crate_sets WHERE batch_id = $1 AND left(set_label, length($2)) = $2").bind(set.batch_id).bind(&prefix).fetch_one(&mut *tx).await?;
        let rest = set.crates as i64 - n;
        let seq: i32 = sqlx::query_scalar("SELECT seq FROM qr_crate_sets WHERE id = $1").bind(set.id).fetch_one(&mut *tx).await?;
        for _ in 0..8 {
            let r: Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO qr_crate_sets (code, batch_id, product_id, set_label, seq, parent_set_id, original_crates, crates, status, created_by)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$7,$8,$9) ON CONFLICT (code) DO NOTHING RETURNING id")
                .bind(new_code()).bind(set.batch_id).bind(set.product_id).bind(format!("{prefix}{}", k + 1)).bind(seq).bind(set.id)
                .bind(rest as i32).bind(free_status(&bt.qc_status)).bind(_user.id).fetch_optional(&mut *tx).await?;
            if r.is_some() { residual = r; break; }
        }
        if residual.is_none() { return Err(AppError::Internal(anyhow::anyhow!("could not allocate a unique QR code"))); }
    }
    sqlx::query("UPDATE qr_crate_sets SET status = 'RESERVED', trip_id = $2, crates = $3 WHERE id = $1").bind(set.id).bind(trip.id).bind(n as i32).execute(&mut *tx).await?;
    let item_id: Uuid = sqlx::query_scalar("INSERT INTO trip_items (trip_id, qr_set_id, batch_id, product_id, crates) VALUES ($1,$2,$3,$4,$5) RETURNING id")
        .bind(trip.id).bind(set.id).bind(set.batch_id).bind(set.product_id).bind(n as i32).fetch_one(&mut *tx).await?;
    fx.effects.push(json!({ "item": format!("{} · {} · label {}", bt.product_name, bt.batch_no, set.set_label), "zone": zone, "delta": "0", "after": dec_str(av.on_hand),
        "note": format!("reserves {n} crates for trip {}", trip.trip_no) }));
    let items = items_by_ids(tx, &[item_id]).await?;
    let trip_v = trip_json(tx, trip.id, true).await?;
    let residual_v = match residual {
        Some(rid) => {
            fetch_sets(tx, st, headers, &[rid], true).await?.into_iter().next()
        }
        None => None,
    };
    Ok(json!({ "trip": trip_v, "item": items.first(), "items": items, "residual_set": residual_v }))
}

use rust_decimal::Decimal;
