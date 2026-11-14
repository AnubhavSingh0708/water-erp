//! Stock corrections (OWNER B, page `warehouse`): adjustments, transfers, write-offs, physical counts.
//! Every flow: one transaction; the production_batches row of an FG_READY lot is locked `FOR UPDATE` BEFORE reservations are read
//! (serialises with loaders/dispatch); stock only through `crate::stock`; QC_QUARANTINE is never touched (QC owns it);
//! suspicious entries need `confirm`; `?dry_run=1` rolls back and returns effects + warnings.
use crate::{
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    routes::ref_data::bh::{self, created, finish_dry, local_date, Arg, DryQ, Id, ItemKind, J, Q, D},
    state::AppState,
    stock::{self, ItemType, Move},
    validate,
};
use axum::{extract::State, http::StatusCode, response::Response, Json};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

struct ItemCtx { kind: ItemKind, id: Uuid, name: String, returnable: bool }

async fn load_item(tx: &mut PgConnection, kind: ItemKind, id: Uuid) -> AppResult<ItemCtx> {
    match kind {
        ItemKind::Raw => {
            let n: Option<String> = sqlx::query_scalar("SELECT name FROM raw_materials WHERE id=$1").bind(id).fetch_optional(&mut *tx).await?;
            Ok(ItemCtx { kind, id, name: n.ok_or_else(|| AppError::field("item_id", "unknown raw material"))?, returnable: false })
        }
        ItemKind::Product => {
            let r = sqlx::query("SELECT name, kind FROM products WHERE id=$1").bind(id).fetch_optional(&mut *tx).await?
                .ok_or_else(|| AppError::field("item_id", "unknown product"))?;
            Ok(ItemCtx { kind, id, name: r.get("name"), returnable: r.get::<String, _>("kind") == "RETURNABLE" })
        }
    }
}

fn no_quarantine(zone: &str) -> AppResult<()> {
    if zone == "QC_QUARANTINE" { Err(AppError::bad("Stock in QC_QUARANTINE can only be released or rejected by Lab QC")) } else { Ok(()) }
}

fn zone_allowed(c: &ItemCtx, zone: &str) -> AppResult<()> {
    let ok = match c.kind {
        ItemKind::Raw => matches!(zone, "RM_STORE" | "BLOW_FLOOR" | "SCRAP_YARD"),
        ItemKind::Product => matches!(zone, "FG_READY" | "SCRAP_YARD") || (c.returnable && matches!(zone, "JAR_EMPTY" | "JAR_CLEAN")),
    };
    if ok { Ok(()) } else { Err(AppError::field("zone", "this item cannot be held in that zone")) }
}

fn whole_if_product(c: &ItemCtx, field: &str, d: Decimal) -> AppResult<()> {
    if c.kind == ItemKind::Product && d.fract() != Decimal::ZERO { return Err(AppError::field(field, "crates must be a whole number")); }
    Ok(())
}

/// Validates that `lot` really belongs to the item (id-abuse defence). Returns the lot's expiry date (FEFO key) when known.
async fn check_lot(tx: &mut PgConnection, c: &ItemCtx, zone: &str, lot: Uuid, positive: bool) -> AppResult<Option<NaiveDate>> {
    match c.kind {
        ItemKind::Raw => {
            if lot.is_nil() { return Ok(None); }
            // a lot is known when it is a GRN line of this material, or already held (in any zone) by this material (seeded/opening stock)
            let r: Option<Option<NaiveDate>> = sqlx::query_scalar("SELECT expiry_date FROM grn_lines WHERE id=$1 AND raw_material_id=$2").bind(lot).bind(c.id).fetch_optional(&mut *tx).await?;
            if let Some(e) = r { return Ok(e); }
            let r: Option<Option<NaiveDate>> = sqlx::query_scalar("SELECT expiry_date FROM stock_balances WHERE item_type='RAW' AND item_id=$1 AND lot_id=$2 LIMIT 1").bind(c.id).bind(lot).fetch_optional(&mut *tx).await?;
            Ok(r.ok_or_else(|| AppError::field("lot_id", "unknown lot for this material"))?)
        }
        ItemKind::Product => {
            if matches!(zone, "JAR_EMPTY" | "JAR_CLEAN") {
                if !lot.is_nil() { return Err(AppError::field("lot_id", "containers in this zone have no lot")); }
                return Ok(None);
            }
            if lot.is_nil() {
                if zone == "SCRAP_YARD" && c.returnable { return Ok(None); }
                return Err(AppError::field("lot_id", "a production batch lot is required for finished goods"));
            }
            let r = sqlx::query("SELECT qc_status, expiry_date FROM production_batches WHERE id=$1 AND product_id=$2").bind(lot).bind(c.id).fetch_optional(&mut *tx).await?
                .ok_or_else(|| AppError::field("lot_id", "unknown batch for this product"))?;
            if zone == "FG_READY" && positive && r.get::<String, _>("qc_status") != "APPROVED" {
                return Err(AppError::conflict("QC_LOCKED", "Only QC-approved batches can be added to finished goods"));
            }
            Ok(Some(r.get("expiry_date")))
        }
    }
}

