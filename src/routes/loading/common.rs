//! Shared plumbing for the loading / dispatch / settlement module (and the optional QR module):
//! JSON-envelope extractors, dry-run effects, lot locking, reservation-aware availability, trip views.
use crate::{
    error::{AppError, AppResult},
    stock::{self, ItemType, Move},
};
use axum::{
    body::Bytes,
    extract::{FromRequest, FromRequestParts, Query, RawPathParams, Request},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgConnection, Postgres, Transaction};
use std::collections::HashMap;
use uuid::Uuid;

// ---------------------------------------------------------------------------------------------------------------
// Extractors that fail with the standard error envelope instead of axum's plain-text rejections.
// ---------------------------------------------------------------------------------------------------------------

/// JSON body. An empty body is treated as `{}` so optional-body POSTs (`/cancel`) work.
pub struct J<T>(pub T);
impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for J<T> {
    type Rejection = AppError;
    async fn from_request(req: Request, state: &S) -> Result<Self, AppError> {
        let bytes = Bytes::from_request(req, state).await.map_err(|_| AppError::bad("The request body could not be read (too large?)"))?;
        let slice: &[u8] = if bytes.iter().all(|b| b.is_ascii_whitespace()) { b"{}" } else { &bytes };
        serde_json::from_slice(slice).map(J).map_err(|e| AppError::bad(format!("Invalid request body: {e}")))
    }
}

/// Query string.
pub struct Q<T>(pub T);
impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for Q<T> {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, AppError> {
        Query::<T>::try_from_uri(&parts.uri).map(|Query(v)| Q(v)).map_err(|_| AppError::bad("Invalid query string"))
    }
}

/// All path parameters parsed as UUIDs, in route order.
pub struct Ids(pub Vec<Uuid>);
impl<S: Send + Sync> FromRequestParts<S> for Ids {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, AppError> {
        let p = RawPathParams::from_request_parts(parts, state).await.map_err(|_| AppError::bad("Invalid path"))?;
        let mut v = vec![];
        for (_, val) in &p { v.push(Uuid::parse_str(val).map_err(|_| AppError::bad("Invalid id in path"))?); }
        Ok(Ids(v))
    }
}

