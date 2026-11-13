//! `/api/godowns/*` – inter-godown transfers (OWNER B, page `warehouse`; API_REFERENCE 0.2-F).
//! Goods never move directly between godowns: sending books them into the system TRANSIT location, receiving books them into the
//! destination (received / damaged -> SCRAP_YARD / shortage written off from TRANSIT with confirm + note). Nothing disappears silently.
//! Locks: transfer row -> production_batches rows (FG lots, sorted) -> stock rows (sorted by `stock::post_all`).
use crate::{
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    routes::ref_data::bh::{self, created, finish_dry, local_date, ok, Arg, DryQ, Id, ItemKind, J, Q, D},
    state::AppState,
    stock::{self, ItemType, Move, TRANSIT_LOCATION},
    validate,
};
use axum::{extract::State, http::StatusCode, routing::{get, post}, Json, Router};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/transfers", get(list).post(create))
        .route("/transfers/{id}", get(detail))
        .route("/transfers/{id}/receive", post(receive))
        .route("/transfers/{id}/cancel", post(cancel))
}

const ZONES: &[&str] = &["RM_STORE", "FG_READY", "JAR_EMPTY", "JAR_CLEAN", "BLOW_FLOOR"];

fn zone_static(z: &str) -> AppResult<&'static str> {
    ZONES.iter().copied().find(|x| *x == z).ok_or_else(|| AppError::field("zone", "only RM_STORE, FG_READY, JAR_EMPTY, JAR_CLEAN and BLOW_FLOOR can be transferred (never QC_QUARANTINE or SCRAP_YARD)"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineReq { item_type: ItemKind, item_id: Uuid, lot_id: Option<Uuid>, zone: String, qty: D }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateReq {
    from_location_id: Uuid, to_location_id: Uuid, lines: Vec<LineReq>,
    vehicle_id: Option<Uuid>, vehicle_no: Option<String>, driver_name: Option<String>, note: Option<String>,
    #[serde(default)] confirm: bool,
}

pub(crate) async fn transfer_json(conn: &mut PgConnection, id: Uuid) -> AppResult<Value> {
    let v = bh::jval(&mut *conn,
        "SELECT jsonb_build_object('id', t.id, 'tf_no', t.tf_no, 'status', t.status, 'from_location_id', t.from_location_id, 'from_name', lf.name,
            'to_location_id', t.to_location_id, 'to_name', lt.name, 'vehicle_id', t.vehicle_id, 'vehicle_no', t.vehicle_no, 'driver_name', t.driver_name, 'note', t.note,
            'created_at', t.created_at, 'created_by_name', cu.full_name, 'received_at', t.received_at, 'received_by_name', ru.full_name, 'cancel_reason', t.cancel_reason,
            'lines', COALESCE((SELECT jsonb_agg(jsonb_build_object('line_id', l.id, 'item_type', l.item_type, 'item_id', l.item_id, 'name', COALESCE(m.name, p.name), 'code', COALESCE(m.code, p.code),
                    'unit', CASE WHEN l.item_type = 'RAW' THEN m.stock_unit ELSE 'crates' END, 'lot_id', l.lot_id, 'lot_label', COALESCE(gl.lot_no, b.batch_no), 'zone', l.zone,
                    'qty_sent', l.qty_sent::text, 'qty_received', l.qty_received::text, 'qty_damaged', l.qty_damaged::text,
                    'qty_short', CASE WHEN l.qty_received IS NOT NULL THEN (l.qty_sent - l.qty_received - COALESCE(l.qty_damaged, 0))::text END, 'note', l.note)
                    ORDER BY COALESCE(m.name, p.name), l.zone, l.id)
                 FROM godown_transfer_lines l LEFT JOIN raw_materials m ON l.item_type='RAW' AND m.id = l.item_id LEFT JOIN products p ON l.item_type='PRODUCT' AND p.id = l.item_id
                 LEFT JOIN grn_lines gl ON l.item_type='RAW' AND gl.id = l.lot_id LEFT JOIN production_batches b ON l.item_type='PRODUCT' AND b.id = l.lot_id
                 WHERE l.transfer_id = t.id), '[]'::jsonb))
         FROM godown_transfers t JOIN locations lf ON lf.id = t.from_location_id JOIN locations lt ON lt.id = t.to_location_id
         LEFT JOIN users cu ON cu.id = t.created_by LEFT JOIN users ru ON ru.id = t.received_by WHERE t.id = $1", &[Arg::U(Some(id))]).await;
    match v { Ok(v) if !v.is_null() => Ok(v), Ok(_) | Err(AppError::NotFound(_)) => Err(AppError::not_found("transfer")), Err(e) => Err(e) }
}

fn loc(v: &Value, k: &str) -> Uuid { v[k].as_str().and_then(|x| Uuid::parse_str(x).ok()).unwrap_or_default() }

async fn selectable(conn: &mut PgConnection, id: Uuid, field: &str) -> AppResult<()> {
    let r: Option<(String, bool)> = sqlx::query_as("SELECT kind, is_active FROM locations WHERE id=$1").bind(id).fetch_optional(&mut *conn).await?;
    match r { Some((k, true)) if k != "TRANSIT" => Ok(()), Some(_) => Err(AppError::field(field, "this godown is inactive or not selectable")), None => Err(AppError::field(field, "unknown godown")) }
}

pub async fn create(State(s): State<AppState>, user: AuthUser, Q(dq): Q<DryQ>, J(b): J<CreateReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let dry = dq.on();
    validate::max_items("lines", b.lines.len(), validate::MAX_LINES)?;
    if b.from_location_id == b.to_location_id { return Err(AppError::field("to_location_id", "must differ from the source godown")); }
    let vehicle_no = bh::opt_text("vehicle_no", &b.vehicle_no, 30)?;
    let driver = bh::opt_text("driver_name", &b.driver_name, 80)?;
    let note = bh::opt_text("note", &b.note, 500)?;
    let mut zones = vec![];
    for l in &b.lines {
        validate::qty_positive("qty", l.qty.0)?;
        zones.push(zone_static(&l.zone)?);
    }
    let mut seen = HashSet::new();
    for (i, l) in b.lines.iter().enumerate() {
        if !seen.insert((l.item_type.it().as_str(), l.item_id, l.lot_id.unwrap_or(stock::NIL_LOT), zones[i])) { return Err(AppError::field("lines", "the same item, lot and zone appears twice")); }
    }
    let mut tx = s.db.begin().await?;
    let from = crate::util::resolve_location(&mut tx, &user, Some(b.from_location_id)).await?;
    selectable(&mut tx, b.to_location_id, "to_location_id").await?;
    let to = b.to_location_id;
    if let Some(v) = b.vehicle_id {
        let a: Option<bool> = sqlx::query_scalar("SELECT is_active FROM vehicles WHERE id=$1").bind(v).fetch_optional(&mut *tx).await?;
        if a != Some(true) { return Err(AppError::field("vehicle_id", "unknown or inactive vehicle")); }
    }
    let today = crate::util::plant_today(&mut tx).await?;

    // validate every line (ids are hostile)
    struct L { it: ItemType, item: Uuid, lot: Uuid, zone: &'static str, qty: Decimal, name: String, batch_product: Option<Uuid>, expiry: Option<NaiveDate> }
    let mut lines: Vec<L> = vec![];
    for (i, l) in b.lines.iter().enumerate() {
        let zone = zones[i];
        let lot = l.lot_id.unwrap_or(stock::NIL_LOT);
        let qty = l.qty.0;
        let (name, batch_product, expiry) = match l.item_type {
            ItemKind::Raw => {
                if !matches!(zone, "RM_STORE" | "BLOW_FLOOR") { return Err(AppError::field("zone", "raw materials can only be transferred from RM_STORE or BLOW_FLOOR")); }
                let n: Option<String> = sqlx::query_scalar("SELECT name FROM raw_materials WHERE id=$1").bind(l.item_id).fetch_optional(&mut *tx).await?;
                let n = n.ok_or_else(|| AppError::field("item_id", "unknown raw material"))?;
                let exp: Option<Option<NaiveDate>> = sqlx::query_scalar("SELECT expiry_date FROM stock_balances WHERE item_type='RAW' AND item_id=$1 AND zone=$2 AND lot_id=$3 AND location_id=$4")
                    .bind(l.item_id).bind(zone).bind(lot).bind(from).fetch_optional(&mut *tx).await?;
                let exp = exp.ok_or_else(|| AppError::conflict("INSUFFICIENT_STOCK", format!("{n}: this lot is not held in the source godown")))?;
                (n, None, exp)
            }
            ItemKind::Product => {
                if qty.fract() != Decimal::ZERO { return Err(AppError::field("qty", "crates must be a whole number")); }
                let r = sqlx::query("SELECT name, kind FROM products WHERE id=$1").bind(l.item_id).fetch_optional(&mut *tx).await?.ok_or_else(|| AppError::field("item_id", "unknown product"))?;
                let n: String = r.get("name");
                match zone {
                    "FG_READY" => {
                        let br = sqlx::query("SELECT qc_status, expiry_date FROM production_batches WHERE id=$1 AND product_id=$2").bind(lot).bind(l.item_id).fetch_optional(&mut *tx).await?
                            .ok_or_else(|| AppError::field("lot_id", "a production batch of this product is required"))?;
                        if br.get::<String, _>("qc_status") != "APPROVED" { return Err(AppError::conflict("QC_LOCKED", format!("{n}: only QC-approved batches can be transferred"))); }
                        (n, Some(l.item_id), Some(br.get::<NaiveDate, _>("expiry_date")))
                    }
                    "JAR_EMPTY" | "JAR_CLEAN" => {
                        if r.get::<String, _>("kind") != "RETURNABLE" || !lot.is_nil() { return Err(AppError::field("zone", "only returnable containers (no lot) can be transferred from the jar zones")); }
                        (n, None, None)
                    }
                    _ => return Err(AppError::field("zone", "products can only be transferred from FG_READY or the jar zones")),
                }
            }
        };
        lines.push(L { it: l.item_type.it(), item: l.item_id, lot, zone, qty, name, batch_product, expiry });
    }
    // reservations: lock the batch rows FIRST (same rows the loaders lock), then compare with free crates
    let mut batch_ids: Vec<Uuid> = lines.iter().filter(|l| l.batch_product.is_some()).map(|l| l.lot).collect();
    batch_ids.sort(); batch_ids.dedup();
    if !batch_ids.is_empty() {
        sqlx::query("SELECT id FROM production_batches WHERE id = ANY($1) ORDER BY id FOR UPDATE").bind(&batch_ids).execute(&mut *tx).await?;
    }
    let mut warnings = vec![];
    for l in &lines {
        if let Some(p) = l.batch_product {
            let free = stock::fg_free_at(&mut tx, from, p, l.lot).await?;
            if l.qty > free {
                return Err(AppError::conflict_with("INSUFFICIENT_STOCK",
                    format!("{}: only {} crates are free to transfer (the rest is reserved by open loading trips)", l.name, free.max(Decimal::ZERO).normalize()),
                    json!({ "item_type": "PRODUCT", "item_id": l.item, "lot_id": l.lot, "location_id": from, "available": free.max(Decimal::ZERO), "required": l.qty })));
            }
        }
        if let Some(e) = l.expiry {
            if e < today { warnings.push(format!("{}: this lot is already expired ({e}) - consider a write-off instead", l.name)); }
            else if (e - today).num_days() <= 7 { warnings.push(format!("{}: this lot expires within 7 days ({e})", l.name)); }
        }
    }
    bh::need_confirm(&warnings, b.confirm, dry, "This transfer needs a second look")?;

    let id = Uuid::new_v4();
    let tf_no = bh::doc_no(&mut tx, "tf_seq", "TF", dry).await?;
    sqlx::query("INSERT INTO godown_transfers (id, tf_no, from_location_id, to_location_id, vehicle_id, vehicle_no, driver_name, note, created_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(id).bind(&tf_no).bind(from).bind(to).bind(b.vehicle_id).bind(&vehicle_no).bind(&driver).bind(&note).bind(user.id).execute(&mut *tx).await?;
    let mut sorted: Vec<&L> = lines.iter().collect();
    sorted.sort_by(|a, c| (a.it, a.item, a.zone, a.lot).cmp(&(c.it, c.item, c.zone, c.lot)));
    for l in sorted {
        sqlx::query("INSERT INTO godown_transfer_lines (transfer_id, item_type, item_id, lot_id, zone, qty_sent) VALUES ($1,$2,$3,$4,$5,$6)")
            .bind(id).bind(l.it.as_str()).bind(l.item).bind(l.lot).bind(l.zone).bind(l.qty).execute(&mut *tx).await?;
        stock::transfer_between(&mut tx, l.it, l.item, l.lot, from, l.zone, TRANSIT_LOCATION, l.zone, l.qty, "GODOWN_TRANSFER_OUT", Some("GODOWN_TRANSFER"), Some(id), Some(user.id), &tf_no).await?;
    }
    if dry { return finish_dry(tx, "GODOWN_TRANSFER", id, &warnings).await; }
    audit::log(&mut *tx, Some(&user), "GODOWN_TRANSFER_SEND", "godown_transfer", Some(id.to_string()),
        json!({ "tf_no": tf_no, "from": from, "to": to, "lines": lines.len() })).await;
    let mut v = transfer_json(&mut tx, id).await?;
    tx.commit().await?;
    v["warnings"] = json!(warnings);
    created(v)
}

// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecvLine { line_id: Uuid, qty_received: D, qty_damaged: Option<D>, note: Option<String> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecvReq { lines: Vec<RecvLine>, #[serde(default)] confirm: bool }

pub async fn receive(State(s): State<AppState>, user: AuthUser, Id(id): Id, Q(dq): Q<DryQ>, J(b): J<RecvReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let dry = dq.on();
    validate::max_items("lines", b.lines.len(), validate::MAX_LINES)?;
    let mut notes = vec![];
    for l in &b.lines {
        validate::qty_non_negative("qty_received", l.qty_received.0)?;
        if let Some(d) = l.qty_damaged { validate::qty_non_negative("qty_damaged", d.0)?; }
        notes.push(bh::opt_text("note", &l.note, 300)?);
    }
    let mut tx = s.db.begin().await?;
    let t = sqlx::query("SELECT status, to_location_id, tf_no FROM godown_transfers WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?
        .ok_or_else(|| AppError::not_found("transfer"))?;
    let to: Uuid = t.get("to_location_id");
    let tf_no: String = t.get("tf_no");
    user.require_location(to)?;
    if t.get::<String, _>("status") != "IN_TRANSIT" { return Err(AppError::conflict("INVALID_STATE", "This transfer was already received or cancelled")); }
    let rows = sqlx::query("SELECT l.id, l.item_type, l.item_id, l.lot_id, l.zone, l.qty_sent, COALESCE(m.name, p.name) AS name
                            FROM godown_transfer_lines l LEFT JOIN raw_materials m ON l.item_type='RAW' AND m.id = l.item_id LEFT JOIN products p ON l.item_type='PRODUCT' AND p.id = l.item_id
                            WHERE l.transfer_id=$1 ORDER BY l.item_type, l.item_id, l.zone, l.lot_id").bind(id).fetch_all(&mut *tx).await?;
    let want: HashSet<Uuid> = rows.iter().map(|r| r.get::<Uuid, _>("id")).collect();
    let got: HashSet<Uuid> = b.lines.iter().map(|l| l.line_id).collect();
    if got.len() != b.lines.len() || got != want { return Err(AppError::field("lines", "every transfer line must be listed exactly once")); }
    let by_line: BTreeMap<Uuid, usize> = b.lines.iter().enumerate().map(|(i, l)| (l.line_id, i)).collect();
    let mut warnings = vec![];
    let mut plan = vec![];
    for r in &rows {
        let lid: Uuid = r.get("id");
        let i = by_line[&lid];
        let sent: Decimal = r.get("qty_sent");
        let rec = b.lines[i].qty_received.0;
        let dmg = b.lines[i].qty_damaged.map(|d| d.0).unwrap_or(Decimal::ZERO);
        let name: String = r.get("name");
        if r.get::<String, _>("item_type") == "PRODUCT" && (rec.fract() != Decimal::ZERO || dmg.fract() != Decimal::ZERO) {
            return Err(AppError::field("qty_received", "crates must be whole numbers"));
        }
        if rec + dmg > sent { return Err(AppError::field("lines", &format!("{name}: received + damaged ({}) is more than the {} sent", (rec + dmg).normalize(), sent.normalize()))); }
        let short = sent - rec - dmg;
        if short > Decimal::ZERO {
            if notes[i].is_empty() { return Err(AppError::field("lines", &format!("{name}: a note is required when {} is short", short.normalize()))); }
            warnings.push(format!("{name}: {} of {} sent are missing and will be written off as shortage", short.normalize(), sent.normalize()));
        }
        if dmg > Decimal::ZERO { warnings.push(format!("{name}: {} damaged in transit will go to the scrap yard", dmg.normalize())); }
        plan.push((lid, r.get::<String, _>("item_type"), r.get::<Uuid, _>("item_id"), r.get::<Uuid, _>("lot_id"), r.get::<String, _>("zone"), rec, dmg, short, i));
    }
    // only shortages need the explicit confirmation; damage is just reported
    let short_w: Vec<String> = warnings.iter().filter(|w| w.contains("missing")).cloned().collect();
    bh::need_confirm(&short_w, b.confirm, dry, "Shortage in transit - please confirm")?;

    for (lid, it, item, lot, zone, rec, dmg, short, i) in plan {
        let it = if it == "RAW" { ItemType::Raw } else { ItemType::Product };
        let zone = zone_static(&zone)?;
        if rec > Decimal::ZERO {
            stock::transfer_between(&mut tx, it, item, lot, TRANSIT_LOCATION, zone, to, zone, rec, "GODOWN_TRANSFER_IN", Some("GODOWN_TRANSFER"), Some(id), Some(user.id), &tf_no).await?;
        }
        if dmg > Decimal::ZERO {
            stock::transfer_between(&mut tx, it, item, lot, TRANSIT_LOCATION, zone, to, "SCRAP_YARD", dmg, "TRANSIT_DAMAGE", Some("GODOWN_TRANSFER"), Some(id), Some(user.id), &tf_no).await?;
        }
        if short > Decimal::ZERO {
            stock::post_all(&mut tx, vec![Move::new(it, item, zone, lot, -short, "TRANSFER_SHORT").at(TRANSIT_LOCATION).reference("GODOWN_TRANSFER", id).by(Some(user.id)).note(format!("{tf_no}: {}", notes[i]))]).await?;
        }
        sqlx::query("UPDATE godown_transfer_lines SET qty_received=$2, qty_damaged=$3, note=$4 WHERE id=$1").bind(lid).bind(rec).bind(dmg).bind(&notes[i]).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE godown_transfers SET status='RECEIVED', received_by=$2, received_at=now() WHERE id=$1").bind(id).bind(user.id).execute(&mut *tx).await?;
    if dry { return finish_dry(tx, "GODOWN_TRANSFER", id, &warnings).await; }
    audit::log(&mut *tx, Some(&user), "GODOWN_TRANSFER_RECEIVE", "godown_transfer", Some(id.to_string()),
        json!({ "tf_no": tf_no, "to": to, "shortage": !short_w.is_empty(), "warnings": warnings })).await;
    let mut v = transfer_json(&mut tx, id).await?;
    tx.commit().await?;
    v["warnings"] = json!(warnings);
    ok(v)
}

// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelReq { reason: String, #[serde(default)] confirm: bool }

pub async fn cancel(State(s): State<AppState>, user: AuthUser, Id(id): Id, Q(dq): Q<DryQ>, J(b): J<CancelReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let dry = dq.on();
    let reason = validate::required_text("reason", &b.reason, 300)?;
    if reason.chars().count() < 5 { return Err(AppError::field("reason", "must be at least 5 characters")); }
    let mut tx = s.db.begin().await?;
    let t = sqlx::query("SELECT status, from_location_id, tf_no FROM godown_transfers WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?
        .ok_or_else(|| AppError::not_found("transfer"))?;
    let from: Uuid = t.get("from_location_id");
    let tf_no: String = t.get("tf_no");
    user.require_location(from)?;
    if t.get::<String, _>("status") != "IN_TRANSIT" { return Err(AppError::conflict("INVALID_STATE", "Only a transfer that is still in transit can be cancelled")); }
    bh::need_confirm(&[format!("{tf_no}: all goods go back to the source godown")], b.confirm, dry, "Cancelling a transfer returns the goods - please confirm")?;
    let rows = sqlx::query("SELECT item_type, item_id, lot_id, zone, qty_sent FROM godown_transfer_lines WHERE transfer_id=$1 ORDER BY item_type, item_id, zone, lot_id").bind(id).fetch_all(&mut *tx).await?;
    for r in rows {
        let it = if r.get::<String, _>("item_type") == "RAW" { ItemType::Raw } else { ItemType::Product };
        let zone = zone_static(&r.get::<String, _>("zone"))?;
        stock::transfer_between(&mut tx, it, r.get("item_id"), r.get("lot_id"), TRANSIT_LOCATION, zone, from, zone, r.get("qty_sent"),
            "GODOWN_TRANSFER_CANCEL", Some("GODOWN_TRANSFER"), Some(id), Some(user.id), &format!("{tf_no} cancelled: {reason}")).await?;
    }
    sqlx::query("UPDATE godown_transfers SET status='CANCELLED', cancel_reason=$2 WHERE id=$1").bind(id).bind(&reason).execute(&mut *tx).await?;
    if dry { return finish_dry(tx, "GODOWN_TRANSFER", id, &[]).await; }
    audit::log(&mut *tx, Some(&user), "GODOWN_TRANSFER_CANCEL", "godown_transfer", Some(id.to_string()), json!({ "tf_no": tf_no, "reason": reason })).await;
    let v = transfer_json(&mut tx, id).await?;
    tx.commit().await?;
    ok(v)
}

// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize, Default)]
pub struct ListQ { status: Option<String>, location_id: Option<Uuid>, from: Option<NaiveDate>, to: Option<NaiveDate>, limit: Option<i64>, offset: Option<i64> }

pub async fn list(State(s): State<AppState>, user: AuthUser, Q(q): Q<ListQ>) -> AppResult<Json<Value>> {
    if let Some(st) = &q.status { if !["IN_TRANSIT", "RECEIVED", "CANCELLED"].contains(&st.as_str()) { return Err(AppError::field("status", "unknown status")); } }
    let allowed = { let mut c = s.db.acquire().await?; bh::allowed_locations(&mut c, &user).await? };
    if let Some(l) = q.location_id { user.require_location(l)?; }
    let pq = validate::PageQuery { limit: q.limit, offset: q.offset };
    let off = s.settings.get().utc_offset_minutes;
    Ok(Json(bh::paged(&s.db,
        concat!("SELECT jsonb_build_object('id', t.id, 'tf_no', t.tf_no, 'status', t.status, 'from_location_id', t.from_location_id, 'from_name', lf.name, 'to_location_id', t.to_location_id, 'to_name', lt.name,
                  'vehicle_no', t.vehicle_no, 'created_at', t.created_at, 'received_at', t.received_at,
                  'lines_count', (SELECT count(*) FROM godown_transfer_lines l WHERE l.transfer_id = t.id),
                  'overdue', (t.status = 'IN_TRANSIT' AND t.created_at < now() - interval '48 hours')) AS j, t.created_at AS ca, t.id AS tid
           FROM godown_transfers t JOIN locations lf ON lf.id = t.from_location_id JOIN locations lt ON lt.id = t.to_location_id
           WHERE (t.from_location_id = ANY($1) OR t.to_location_id = ANY($1)) AND ($2::text IS NULL OR t.status = $2)
             AND ($3::uuid IS NULL OR t.from_location_id = $3 OR t.to_location_id = $3)
             AND ($4::date IS NULL OR ", local_date!("t.created_at", "$6"), " >= $4) AND ($5::date IS NULL OR ", local_date!("t.created_at", "$6"), " <= $5)"),
        "ca DESC, tid", &[Arg::UA(allowed), Arg::t(&q.status), Arg::U(q.location_id), Arg::D(q.from), Arg::D(q.to), Arg::I(off)], pq.limit(), pq.offset()).await?))
}

pub async fn detail(State(s): State<AppState>, user: AuthUser, Id(id): Id) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let v = transfer_json(&mut c, id).await?;
    if !user.can_use_location(loc(&v, "from_location_id")) && !user.can_use_location(loc(&v, "to_location_id")) {
        return Err(AppError::forbidden("You do not have access to this transfer"));
    }
    Ok(Json(v))
}