/// For a PRODUCT lot in FG_READY: lock the batch row (same row loaders lock) and return the crates reserved by open loading trips.
async fn lock_reserved(tx: &mut PgConnection, loc: Uuid, c: &ItemCtx, zone: &str, lot: Uuid) -> AppResult<Decimal> {
    if c.kind == ItemKind::Product && zone == "FG_READY" && !lot.is_nil() {
        sqlx::query("SELECT id FROM production_batches WHERE id=$1 FOR UPDATE").bind(lot).execute(&mut *tx).await?;
        return stock::fg_reserved_at(tx, loc, lot).await;
    }
    Ok(Decimal::ZERO)
}

async fn balance_locked(tx: &mut PgConnection, loc: Uuid, c: &ItemCtx, zone: &str, lot: Uuid) -> AppResult<Decimal> {
    Ok(sqlx::query_scalar("SELECT COALESCE((SELECT qty FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone=$3 AND lot_id=$4 AND location_id=$5 FOR UPDATE), 0)")
        .bind(c.kind.it().as_str()).bind(c.id).bind(zone).bind(lot).bind(loc).fetch_one(&mut *tx).await?)
}

fn is_large(cur: Decimal, delta_abs: Decimal, pct: i64) -> bool {
    delta_abs >= Decimal::from(1000) || cur <= Decimal::ZERO || delta_abs * Decimal::from(100) > cur * Decimal::from(pct)
}

fn insufficient(c: &ItemCtx, zone: &str, lot: Uuid, have: Decimal, need: Decimal) -> AppError {
    AppError::conflict_with("INSUFFICIENT_STOCK", format!("Insufficient stock of {} in {zone}: have {}, need {}", c.name, have.normalize(), need.normalize()),
        json!({ "item_type": c.kind.it().as_str(), "item_id": c.id, "zone": zone, "lot_id": lot, "available": have, "required": need }))
}

/// Containers leaving the fleet through a warehouse flow need a DESTROYED event (API §0.2-E).
async fn fleet_destroyed(tx: &mut PgConnection, loc: Uuid, c: &ItemCtx, from_zone: &str, qty: Decimal, reason: &str, note: &str, user: &AuthUser) -> AppResult<()> {
    if !c.returnable || from_zone == "SCRAP_YARD" { return Ok(()); }
    let fleet_reason = match reason { "BREAKAGE" => "CRACKED", "EXPIRED" => "AGE_RETIRED", "CONTAMINATED" => "WASH_REJECT", "THEFT_LOSS" => "LOST", "TRANSIT_DAMAGE" => "DAMAGED_RETURN", _ => "OTHER" };
    let fleet_loc = if from_zone == "JAR_EMPTY" { "PLANT_EMPTY" } else { "PLANT_CLEAN" };
    let q = qty.trunc().to_string().parse::<i32>().map_err(|_| AppError::bad("quantity out of range"))?;
    sqlx::query("INSERT INTO jar_fleet_events (product_id, event_type, qty, reason_code, location, note, created_by, location_id) VALUES ($1,'DESTROYED',$2,$3,$4,$5,$6,$7)")
        .bind(c.id).bind(q).bind(fleet_reason).bind(fleet_loc).bind(note).bind(user.id).bind(loc).execute(&mut *tx).await?;
    Ok(())
}