/// `?dry_run=1` (also `true` or a bare `?dry_run`): run everything, then roll back and report the effects.
#[derive(Deserialize, Default)]
pub struct DryQ { #[serde(default)] pub dry_run: Option<String> }
impl DryQ {
    pub fn on(&self) -> bool { !matches!(self.dry_run.as_deref(), None | Some("0") | Some("false")) }
}

/// Whole number that may arrive as a JSON number or a numeric string.
pub fn de_i64<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V { I(i64), S(String) }
    match V::deserialize(d)? {
        V::I(i) => Ok(i),
        V::S(s) => s.trim().parse::<i64>().map_err(|_| serde::de::Error::custom("must be a whole number")),
    }
}
pub fn de_opt_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V { I(i64), S(String) }
    match Option::<V>::deserialize(d)? {
        None => Ok(None),
        Some(V::I(i)) => Ok(Some(i)),
        Some(V::S(s)) => s.trim().parse::<i64>().map(Some).map_err(|_| serde::de::Error::custom("must be a whole number")),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Dry-run effects + transaction conclusion
// ---------------------------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct Fx { pub effects: Vec<Value>, pub warnings: Vec<String> }
impl Fx {
    pub fn warn(&mut self, w: impl Into<String>) { self.warnings.push(w.into()); }
}

/// Commit (real run) or roll back and describe (dry run). `body` must already have been built inside the transaction.
pub async fn conclude(tx: Transaction<'static, Postgres>, dry: bool, fx: Fx, status: StatusCode, mut body: Value) -> AppResult<Response> {
    if dry {
        tx.rollback().await?;
        return Ok((StatusCode::OK, Json(json!({ "dry_run": true, "effects": fx.effects, "warnings": fx.warnings }))).into_response());
    }
    tx.commit().await?;
    if !fx.warnings.is_empty() { if let Value::Object(m) = &mut body { m.insert("warnings".into(), json!(fx.warnings)); } }
    Ok((status, Json(body)).into_response())
}

pub fn dec_str(d: Decimal) -> String { d.normalize().to_string() }

/// (product_id, lot_id) -> human label used in dry-run effects.
pub type Labels = HashMap<(Uuid, Uuid), String>;

/// Post stock moves in a stable lock order and record one effect row per move.
pub async fn post_moves(tx: &mut PgConnection, mut moves: Vec<Move>, labels: &Labels, fx: &mut Fx) -> AppResult<()> {
    moves.retain(|m| !m.delta.is_zero());
    if moves.is_empty() { return Ok(()); }
    moves.sort_by(|a, b| (a.item_type, a.item_id, a.zone, a.lot_id).cmp(&(b.item_type, b.item_id, b.zone, b.lot_id)));
    let group = Uuid::new_v4();
    for m in &moves {
        let after = stock::post(&mut *tx, m, group).await?;
        let label = labels.get(&(m.item_id, m.lot_id)).cloned().unwrap_or_else(|| m.item_id.to_string());
        fx.effects.push(json!({ "item": label, "zone": m.zone, "delta": dec_str(m.delta), "after": dec_str(after), "txn_type": m.txn_type }));
    }
    Ok(())
}

pub fn product_move(product: Uuid, zone: &'static str, lot: Uuid, delta: i64, txn: &'static str) -> Move {
    Move::new(ItemType::Product, product, zone, lot, Decimal::from(delta), txn)
}

// ---------------------------------------------------------------------------------------------------------------
// Batches: locking, zone rules, reservation-aware availability
// ---------------------------------------------------------------------------------------------------------------

#[derive(FromRow, Clone, Debug)]
pub struct BatchRow {
    pub id: Uuid,
    pub batch_no: String,
    pub product_id: Uuid,
    pub qc_status: String,
    pub expiry_date: NaiveDate,
    pub product_name: String,
}

/// Lock batch rows in the canonical order (expiry, created_at, id) – the same order the FEFO allocator uses – so that
/// concurrent flows can never deadlock on each other. Returns the rows in that order.
pub async fn lock_batches(conn: &mut PgConnection, ids: &[Uuid]) -> AppResult<Vec<BatchRow>> {
    if ids.is_empty() { return Ok(vec![]); }
    let rows = sqlx::query_as::<_, BatchRow>(
        "SELECT b.id, b.batch_no, b.product_id, b.qc_status, b.expiry_date, p.name AS product_name
         FROM production_batches b JOIN products p ON p.id = b.product_id
         WHERE b.id = ANY($1) ORDER BY b.expiry_date ASC, b.created_at ASC, b.id ASC FOR UPDATE OF b")
        .bind(ids).fetch_all(&mut *conn).await?;
    Ok(rows)
}

/// Pre-lock the FEFO candidate batches of a product in a statement of their own *before* calling
/// `stock::allocate_free_fefo`. Under READ COMMITTED the helper's single statement would otherwise resume after a lock wait
/// with a stale snapshot and miss reservations committed by the transaction it waited for.
pub async fn prelock_product_batches(conn: &mut PgConnection, product_id: Uuid) -> AppResult<()> {
    sqlx::query(
        "SELECT b.id FROM production_batches b
         JOIN stock_balances sb ON sb.item_type='PRODUCT' AND sb.item_id=b.product_id AND sb.zone='FG_READY' AND sb.lot_id=b.id
         WHERE b.product_id = $1 AND b.qc_status = 'APPROVED' AND sb.qty > 0
         ORDER BY b.expiry_date ASC, b.created_at ASC, b.id ASC FOR UPDATE OF b")
        .bind(product_id).fetch_all(&mut *conn).await?;
    Ok(())
}

pub fn qc_locked(b: &BatchRow) -> AppError {
    AppError::conflict_with("QC_LOCKED",
        format!("Batch {} is {} - it is not approved by Lab QC and cannot be loaded or dispatched", b.batch_no, b.qc_status),
        json!({ "batch_id": b.id, "batch_no": b.batch_no, "qc_status": b.qc_status }))
}

/// Zone a batch's stock is deducted from when loading/gate-passing. Strict QC lock: APPROVED only.
/// Non-strict: QUARANTINE batches may be loaded straight from `QC_QUARANTINE`. REJECTED is never loadable.
pub fn load_zone(b: &BatchRow, strict: bool) -> AppResult<&'static str> {
    match b.qc_status.as_str() {
        "APPROVED" => Ok("FG_READY"),
        "QUARANTINE" if !strict => Ok("QC_QUARANTINE"),
        _ => Err(qc_locked(b)),
    }
}

/// Where returned / voided crates go back to, following the batch's *current* QC state.
pub fn restock_zone(qc_status: &str) -> &'static str {
    match qc_status { "APPROVED" => "FG_READY", "QUARANTINE" => "QC_QUARANTINE", _ => "SCRAP_YARD" }
}

pub struct Avail { pub on_hand: Decimal, pub free: Decimal }

/// On-hand minus crates reserved by LOADING trips (including the caller's own trip) for the lot in its loadable zone.
/// Call only after the batch row is locked and in a statement of its own.
pub async fn lot_avail(conn: &mut PgConnection, product_id: Uuid, batch_id: Uuid, zone: &str) -> AppResult<Avail> {
    let reserved = stock::fg_reserved(&mut *conn, batch_id).await?;
    let on_hand = if zone == "FG_READY" {
        stock::fg_free(&mut *conn, product_id, batch_id).await? + reserved
    } else {
        sqlx::query_scalar::<_, Decimal>(
            "SELECT COALESCE(SUM(qty),0) FROM stock_balances WHERE item_type='PRODUCT' AND item_id=$1 AND zone=$2 AND lot_id=$3")
            .bind(product_id).bind(zone).bind(batch_id).fetch_one(&mut *conn).await?
    };
    Ok(Avail { on_hand, free: on_hand - reserved })
}

/// The shared FEFO allocator reports decimals as strings; loading answers in whole crates everywhere.
pub fn whole_crates_error(e: AppError) -> AppError {
    match e {
        AppError::Conflict { code: "INSUFFICIENT_STOCK", message, details } => {
            let mut d = details;
            for k in ["available", "required"] {
                let n = d.get(k).and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_i64().map(|i| i.to_string())))
                    .and_then(|s| s.parse::<Decimal>().ok());
                if let Some(n) = n { d[k] = serde_json::json!(floor_i64(n).max(0)); }
            }
            AppError::Conflict { code: "INSUFFICIENT_STOCK", message, details: d }
        }
        other => other,
    }
}

pub fn floor_i64(d: Decimal) -> i64 { d.floor().to_string().parse::<i64>().unwrap_or(0) }

pub fn insufficient(b: &BatchRow, available: Decimal, required: i64) -> AppError {
    let a = floor_i64(available).max(0);
    AppError::conflict_with("INSUFFICIENT_STOCK",
        format!("Only {a} crates of batch {} are free (the rest is reserved by open loading trips or not on hand)", b.batch_no),
        json!({ "available": a, "required": required, "batch_id": b.id, "batch_no": b.batch_no }))
}

// ---------------------------------------------------------------------------------------------------------------
// Trips
// ---------------------------------------------------------------------------------------------------------------

#[derive(FromRow, Clone, Debug)]
pub struct TripLock {
    pub id: Uuid,
    pub trip_no: String,
    pub status: String,
    pub vehicle_id: Uuid,
    pub capacity_crates: i32,
    pub gate_pass_no: Option<String>,
    pub gate_pass_issued_at: Option<DateTime<Utc>>,
}

/// Lock the trip row (`FOR UPDATE`) and return its *current* state. Every state-changing trip flow starts here.
pub async fn lock_trip(conn: &mut PgConnection, id: Uuid) -> AppResult<TripLock> {
    sqlx::query_as::<_, TripLock>(
        "SELECT id, trip_no, status, vehicle_id, capacity_crates, gate_pass_no, gate_pass_issued_at FROM loading_trips WHERE id = $1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *conn).await?.ok_or_else(|| AppError::not_found("Trip"))
}

pub fn invalid_state(msg: impl Into<String>, status: &str) -> AppError {
    AppError::conflict_with("INVALID_STATE", msg, json!({ "status": status }))
}

#[derive(FromRow, Serialize, Clone, Debug)]
pub struct ItemRow {
    pub id: Uuid,
    pub qr_set_id: Option<Uuid>,
    pub code: Option<String>,
    pub set_label: Option<String>,
    pub batch_id: Uuid,
    pub batch_no: String,
    pub expiry_date: NaiveDate,
    pub product_id: Uuid,
    pub product_name: String,
    pub kind: String,
    pub units_per_crate: i32,
    pub crates_per_pallet: Option<i32>,
    pub crates: i32,
    pub crates_delivered: Option<i32>,
    pub crates_returned: Option<i32>,
    pub crates_damaged: Option<i32>,
    pub qc_status: String,
    pub pallets: Option<i32>,
    pub loose_crates: Option<i32>,
}

pub const ITEM_SELECT: &str =
    "SELECT ti.id, ti.qr_set_id, s.code, s.set_label, ti.batch_id, b.batch_no, b.expiry_date, ti.product_id, p.name AS product_name, p.kind,
            p.units_per_crate, p.crates_per_pallet, ti.crates, ti.crates_delivered, ti.crates_returned, ti.crates_damaged, b.qc_status,
            CASE WHEN p.crates_per_pallet IS NULL THEN NULL ELSE ti.crates / p.crates_per_pallet END AS pallets,
            CASE WHEN p.crates_per_pallet IS NULL THEN NULL ELSE ti.crates % p.crates_per_pallet END AS loose_crates
     FROM trip_items ti
     JOIN production_batches b ON b.id = ti.batch_id
     JOIN products p ON p.id = ti.product_id
     LEFT JOIN qr_crate_sets s ON s.id = ti.qr_set_id";

pub async fn trip_items(conn: &mut PgConnection, trip_id: Uuid) -> AppResult<Vec<ItemRow>> {
    Ok(sqlx::query_as::<_, ItemRow>(&format!("{ITEM_SELECT} WHERE ti.trip_id = $1 ORDER BY ti.created_at ASC, b.expiry_date ASC, ti.id ASC"))
        .bind(trip_id).fetch_all(&mut *conn).await?)
}

pub async fn items_by_ids(conn: &mut PgConnection, ids: &[Uuid]) -> AppResult<Vec<ItemRow>> {
    Ok(sqlx::query_as::<_, ItemRow>(&format!("{ITEM_SELECT} WHERE ti.id = ANY($1) ORDER BY ti.created_at ASC, b.expiry_date ASC, ti.id ASC"))
        .bind(ids).fetch_all(&mut *conn).await?)
}

#[derive(FromRow, Serialize, Clone, Debug)]
pub struct TripRow {
    pub id: Uuid,
    pub trip_no: String,
    pub status: String,
    pub vehicle_id: Uuid,
    pub vehicle_reg: String,
    pub route_id: Option<Uuid>,
    pub route_name: Option<String>,
    pub driver_name: String,
    pub capacity_crates: i32,
    pub loaded_crates: i64,
    pub gate_pass_no: Option<String>,
    pub gate_pass_issued_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub settled_at: Option<DateTime<Utc>>,
    pub notes: String,
    pub void_reason: Option<String>,
    pub voided_at: Option<DateTime<Utc>>,
}

pub const TRIP_SELECT: &str =
    "SELECT t.id, t.trip_no, t.status, t.vehicle_id, v.reg_no AS vehicle_reg, t.route_id, r.name AS route_name, t.driver_name, t.capacity_crates,
            COALESCE((SELECT SUM(ti.crates) FROM trip_items ti WHERE ti.trip_id = t.id), 0)::bigint AS loaded_crates,
            t.gate_pass_no, t.gate_pass_issued_at, t.created_at, t.settled_at, t.notes, t.void_reason, t.voided_at
     FROM loading_trips t JOIN vehicles v ON v.id = t.vehicle_id LEFT JOIN routes r ON r.id = t.route_id";

pub fn trip_value(t: &TripRow) -> Value {
    let mut v = serde_json::to_value(t).unwrap_or(Value::Null);
    let pct = if t.capacity_crates > 0 { (t.loaded_crates * 100) / t.capacity_crates as i64 } else { 0 };
    v["fill_pct"] = json!(pct);
    v
}

pub async fn trip_json(conn: &mut PgConnection, id: Uuid, with_items: bool) -> AppResult<Value> {
    let t = sqlx::query_as::<_, TripRow>(&format!("{TRIP_SELECT} WHERE t.id = $1")).bind(id).fetch_optional(&mut *conn).await?
        .ok_or_else(|| AppError::not_found("Trip"))?;
    let mut v = trip_value(&t);
    if with_items { v["items"] = serde_json::to_value(trip_items(&mut *conn, id).await?).unwrap_or(Value::Null); }
    Ok(v)
}

/// Warning text when a vehicle is (nearly) full.
pub fn capacity_warning(loaded: i64, capacity: i32) -> Option<String> {
    if capacity > 0 && loaded * 100 >= capacity as i64 * 95 {
        Some(format!("Vehicle is {}% full ({loaded} of {capacity} crates)", loaded * 100 / capacity as i64))
    } else { None }
}

/// Next document number, or a placeholder in dry-run mode (a dry run must not burn numbers from the gap-free series).
pub async fn doc_no(conn: &mut PgConnection, dry: bool, seq: &'static str, prefix: &str) -> AppResult<String> {
    if dry { Ok(format!("{prefix}-(preview)")) } else { crate::util::next_no(conn, seq, prefix).await }
}

pub fn validation(fields: Vec<(String, String)>) -> AppError {
    AppError::Validation(fields.into_iter().collect())
}