// ------------------------------------------------------------------------------------------------------------------
// Adjustments
// ------------------------------------------------------------------------------------------------------------------
const ADJ_CODES: &[&str] = &["COUNT_CORRECTION", "ENTRY_ERROR", "FOUND", "DAMAGE", "OTHER"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdjReq {
    location_id: Option<Uuid>, item_type: ItemKind, item_id: Uuid, zone: String, lot_id: Option<Uuid>, delta: D, reason: String, reason_code: Option<String>,
    #[serde(default)] confirm: bool,
}

pub async fn adjust(State(s): State<AppState>, user: AuthUser, Q(dq): Q<DryQ>, J(b): J<AdjReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let dry = dq.on();
    let zone = bh::zone("zone", &b.zone)?;
    no_quarantine(zone)?;
    let delta = b.delta.0;
    validate::qty_nonzero("delta", delta)?;
    let reason = validate::required_text("reason", &b.reason, 300)?;
    if reason.chars().count() < 5 { return Err(AppError::field("reason", "must be at least 5 characters")); }
    let code = match b.reason_code.as_deref() {
        None => None,
        Some(c) if ADJ_CODES.contains(&c) => Some(c),
        _ => return Err(AppError::field("reason_code", "unknown reason code")),
    };
    let lot = b.lot_id.unwrap_or(stock::NIL_LOT);
    let pct = s.settings.get().large_adjust_pct;
    let mut tx = s.db.begin().await?;
    let loc = bh::write_location(&mut tx, &user, b.location_id).await?;
    let ctx = load_item(&mut tx, b.item_type, b.item_id).await?;
    zone_allowed(&ctx, zone)?;
    whole_if_product(&ctx, "delta", delta)?;
    let expiry = check_lot(&mut tx, &ctx, zone, lot, delta > Decimal::ZERO).await?;
    let reserved = lock_reserved(&mut tx, loc, &ctx, zone, lot).await?;
    let cur = balance_locked(&mut tx, loc, &ctx, zone, lot).await?;
    if cur + delta < Decimal::ZERO { return Err(insufficient(&ctx, zone, lot, cur, -delta)); }
    if delta < Decimal::ZERO && reserved > Decimal::ZERO && cur + delta < reserved { return Err(bh::reserved_error(&mut tx, loc, lot, reserved).await); }
    let mut warnings = vec![];
    if is_large(cur, delta.abs(), pct) {
        warnings.push(format!("{} {} is a large change for a balance of {} (limit {pct}% or 1000 units)", if delta > Decimal::ZERO { "+" } else { "" }, delta.normalize(), cur.normalize()));
    }
    bh::need_confirm(&warnings, b.confirm, dry, "This adjustment looks unusual")?;
    let id = Uuid::new_v4();
    let note = match code { Some(c) => format!("[{c}] {reason}"), None => reason.clone() };
    let after = stock::post(&mut tx, &Move::new(ctx.kind.it(), ctx.id, zone, lot, delta, "ADJUST").at(loc).reference("ADJUSTMENT", id).by(Some(user.id)).note(note).expiry(expiry), Uuid::new_v4()).await?;
    if dry { return finish_dry(tx, "ADJUSTMENT", id, &warnings).await; }
    audit::log(&mut *tx, Some(&user), "STOCK_ADJUST", "stock", Some(id.to_string()),
        json!({ "location_id": loc, "item_type": ctx.kind.it().as_str(), "item_id": ctx.id, "item": ctx.name, "zone": zone, "lot_id": lot, "delta": delta.to_string(), "reason": reason, "reason_code": code })).await;
    let effects = bh::effects_by_ref(&mut tx, "ADJUSTMENT", id).await?;
    tx.commit().await?;
    created(json!({ "id": id, "location_id": loc, "item_type": ctx.kind.it().as_str(), "item_id": ctx.id, "item": ctx.name, "zone": zone, "lot_id": lot, "delta": delta.to_string(),
        "balance_after": after.to_string(), "reason": reason, "effects": effects, "warnings": warnings }))
}

// ------------------------------------------------------------------------------------------------------------------
// Transfers
// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferReq {
    location_id: Option<Uuid>, item_type: ItemKind, item_id: Uuid, lot_id: Option<Uuid>, from_zone: String, to_zone: String, qty: D, note: Option<String>,
    #[serde(default)] confirm: bool,
}

pub async fn transfer(State(s): State<AppState>, user: AuthUser, Q(dq): Q<DryQ>, J(b): J<TransferReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let dry = dq.on();
    let from = bh::zone("from_zone", &b.from_zone)?;
    let to = bh::zone("to_zone", &b.to_zone)?;
    if from == "QC_QUARANTINE" || to == "QC_QUARANTINE" { return Err(AppError::bad("Stock in QC_QUARANTINE can only be released or rejected by Lab QC")); }
    if from == to { return Err(AppError::field("to_zone", "must differ from the source zone")); }
    if b.item_type == ItemKind::Product && to == "FG_READY" { return Err(AppError::bad("Finished goods can only enter FG_READY through QC approval")); }
    let qty = b.qty.0;
    validate::qty_positive("qty", qty)?;
    let note = bh::opt_text("note", &b.note, 500)?;
    let to_scrap = to == "SCRAP_YARD";
    let allowed = (to_scrap && from != "SCRAP_YARD")
        || (b.item_type == ItemKind::Raw && matches!((from, to), ("RM_STORE", "BLOW_FLOOR") | ("BLOW_FLOOR", "RM_STORE")));
    if !allowed { return Err(AppError::bad("This transfer is not allowed (allowed: raw materials between RM_STORE and BLOW_FLOOR, or anything to SCRAP_YARD)")); }
    if to_scrap && note.chars().count() < 5 { return Err(AppError::field("note", "a reason of at least 5 characters is required when moving stock to scrap")); }
    let lot = b.lot_id.unwrap_or(stock::NIL_LOT);
    let pct = s.settings.get().large_adjust_pct;
    let mut tx = s.db.begin().await?;
    let loc = bh::write_location(&mut tx, &user, b.location_id).await?;
    let ctx = load_item(&mut tx, b.item_type, b.item_id).await?;
    zone_allowed(&ctx, from)?;
    whole_if_product(&ctx, "qty", qty)?;
    check_lot(&mut tx, &ctx, from, lot, false).await?;
    let reserved = lock_reserved(&mut tx, loc, &ctx, from, lot).await?;
    let cur = balance_locked(&mut tx, loc, &ctx, from, lot).await?;
    if cur < qty { return Err(insufficient(&ctx, from, lot, cur, qty)); }
    if reserved > Decimal::ZERO && cur - qty < reserved { return Err(bh::reserved_error(&mut tx, loc, lot, reserved).await); }
    let mut warnings = vec![];
    if is_large(cur, qty, pct) { warnings.push(format!("Moving {} of {} is a large share of the balance {} (limit {pct}% or 1000 units)", qty.normalize(), ctx.name, cur.normalize())); }
    bh::need_confirm(&warnings, b.confirm, dry, "This transfer looks unusual")?;
    let id = Uuid::new_v4();
    stock::transfer_between(&mut tx, ctx.kind.it(), ctx.id, lot, loc, from, loc, to, qty, "TRANSFER", Some("TRANSFER"), Some(id), Some(user.id), &note).await?;
    if to_scrap { fleet_destroyed(&mut tx, loc, &ctx, from, qty, "OTHER", &note, &user).await?; }
    if dry { return finish_dry(tx, "TRANSFER", id, &warnings).await; }
    audit::log(&mut *tx, Some(&user), "STOCK_TRANSFER", "stock", Some(id.to_string()),
        json!({ "item_type": ctx.kind.it().as_str(), "item_id": ctx.id, "item": ctx.name, "lot_id": lot, "from": from, "to": to, "qty": qty.to_string(), "note": note })).await;
    let effects = bh::effects_by_ref(&mut tx, "TRANSFER", id).await?;
    tx.commit().await?;
    created(json!({ "id": id, "location_id": loc, "item": ctx.name, "from_zone": from, "to_zone": to, "qty": qty.to_string(), "effects": effects, "warnings": warnings }))
}

// ------------------------------------------------------------------------------------------------------------------
// Write-offs
// ------------------------------------------------------------------------------------------------------------------
const WO_REASONS: &[&str] = &["BREAKAGE", "EXPIRED", "CONTAMINATED", "SPILLAGE", "QC_REJECT", "THEFT_LOSS", "TRANSIT_DAMAGE", "OTHER"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WoReq {
    location_id: Option<Uuid>, item_type: ItemKind, item_id: Uuid, zone: String, lot_id: Option<Uuid>, qty: D, reason_code: String, note: String,
    #[serde(default)] confirm: bool, dispose: Option<bool>,
}

async fn wo_one(conn: &mut PgConnection, id: Uuid) -> AppResult<Value> {
    let v = bh::jval(&mut *conn,
        "SELECT jsonb_build_object('id', w.id, 'wo_no', w.wo_no, 'location_id', w.location_id, 'location_name', lo.name, 'item_type', w.item_type, 'item_id', w.item_id, 'item_name', COALESCE(m.name, p.name), 'item_code', COALESCE(m.code, p.code),
            'unit', CASE WHEN w.item_type='RAW' THEN m.stock_unit ELSE 'crates' END, 'zone', w.zone, 'lot_id', w.lot_id, 'lot_label', COALESCE(gl.lot_no, b.batch_no),
            'qty', w.qty::text, 'reason_code', w.reason_code, 'note', w.note, 'disposed', w.disposed, 'created_at', w.created_at, 'created_by_name', u.full_name)
         FROM stock_write_offs w JOIN locations lo ON lo.id = w.location_id LEFT JOIN raw_materials m ON w.item_type='RAW' AND m.id = w.item_id LEFT JOIN products p ON w.item_type='PRODUCT' AND p.id = w.item_id
         LEFT JOIN grn_lines gl ON w.item_type='RAW' AND gl.id = w.lot_id LEFT JOIN production_batches b ON w.item_type='PRODUCT' AND b.id = w.lot_id
         LEFT JOIN users u ON u.id = w.created_by WHERE w.id = $1", &[Arg::U(Some(id))]).await;
    match v { Ok(v) if !v.is_null() => Ok(v), Ok(_) | Err(AppError::NotFound(_)) => Err(AppError::not_found("write-off")), Err(e) => Err(e) }
}

pub async fn wo_create(State(s): State<AppState>, user: AuthUser, Q(dq): Q<DryQ>, J(b): J<WoReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let dry = dq.on();
    let zone = bh::zone("zone", &b.zone)?;
    no_quarantine(zone)?;
    let qty = b.qty.0;
    validate::qty_positive("qty", qty)?;
    if !WO_REASONS.contains(&b.reason_code.as_str()) { return Err(AppError::field("reason_code", "unknown reason code")); }
    let note = validate::required_text("note", &b.note, 500)?;
    if note.chars().count() < 5 { return Err(AppError::field("note", "must be at least 5 characters")); }
    let lot = b.lot_id.unwrap_or(stock::NIL_LOT);
    let from_scrap = zone == "SCRAP_YARD";
    let dispose = b.dispose.unwrap_or(false) || from_scrap;
    let mut tx = s.db.begin().await?;
    let loc = bh::write_location(&mut tx, &user, b.location_id).await?;
    let ctx = load_item(&mut tx, b.item_type, b.item_id).await?;
    zone_allowed(&ctx, zone)?;
    whole_if_product(&ctx, "qty", qty)?;
    check_lot(&mut tx, &ctx, zone, lot, false).await?;
    let reserved = lock_reserved(&mut tx, loc, &ctx, zone, lot).await?;
    let cur = balance_locked(&mut tx, loc, &ctx, zone, lot).await?;
    if cur < qty { return Err(insufficient(&ctx, zone, lot, cur, qty)); }
    if reserved > Decimal::ZERO && cur - qty < reserved { return Err(bh::reserved_error(&mut tx, loc, lot, reserved).await); }
    let warnings = vec![format!("{} {} of {} will be {} ({}). This cannot be undone.", qty.normalize(), if ctx.kind == ItemKind::Product { "crates" } else { "units" }, ctx.name,
        if dispose { "written off and removed from the books" } else { "written off to the scrap yard" }, b.reason_code)];
    bh::need_confirm(&warnings, b.confirm, dry, "Write-offs are irreversible - please confirm")?;
    let id = Uuid::new_v4();
    let wo_no = bh::doc_no(&mut tx, "wo_seq", "WO", dry).await?;
    sqlx::query("INSERT INTO stock_write_offs (id, wo_no, item_type, item_id, zone, lot_id, qty, reason_code, note, disposed, created_by, location_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
        .bind(id).bind(&wo_no).bind(ctx.kind.it().as_str()).bind(ctx.id).bind(zone).bind(lot).bind(qty).bind(&b.reason_code).bind(&note).bind(dispose).bind(user.id).bind(loc)
        .execute(&mut *tx).await?;
    let full_note = format!("{wo_no} {}: {note}", b.reason_code);
    if !from_scrap {
        stock::transfer_between(&mut tx, ctx.kind.it(), ctx.id, lot, loc, zone, loc, "SCRAP_YARD", qty, "WRITE_OFF", Some("WRITE_OFF"), Some(id), Some(user.id), &full_note).await?;
        fleet_destroyed(&mut tx, loc, &ctx, zone, qty, &b.reason_code, &full_note, &user).await?;
    }
    if dispose {
        stock::post_all(&mut tx, vec![Move::new(ctx.kind.it(), ctx.id, "SCRAP_YARD", lot, -qty, "DISPOSE").at(loc).reference("WRITE_OFF", id).by(Some(user.id)).note(full_note.clone())]).await?;
    }
    if dry { return finish_dry(tx, "WRITE_OFF", id, &warnings).await; }
    audit::log(&mut *tx, Some(&user), "WRITE_OFF", "stock_write_off", Some(id.to_string()),
        json!({ "wo_no": wo_no, "item": ctx.name, "item_type": ctx.kind.it().as_str(), "zone": zone, "lot_id": lot, "qty": qty.to_string(), "reason_code": b.reason_code, "dispose": dispose, "note": note })).await;
    let mut v = wo_one(&mut tx, id).await?;
    v["effects"] = bh::effects_by_ref(&mut tx, "WRITE_OFF", id).await?;
    tx.commit().await?;
    v["warnings"] = json!(warnings);
    created(v)
}

#[derive(Deserialize, Default)]
pub struct WoListQ { location_id: Option<String>, reason: Option<String>, from: Option<NaiveDate>, to: Option<NaiveDate>, format: Option<String>, limit: Option<i64>, offset: Option<i64> }

pub async fn wo_list(State(s): State<AppState>, user: AuthUser, Q(q): Q<WoListQ>) -> AppResult<Response> {
    let locs = { let mut c = s.db.acquire().await?; bh::list_scope(&mut c, &user, &q.location_id).await? };
    if let Some(r) = &q.reason { if !WO_REASONS.contains(&r.as_str()) { return Err(AppError::field("reason", "unknown reason code")); } }
    let csv = bh::is_csv(&q.format);
    let pq = validate::PageQuery { limit: q.limit, offset: q.offset };
    let (limit, offset) = if csv { (bh::CSV_MAX_ROWS, 0) } else { (pq.limit(), pq.offset()) };
    let off = s.settings.get().utc_offset_minutes;
    let v = bh::paged(&s.db,
        concat!("SELECT jsonb_build_object('id', w.id, 'wo_no', w.wo_no, 'location_id', w.location_id, 'location_name', lo.name, 'item_type', w.item_type, 'item_id', w.item_id, 'item_name', COALESCE(m.name, p.name), 'zone', w.zone,
                  'lot_id', w.lot_id, 'qty', w.qty::text, 'reason_code', w.reason_code, 'note', w.note, 'disposed', w.disposed, 'created_at', w.created_at, 'created_by_name', u.full_name) AS j,
                  w.created_at AS ca, w.id AS wid
           FROM stock_write_offs w JOIN locations lo ON lo.id = w.location_id LEFT JOIN raw_materials m ON w.item_type='RAW' AND m.id = w.item_id LEFT JOIN products p ON w.item_type='PRODUCT' AND p.id = w.item_id
           LEFT JOIN users u ON u.id = w.created_by
           WHERE w.location_id = ANY($5) AND ($1::text IS NULL OR w.reason_code = $1) AND ($2::date IS NULL OR ", local_date!("w.created_at", "$4"), " >= $2) AND ($3::date IS NULL OR ", local_date!("w.created_at", "$4"), " <= $3)"),
        "ca DESC, wid", &[Arg::t(&q.reason), Arg::D(q.from), Arg::D(q.to), Arg::I(off), Arg::UA(locs)], limit, offset).await?;
    if csv {
        return Ok(bh::csv_response("write-offs.csv", &[("Write-off", "wo_no"), ("Date", "created_at"), ("Item", "item_name"), ("Zone", "zone"), ("Quantity", "qty"),
            ("Reason", "reason_code"), ("Note", "note"), ("Disposed", "disposed"), ("By", "created_by_name")], bh::items_of(&v)));
    }
    Ok(bh::json_resp(v))
}

fn loc_of(v: &Value) -> Uuid { v["location_id"].as_str().and_then(|x| Uuid::parse_str(x).ok()).unwrap_or_default() }

pub async fn wo_get(State(s): State<AppState>, user: AuthUser, Id(id): Id) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let v = wo_one(&mut c, id).await?;
    user.require_location(loc_of(&v))?;
    Ok(Json(v))
}

// ------------------------------------------------------------------------------------------------------------------
// Physical stock counts
// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountReq { location_id: Option<Uuid>, zone: String, note: Option<String> }

async fn count_one(conn: &mut PgConnection, id: Uuid) -> AppResult<Value> {
    let v = bh::jval(&mut *conn,
        "SELECT jsonb_build_object('id', c.id, 'cnt_no', c.cnt_no, 'location_id', c.location_id, 'location_name', lo.name, 'zone', c.zone, 'status', c.status, 'note', c.note, 'created_at', c.created_at, 'created_by_name', u.full_name,
            'posted_at', c.posted_at, 'posted_by_name', pu.full_name,
            'total_lines', (SELECT count(*) FROM stock_count_lines WHERE count_id = c.id), 'counted_lines', (SELECT count(*) FROM stock_count_lines WHERE count_id = c.id AND counted_qty IS NOT NULL),
            'lines', COALESCE((SELECT jsonb_agg(jsonb_build_object('line_id', l.id, 'item_type', l.item_type, 'item_id', l.item_id, 'name', COALESCE(m.name, p.name), 'code', COALESCE(m.code, p.code),
                  'unit', CASE WHEN l.item_type='RAW' THEN m.stock_unit ELSE 'crates' END, 'lot_id', l.lot_id, 'lot_label', COALESCE(gl.lot_no, b.batch_no),
                  'expected_qty', l.expected_qty::text, 'counted_qty', l.counted_qty::text, 'variance', (l.counted_qty - l.expected_qty)::text,
                  'variance_pct', CASE WHEN l.counted_qty IS NOT NULL AND l.expected_qty <> 0 THEN round((l.counted_qty - l.expected_qty) * 100 / l.expected_qty, 2)::text END,
                  'note', l.note) ORDER BY COALESCE(m.name, p.name), l.lot_id)
               FROM stock_count_lines l LEFT JOIN raw_materials m ON l.item_type='RAW' AND m.id = l.item_id LEFT JOIN products p ON l.item_type='PRODUCT' AND p.id = l.item_id
               LEFT JOIN grn_lines gl ON l.item_type='RAW' AND gl.id = l.lot_id LEFT JOIN production_batches b ON l.item_type='PRODUCT' AND b.id = l.lot_id
               WHERE l.count_id = c.id), '[]'::jsonb))
         FROM stock_counts c JOIN locations lo ON lo.id = c.location_id LEFT JOIN users u ON u.id = c.created_by LEFT JOIN users pu ON pu.id = c.posted_by WHERE c.id = $1", &[Arg::U(Some(id))]).await;
    match v { Ok(v) if !v.is_null() => Ok(v), Ok(_) | Err(AppError::NotFound(_)) => Err(AppError::not_found("count")), Err(e) => Err(e) }
}

pub async fn count_create(State(s): State<AppState>, user: AuthUser, J(b): J<CountReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let zone = bh::zone("zone", &b.zone)?;
    no_quarantine(zone)?;
    let note = bh::opt_text("note", &b.note, 500)?;
    let mut tx = s.db.begin().await?;
    let loc = bh::write_location(&mut tx, &user, b.location_id).await?;
    let id = Uuid::new_v4();
    let no = bh::doc_no(&mut tx, "cnt_seq", "CNT", false).await?;
    sqlx::query("INSERT INTO stock_counts (id, cnt_no, zone, note, created_by, location_id) VALUES ($1,$2,$3,$4,$5,$6)").bind(id).bind(&no).bind(zone).bind(&note).bind(user.id).bind(loc).execute(&mut *tx).await?;
    let n = sqlx::query("INSERT INTO stock_count_lines (count_id, item_type, item_id, lot_id, expected_qty) SELECT $1, item_type, item_id, lot_id, qty FROM stock_balances WHERE zone=$2 AND location_id=$3 AND qty > 0")
        .bind(id).bind(zone).bind(loc).execute(&mut *tx).await?.rows_affected();
    if n == 0 { return Err(AppError::bad("There is no stock in this zone to count")); }
    audit::log(&mut *tx, Some(&user), "COUNT_CREATE", "stock_count", Some(id.to_string()), json!({ "cnt_no": no, "zone": zone, "location_id": loc, "lines": n })).await;
    let v = count_one(&mut tx, id).await?;
    tx.commit().await?;
    created(v)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountLine { line_id: Uuid, counted_qty: D, note: Option<String> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountLinesReq { lines: Vec<CountLine> }

pub async fn count_lines(State(s): State<AppState>, user: AuthUser, Id(id): Id, J(b): J<CountLinesReq>) -> AppResult<Json<Value>> {
    validate::max_items("lines", b.lines.len(), 1000)?;
    let mut notes = vec![];
    for l in &b.lines {
        validate::qty_non_negative("counted_qty", l.counted_qty.0)?;
        notes.push(bh::opt_text("note", &l.note, 300)?);
    }
    let mut tx = s.db.begin().await?;
    let hd = sqlx::query("SELECT status, location_id FROM stock_counts WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.ok_or_else(|| AppError::not_found("count"))?;
    user.require_location(hd.get::<Uuid, _>("location_id"))?;
    if hd.get::<String, _>("status") != "DRAFT" { return Err(AppError::conflict("INVALID_STATE", "This count is no longer a draft")); }
    for (i, l) in b.lines.iter().enumerate() {
        let item_type: Option<String> = sqlx::query_scalar("SELECT item_type FROM stock_count_lines WHERE id=$1 AND count_id=$2").bind(l.line_id).bind(id).fetch_optional(&mut *tx).await?;
        let item_type = item_type.ok_or_else(|| AppError::field("line_id", "line does not belong to this count"))?;
        if item_type == "PRODUCT" && l.counted_qty.0.fract() != Decimal::ZERO { return Err(AppError::field("counted_qty", "crates must be a whole number")); }
        sqlx::query("UPDATE stock_count_lines SET counted_qty=$3, note=$4 WHERE id=$1 AND count_id=$2").bind(l.line_id).bind(id).bind(l.counted_qty.0).bind(&notes[i]).execute(&mut *tx).await?;
    }
    let _ = &user;
    let v = count_one(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(v))
}

pub async fn count_get(State(s): State<AppState>, user: AuthUser, Id(id): Id) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let v = count_one(&mut c, id).await?;
    user.require_location(loc_of(&v))?;
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
pub struct CountListQ { location_id: Option<String>, status: Option<String>, limit: Option<i64>, offset: Option<i64> }

pub async fn count_list(State(s): State<AppState>, user: AuthUser, Q(q): Q<CountListQ>) -> AppResult<Json<Value>> {
    let locs = { let mut c = s.db.acquire().await?; bh::list_scope(&mut c, &user, &q.location_id).await? };
    if let Some(st) = &q.status { if !["DRAFT", "POSTED", "CANCELLED"].contains(&st.as_str()) { return Err(AppError::field("status", "unknown status")); } }
    let pq = validate::PageQuery { limit: q.limit, offset: q.offset };
    Ok(Json(bh::paged(&s.db,
        "SELECT jsonb_build_object('id', c.id, 'cnt_no', c.cnt_no, 'location_id', c.location_id, 'zone', c.zone, 'status', c.status, 'note', c.note, 'created_at', c.created_at, 'posted_at', c.posted_at,
                'total_lines', (SELECT count(*) FROM stock_count_lines WHERE count_id = c.id), 'counted_lines', (SELECT count(*) FROM stock_count_lines WHERE count_id = c.id AND counted_qty IS NOT NULL)) AS j,
                c.created_at AS ca, c.id AS cid
         FROM stock_counts c WHERE c.location_id = ANY($2) AND ($1::text IS NULL OR c.status = $1)", "ca DESC, cid", &[Arg::t(&q.status), Arg::UA(locs)], pq.limit(), pq.offset()).await?))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PostReq { #[serde(default)] confirm: bool }

pub async fn count_cancel(State(s): State<AppState>, user: AuthUser, Id(id): Id) -> AppResult<Json<Value>> {
    let mut tx = s.db.begin().await?;
    let hd = sqlx::query("SELECT status, location_id FROM stock_counts WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.ok_or_else(|| AppError::not_found("count"))?;
    user.require_location(hd.get::<Uuid, _>("location_id"))?;
    if hd.get::<String, _>("status") != "DRAFT" { return Err(AppError::conflict("INVALID_STATE", "Only a draft count can be cancelled")); }
    sqlx::query("UPDATE stock_counts SET status='CANCELLED' WHERE id=$1").bind(id).execute(&mut *tx).await?;
    audit::log(&mut *tx, Some(&user), "COUNT_CANCEL", "stock_count", Some(id.to_string()), json!({})).await;
    let v = count_one(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(v))
}

pub async fn count_post(State(s): State<AppState>, user: AuthUser, Id(id): Id, Q(dq): Q<DryQ>, body: axum::body::Bytes) -> AppResult<(StatusCode, Json<Value>)> {
    let req: PostReq = if body.iter().all(|c| c.is_ascii_whitespace()) { PostReq::default() }
        else { serde_json::from_slice(&body).map_err(|_| AppError::bad("Invalid request body"))? };
    let dry = dq.on();
    let pct = s.settings.get().large_adjust_pct;
    let mut tx = s.db.begin().await?;
    let h = sqlx::query("SELECT status, zone, cnt_no, location_id FROM stock_counts WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.ok_or_else(|| AppError::not_found("count"))?;
    let loc: Uuid = h.get("location_id");
    user.require_location(loc)?;
    if h.get::<String, _>("status") != "DRAFT" { return Err(AppError::conflict("INVALID_STATE", "This count was already posted or cancelled")); }
    let zone = bh::zone("zone", &h.get::<String, _>("zone"))?;
    let cnt_no: String = h.get("cnt_no");
    no_quarantine(zone)?;
    let lines = sqlx::query("SELECT l.id, l.item_type, l.item_id, l.lot_id, l.expected_qty, l.counted_qty, l.note, COALESCE(m.name, p.name) AS name
                             FROM stock_count_lines l LEFT JOIN raw_materials m ON l.item_type='RAW' AND m.id = l.item_id LEFT JOIN products p ON l.item_type='PRODUCT' AND p.id = l.item_id
                             WHERE l.count_id=$1 ORDER BY l.item_type, l.item_id, l.lot_id").bind(id).fetch_all(&mut *tx).await?;
    // serialise with loaders: lock all affected batch rows first (stable order)
    if zone == "FG_READY" {
        let lots: Vec<Uuid> = lines.iter().filter(|l| l.get::<String, _>("item_type") == "PRODUCT").map(|l| l.get::<Uuid, _>("lot_id")).collect();
        sqlx::query("SELECT id FROM production_batches WHERE id = ANY($1) ORDER BY expiry_date ASC, created_at ASC, id ASC FOR UPDATE").bind(&lots).execute(&mut *tx).await?;
    }
    let mut moves = vec![];
    let mut warnings = vec![];
    let mut uncounted = 0;
    let mut changed = 0;
    for l in &lines {
        let Some(counted) = l.get::<Option<Decimal>, _>("counted_qty") else { uncounted += 1; continue; };
        let kind = if l.get::<String, _>("item_type") == "RAW" { ItemKind::Raw } else { ItemKind::Product };
        let item: Uuid = l.get("item_id");
        let lot: Uuid = l.get("lot_id");
        let expected: Decimal = l.get("expected_qty");
        let name: String = l.get("name");
        let ctx = ItemCtx { kind, id: item, name: name.clone(), returnable: false };
        let reserved = lock_reserved(&mut tx, loc, &ctx, zone, lot).await?;
        let cur = balance_locked(&mut tx, loc, &ctx, zone, lot).await?;
        let var = counted - expected;
        if !var.is_zero() && (expected <= Decimal::ZERO || var.abs() * Decimal::from(100) > expected * Decimal::from(pct)) && l.get::<String, _>("note").trim().is_empty() {
            return Err(AppError::field("lines", &format!("{name}: a note is required for a large variance ({} expected, {} counted)", expected.normalize(), counted.normalize())));
        }
        let delta = counted - cur;
        if delta.is_zero() { continue; }
        changed += 1;
        if kind == ItemKind::Product && zone == "FG_READY" && delta > Decimal::ZERO {
            let st: Option<String> = sqlx::query_scalar("SELECT qc_status FROM production_batches WHERE id=$1 AND product_id=$2").bind(lot).bind(item).fetch_optional(&mut *tx).await?;
            if st.as_deref() != Some("APPROVED") { return Err(AppError::conflict("QC_LOCKED", format!("{name}: only QC-approved batches can be added to finished goods"))); }
        }
        if reserved > Decimal::ZERO && counted < reserved { return Err(bh::reserved_error(&mut tx, loc, lot, reserved).await); }
        if warnings.len() < 12 { warnings.push(format!("{name}: {} -> {} ({}{})", cur.normalize(), counted.normalize(), if delta > Decimal::ZERO { "+" } else { "" }, delta.normalize())); }
        let kind_it: ItemType = kind.it();
        moves.push(Move::new(kind_it, item, zone, lot, delta, "ADJUST").at(loc).reference("COUNT", id).by(Some(user.id)).note(format!("stock count {cnt_no}")));
    }
    let mut head = vec![format!("Posting {cnt_no} changes {changed} stock line(s) in {zone}; this cannot be undone")];
    if uncounted > 0 { head.push(format!("{uncounted} line(s) were not counted and stay unchanged")); }
    head.extend(warnings);
    let warnings = head;
    bh::need_confirm(&warnings, req.confirm, dry, "Posting a stock count is irreversible - please confirm")?;
    stock::post_all(&mut tx, moves).await?;
    if dry { return finish_dry(tx, "COUNT", id, &warnings).await; }
    sqlx::query("UPDATE stock_counts SET status='POSTED', posted_by=$2, posted_at=now() WHERE id=$1").bind(id).bind(user.id).execute(&mut *tx).await?;
    audit::log(&mut *tx, Some(&user), "COUNT_POST", "stock_count", Some(id.to_string()), json!({ "cnt_no": cnt_no, "zone": zone, "adjusted_lines": changed, "uncounted": uncounted })).await;
    let mut v = count_one(&mut tx, id).await?;
    v["effects"] = bh::effects_by_ref(&mut tx, "COUNT", id).await?;
    v["warnings"] = json!(warnings);
    tx.commit().await?;
    Ok((StatusCode::OK, Json(v)))
}
