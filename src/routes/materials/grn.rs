//! Goods receipt notes, GRN void and supplier returns (OWNER B, page `raw-materials`).
use crate::{
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    routes::ref_data::bh::{self, created, finish_dry, ok, Arg, DryQ, Id, J, Q, D},
    state::AppState,
    stock::{self, ItemType, Move},
    util, validate,
    workers::reorder,
};
use axum::{extract::State, http::StatusCode, Json};
use chrono::NaiveDate;
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrnLineReq { raw_material_id: Uuid, po_line_id: Option<Uuid>, qty_purchase: D, lot_no: Option<String>, expiry_date: Option<NaiveDate> }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrnReq {
    po_id: Option<Uuid>, location_id: Option<Uuid>, supplier_id: Uuid, challan_no: Option<String>, vehicle_no: Option<String>, notes: Option<String>,
    lines: Vec<GrnLineReq>,
    #[serde(default)] confirm: bool,
}

pub(crate) async fn grn_json(conn: &mut PgConnection, id: Uuid) -> AppResult<Value> {
    let v = bh::jval(&mut *conn,
        "SELECT jsonb_build_object('id', g.id, 'grn_no', g.grn_no, 'po_id', g.po_id, 'po_no', o.po_no, 'supplier_id', g.supplier_id, 'supplier_name', s.name,
            'received_on', g.received_on, 'location_id', g.location_id, 'location_name', lo.name, 'challan_no', g.challan_no, 'vehicle_no', g.vehicle_no, 'notes', g.notes, 'status', g.status,
            'void_reason', g.void_reason, 'voided_at', g.voided_at, 'created_at', g.created_at, 'received_by_name', u.full_name,
            'lines', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', l.id, 'lot_id', l.id, 'raw_material_id', l.raw_material_id, 'name', m.name, 'code', m.code,
                    'purchase_unit', m.purchase_unit, 'stock_unit', m.stock_unit, 'qty_purchase', l.qty_purchase::text, 'qty_stock', l.qty_stock::text,
                    'lot_no', l.lot_no, 'expiry_date', l.expiry_date, 'po_line_id', l.po_line_id) ORDER BY m.name, l.id)
                 FROM grn_lines l JOIN raw_materials m ON m.id = l.raw_material_id WHERE l.grn_id = g.id), '[]'::jsonb))
         FROM grns g JOIN suppliers s ON s.id = g.supplier_id JOIN locations lo ON lo.id = g.location_id LEFT JOIN purchase_orders o ON o.id = g.po_id LEFT JOIN users u ON u.id = g.received_by
         WHERE g.id = $1", &[Arg::U(Some(id))]).await;
    match v { Ok(v) if !v.is_null() => Ok(v), Ok(_) | Err(AppError::NotFound(_)) => Err(AppError::not_found("GRN")), Err(e) => Err(e) }
}

async fn recompute_po_status(tx: &mut PgConnection, po_id: Uuid) -> AppResult<()> {
    sqlx::query(
        "UPDATE purchase_orders SET status = CASE
            WHEN NOT EXISTS (SELECT 1 FROM po_lines WHERE po_id = $1 AND qty_received < qty_ordered) THEN 'RECEIVED'
            WHEN EXISTS (SELECT 1 FROM po_lines WHERE po_id = $1 AND qty_received > 0) THEN 'PARTIAL' ELSE 'OPEN' END
         WHERE id = $1 AND status <> 'CANCELLED'").bind(po_id).execute(&mut *tx).await?;
    Ok(())
}

pub async fn create(State(s): State<AppState>, user: AuthUser, Q(dq): Q<DryQ>, J(b): J<GrnReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let dry = dq.on();
    validate::max_items("lines", b.lines.len(), validate::MAX_LINES)?;
    let challan = bh::opt_text("challan_no", &b.challan_no, 60)?;
    let vehicle = bh::opt_text("vehicle_no", &b.vehicle_no, 30)?;
    let notes = bh::opt_text("notes", &b.notes, 500)?;
    let mut lot_nos = vec![];
    for l in &b.lines {
        validate::qty_positive("qty_purchase", l.qty_purchase.0)?;
        lot_nos.push(bh::opt_text("lot_no", &l.lot_no, 60)?);
    }
    if b.po_id.is_none() && b.lines.iter().any(|l| l.po_line_id.is_some()) { return Err(AppError::field("po_line_id", "requires po_id")); }

    let mut tx = s.db.begin().await?;
    let loc = bh::write_location(&mut tx, &user, b.location_id).await?;
    let today = util::plant_today(&mut tx).await?;
    for l in &b.lines {
        if let Some(e) = l.expiry_date { if e < today { return Err(AppError::field("expiry_date", "is already in the past")); } }
    }
    let sup_active: Option<bool> = sqlx::query_scalar("SELECT is_active FROM suppliers WHERE id=$1").bind(b.supplier_id).fetch_optional(&mut *tx).await?;
    if sup_active != Some(true) { return Err(AppError::field("supplier_id", "unknown or inactive supplier")); }
    if !challan.is_empty() {
        let dup: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM grns WHERE supplier_id=$1 AND lower(challan_no)=lower($2) AND status='ACTIVE')")
            .bind(b.supplier_id).bind(&challan).fetch_one(&mut *tx).await?;
        if dup { return Err(AppError::conflict("DUPLICATE", format!("A GRN for challan {challan} from this supplier already exists - open it, or void it first"))); }
    }

    // materials
    let ids: Vec<Uuid> = b.lines.iter().map(|l| l.raw_material_id).collect();
    let mrows = sqlx::query("SELECT id, name, stock_per_purchase_unit, is_active FROM raw_materials WHERE id = ANY($1)").bind(&ids).fetch_all(&mut *tx).await?;
    let mut mats: HashMap<Uuid, (String, Decimal, bool)> = HashMap::new();
    for r in &mrows { mats.insert(r.get("id"), (r.get("name"), r.get("stock_per_purchase_unit"), r.get("is_active"))); }
    for id in &ids { if !mats.get(id).map(|m| m.2).unwrap_or(false) { return Err(AppError::field("raw_material_id", "unknown or inactive raw material")); } }

    // purchase order + lines (locked in a stable order)
    let mut po_lines: Vec<(Uuid, Uuid, Decimal, Decimal)> = vec![]; // (line id, material, ordered, received)
    if let Some(po) = b.po_id {
        let row = sqlx::query("SELECT supplier_id, status FROM purchase_orders WHERE id=$1 FOR UPDATE").bind(po).fetch_optional(&mut *tx).await?
            .ok_or_else(|| AppError::field("po_id", "unknown purchase order"))?;
        if row.get::<Uuid, _>("supplier_id") != b.supplier_id { return Err(AppError::field("po_id", "belongs to a different supplier")); }
        if row.get::<String, _>("status") == "CANCELLED" { return Err(AppError::field("po_id", "purchase order is cancelled")); }
        let rows = sqlx::query("SELECT id, raw_material_id, qty_ordered, qty_received FROM po_lines WHERE po_id=$1 ORDER BY id FOR UPDATE").bind(po).fetch_all(&mut *tx).await?;
        for r in rows { po_lines.push((r.get("id"), r.get("raw_material_id"), r.get("qty_ordered"), r.get("qty_received"))); }
    }
    let mut line_po: Vec<Option<Uuid>> = vec![];
    let mut per_po_line: HashMap<Uuid, Decimal> = HashMap::new();
    for l in &b.lines {
        let pl = if b.po_id.is_some() {
            let found = match l.po_line_id {
                Some(x) => po_lines.iter().find(|p| p.0 == x && p.1 == l.raw_material_id),
                None => po_lines.iter().find(|p| p.1 == l.raw_material_id),
            };
            Some(found.ok_or_else(|| AppError::field("po_line_id", "this material is not on the purchase order"))?.0)
        } else { None };
        if let Some(p) = pl { *per_po_line.entry(p).or_default() += l.qty_purchase.0; }
        line_po.push(pl);
    }

    // over-receipt hard stop (110 %) and anomaly warnings
    let mut warnings: Vec<String> = vec![];
    for (pl, sum) in &per_po_line {
        let p = po_lines.iter().find(|p| p.0 == *pl).unwrap();
        if p.3 + *sum > p.2 * Decimal::new(11, 1) {
            return Err(AppError::conflict("BAD_REQUEST", format!(
                "Over-receipt: {} ordered, {} already received, {} more would exceed 110% of the order", p.2.normalize(), p.3.normalize(), sum.normalize())));
        }
    }
    for (i, l) in b.lines.iter().enumerate() {
        let name = &mats[&l.raw_material_id].0;
        if let Some(pl) = line_po[i] {
            let p = po_lines.iter().find(|p| p.0 == pl).unwrap();
            let remaining = p.2 - p.3;
            if remaining <= Decimal::ZERO { warnings.push(format!("{name}: the purchase order line is already fully received")); }
            else if l.qty_purchase.0 > remaining * Decimal::from(3) {
                warnings.push(format!("{name}: {} is more than 3x the remaining {} on the purchase order", l.qty_purchase.0.normalize(), remaining.normalize()));
            }
        }
        let hist: Vec<f64> = sqlx::query_scalar(
            "SELECT gl.qty_purchase::float8 FROM grn_lines gl JOIN grns g ON g.id = gl.grn_id WHERE gl.raw_material_id=$1 AND g.status='ACTIVE'
             ORDER BY g.created_at DESC LIMIT 10").bind(l.raw_material_id).fetch_all(&mut *tx).await?;
        if hist.len() >= 3 {
            let mut h = hist.clone();
            h.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let med = if h.len() % 2 == 1 { h[h.len() / 2] } else { (h[h.len() / 2 - 1] + h[h.len() / 2]) / 2.0 };
            if med > 0.0 && l.qty_purchase.0.to_f64().unwrap_or(0.0) > med * 10.0 {
                warnings.push(format!("{name}: {} is more than 10x the usual receipt (median {})", l.qty_purchase.0.normalize(), med));
            }
        }
    }
    bh::need_confirm(&warnings, b.confirm, dry, "This receipt looks unusual")?;

    // header, lines, stock
    let grn_id = Uuid::new_v4();
    let grn_no = bh::doc_no(&mut tx, "grn_seq", "GRN", dry).await?;
    sqlx::query("INSERT INTO grns (id, grn_no, po_id, supplier_id, received_on, challan_no, vehicle_no, notes, received_by, location_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
        .bind(grn_id).bind(&grn_no).bind(b.po_id).bind(b.supplier_id).bind(today).bind(&challan).bind(&vehicle).bind(&notes).bind(user.id).bind(loc)
        .execute(&mut *tx).await?;
    let mut moves = vec![];
    for (i, l) in b.lines.iter().enumerate() {
        let spp = mats[&l.raw_material_id].1;
        let qty_stock = (l.qty_purchase.0 * spp).round_dp(4);
        if qty_stock <= Decimal::ZERO || qty_stock > Decimal::from(validate::MAX_QTY) * Decimal::from(1000) { return Err(AppError::field("qty_purchase", "converted stock quantity is out of range")); }
        let line_id = Uuid::new_v4();
        sqlx::query("INSERT INTO grn_lines (id, grn_id, raw_material_id, po_line_id, qty_purchase, qty_stock, lot_no, expiry_date) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(line_id).bind(grn_id).bind(l.raw_material_id).bind(line_po[i]).bind(l.qty_purchase.0).bind(qty_stock).bind(&lot_nos[i]).bind(l.expiry_date)
            .execute(&mut *tx).await?;
        moves.push(Move::new(ItemType::Raw, l.raw_material_id, "RM_STORE", line_id, qty_stock, "GRN").at(loc)
            .reference("GRN", grn_id).by(Some(user.id)).note(grn_no.clone()).expiry(l.expiry_date));
    }
    stock::post_all(&mut tx, moves).await?;
    for (pl, sum) in &per_po_line {
        sqlx::query("UPDATE po_lines SET qty_received = qty_received + $2 WHERE id = $1").bind(pl).bind(sum).execute(&mut *tx).await?;
    }
    if let Some(po) = b.po_id { recompute_po_status(&mut tx, po).await?; }

    if dry { return finish_dry(tx, "GRN", grn_id, &warnings).await; }

    // goods arrived: recommendations that were waiting for this delivery are done; re-evaluate the engine for these materials
    let mut mids = ids.clone();
    mids.sort(); mids.dedup();
    sqlx::query("UPDATE reorder_recommendations SET status='RESOLVED', updated_at=now() WHERE raw_material_id = ANY($1) AND status='ORDERED'")
        .bind(&mids).execute(&mut *tx).await?;
    reorder::evaluate_materials(&mut tx, &s.settings.get(), &mids).await?;
    audit::log(&mut *tx, Some(&user), "GRN_CREATE", "grn", Some(grn_id.to_string()),
        json!({ "grn_no": grn_no, "supplier_id": b.supplier_id, "po_id": b.po_id, "lines": b.lines.len(), "challan_no": challan })).await;
    let mut v = grn_json(&mut tx, grn_id).await?;
    tx.commit().await?;
    v["warnings"] = json!(warnings);
    created(v)
}

#[derive(Deserialize, Default)]
pub struct ListQ { location_id: Option<String>, supplier_id: Option<Uuid>, status: Option<String>, from: Option<NaiveDate>, to: Option<NaiveDate>, q: Option<String>, limit: Option<i64>, offset: Option<i64> }

pub async fn list(State(s): State<AppState>, user: AuthUser, Q(q): Q<ListQ>) -> AppResult<Json<Value>> {
    let locs = { let mut c = s.db.acquire().await?; bh::list_scope(&mut c, &user, &q.location_id).await? };
    if let Some(st) = &q.status { if st != "ACTIVE" && st != "VOID" { return Err(AppError::field("status", "must be ACTIVE or VOID")); } }
    let pq = validate::PageQuery { limit: q.limit, offset: q.offset };
    let args = [Arg::U(q.supplier_id), Arg::t(&q.status), Arg::D(q.from), Arg::D(q.to), Arg::like(&q.q), Arg::UA(locs)];
    let v = bh::paged(&s.db,
        "SELECT jsonb_build_object('id', g.id, 'grn_no', g.grn_no, 'po_no', o.po_no, 'supplier_id', g.supplier_id, 'supplier_name', s.name, 'received_on', g.received_on, 'location_id', g.location_id, 'location_name', lo.name,
                'challan_no', g.challan_no, 'status', g.status, 'created_at', g.created_at,
                'lines_count', (SELECT count(*) FROM grn_lines l WHERE l.grn_id = g.id)) AS j, g.created_at AS ca, g.id AS gid
         FROM grns g JOIN suppliers s ON s.id = g.supplier_id JOIN locations lo ON lo.id = g.location_id LEFT JOIN purchase_orders o ON o.id = g.po_id
         WHERE g.location_id = ANY($6) AND ($1::uuid IS NULL OR g.supplier_id = $1) AND ($2::text IS NULL OR g.status = $2) AND ($3::date IS NULL OR g.received_on >= $3)
           AND ($4::date IS NULL OR g.received_on <= $4) AND ($5::text IS NULL OR g.grn_no ILIKE $5 OR g.challan_no ILIKE $5 OR s.name ILIKE $5)",
        "ca DESC, gid", &args, pq.limit(), pq.offset()).await?;
    Ok(Json(v))
}

pub async fn get(State(s): State<AppState>, user: AuthUser, Id(id): Id) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let v = grn_json(&mut c, id).await?;
    user.require_location(uid_of(&v["location_id"]))?;
    Ok(Json(v))
}

fn uid_of(v: &Value) -> Uuid { v.as_str().and_then(|x| Uuid::parse_str(x).ok()).unwrap_or(Uuid::nil()) }

// ------------------------------------------------------------------------------------------------------------------
// GRN void
// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoidReq { reason: String, #[serde(default)] confirm: bool }

pub async fn void(State(s): State<AppState>, user: AuthUser, Id(id): Id, Q(dq): Q<DryQ>, J(b): J<VoidReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let _ = b.confirm;
    let dry = dq.on();
    let reason = validate::required_text("reason", &b.reason, 300)?;
    if reason.chars().count() < 5 { return Err(AppError::field("reason", "must be at least 5 characters")); }
    let mut tx = s.db.begin().await?;
    let g = sqlx::query("SELECT status, po_id, grn_no, location_id FROM grns WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?
        .ok_or_else(|| AppError::not_found("GRN"))?;
    let loc: Uuid = g.get("location_id");
    user.require_location(loc)?;
    if g.get::<String, _>("status") != "ACTIVE" { return Err(AppError::conflict("INVALID_STATE", "This GRN is already void")); }
    let po_id: Option<Uuid> = g.get("po_id");
    let grn_no: String = g.get("grn_no");
    if let Some(po) = po_id {
        sqlx::query("SELECT id FROM purchase_orders WHERE id=$1 FOR UPDATE").bind(po).execute(&mut *tx).await?;
        sqlx::query("SELECT id FROM po_lines WHERE po_id=$1 ORDER BY id FOR UPDATE").bind(po).execute(&mut *tx).await?;
    }
    let lines = sqlx::query("SELECT l.id, l.raw_material_id, l.qty_stock, l.qty_purchase, l.po_line_id, m.name FROM grn_lines l JOIN raw_materials m ON m.id = l.raw_material_id
                             WHERE l.grn_id=$1 ORDER BY l.raw_material_id, l.id").bind(id).fetch_all(&mut *tx).await?;
    let mut moves = vec![];
    for l in &lines {
        let lot: Uuid = l.get("id");
        let rm: Uuid = l.get("raw_material_id");
        let qty: Decimal = l.get("qty_stock");
        let have: Decimal = sqlx::query_scalar("SELECT COALESCE((SELECT qty FROM stock_balances WHERE item_type='RAW' AND item_id=$1 AND zone='RM_STORE' AND lot_id=$2 AND location_id=$3 FOR UPDATE), 0)")
            .bind(rm).bind(lot).bind(loc).fetch_one(&mut *tx).await?;
        if have < qty {
            return Err(AppError::conflict_with("INSUFFICIENT_STOCK",
                format!("{}: already consumed or moved ({} of {} left in the store) - use a supplier return or an adjustment instead", l.get::<String, _>("name"), have.normalize(), qty.normalize()),
                json!({ "item_type": "RAW", "item_id": rm, "zone": "RM_STORE", "lot_id": lot, "available": have, "required": qty })));
        }
        moves.push(Move::new(ItemType::Raw, rm, "RM_STORE", lot, -qty, "GRN_VOID").at(loc).reference("GRN_VOID", id).by(Some(user.id)).note(format!("void {grn_no}: {reason}")));
        if let Some(pl) = l.get::<Option<Uuid>, _>("po_line_id") {
            let qp: Decimal = l.get("qty_purchase");
            sqlx::query("UPDATE po_lines SET qty_received = GREATEST(qty_received - $2, 0) WHERE id = $1").bind(pl).bind(qp).execute(&mut *tx).await?;
        }
    }
    stock::post_all(&mut tx, moves).await?;
    if let Some(po) = po_id { recompute_po_status(&mut tx, po).await?; }
    sqlx::query("UPDATE grns SET status='VOID', void_reason=$2, voided_by=$3, voided_at=now() WHERE id=$1").bind(id).bind(&reason).bind(user.id).execute(&mut *tx).await?;
    if dry { return finish_dry(tx, "GRN_VOID", id, &[]).await; }
    let mids: Vec<Uuid> = lines.iter().map(|l| l.get::<Uuid, _>("raw_material_id")).collect();
    reorder::evaluate_materials(&mut tx, &s.settings.get(), &mids).await?;
    audit::log(&mut *tx, Some(&user), "GRN_VOID", "grn", Some(id.to_string()), json!({ "grn_no": grn_no, "reason": reason })).await;
    let v = grn_json(&mut tx, id).await?;
    tx.commit().await?;
    ok(v)
}

// ------------------------------------------------------------------------------------------------------------------
// Supplier returns (return to vendor)
// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RtvLine { raw_material_id: Uuid, lot_id: Uuid, qty_stock: D, reason: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RtvReq { location_id: Option<Uuid>, supplier_id: Uuid, grn_id: Option<Uuid>, note: Option<String>, lines: Vec<RtvLine>, #[serde(default)] confirm: bool }

async fn rtv_json(conn: &mut PgConnection, id: Uuid) -> AppResult<Value> {
    let v = bh::jval(&mut *conn,
        "SELECT jsonb_build_object('id', r.id, 'rtv_no', r.rtv_no, 'location_id', r.location_id, 'location_name', lo.name, 'supplier_id', r.supplier_id, 'supplier_name', s.name, 'grn_id', r.grn_id, 'grn_no', g.grn_no,
            'note', r.note, 'created_at', r.created_at, 'created_by_name', u.full_name,
            'lines', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', l.id, 'raw_material_id', l.raw_material_id, 'name', m.name, 'code', m.code,
                    'stock_unit', m.stock_unit, 'lot_id', l.lot_id, 'lot_no', gl.lot_no, 'qty_stock', l.qty_stock::text, 'reason', l.reason) ORDER BY m.name, l.id)
                 FROM supplier_return_lines l JOIN raw_materials m ON m.id = l.raw_material_id LEFT JOIN grn_lines gl ON gl.id = l.lot_id WHERE l.return_id = r.id), '[]'::jsonb))
         FROM supplier_returns r JOIN suppliers s ON s.id = r.supplier_id JOIN locations lo ON lo.id = r.location_id LEFT JOIN grns g ON g.id = r.grn_id LEFT JOIN users u ON u.id = r.created_by
         WHERE r.id = $1", &[Arg::U(Some(id))]).await;
    match v { Ok(v) if !v.is_null() => Ok(v), Ok(_) | Err(AppError::NotFound(_)) => Err(AppError::not_found("supplier return")), Err(e) => Err(e) }
}

pub async fn rtv_create(State(s): State<AppState>, user: AuthUser, Q(dq): Q<DryQ>, J(b): J<RtvReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let _ = b.confirm;
    let dry = dq.on();
    validate::max_items("lines", b.lines.len(), validate::MAX_LINES)?;
    let note = bh::opt_text("note", &b.note, 500)?;
    let mut reasons = vec![];
    for l in &b.lines {
        validate::qty_positive("qty_stock", l.qty_stock.0)?;
        let r = validate::required_text("reason", &l.reason, 200)?;
        if r.chars().count() < 3 { return Err(AppError::field("reason", "must be at least 3 characters")); }
        reasons.push(r);
    }
    let mut tx = s.db.begin().await?;
    let loc = bh::write_location(&mut tx, &user, b.location_id).await?;
    let sup: Option<bool> = sqlx::query_scalar("SELECT is_active FROM suppliers WHERE id=$1").bind(b.supplier_id).fetch_optional(&mut *tx).await?;
    if sup != Some(true) { return Err(AppError::field("supplier_id", "unknown or inactive supplier")); }
    if let Some(g) = b.grn_id {
        let row = sqlx::query("SELECT supplier_id, status FROM grns WHERE id=$1").bind(g).fetch_optional(&mut *tx).await?
            .ok_or_else(|| AppError::field("grn_id", "unknown GRN"))?;
        if row.get::<Uuid, _>("supplier_id") != b.supplier_id { return Err(AppError::field("grn_id", "belongs to a different supplier")); }
        if row.get::<String, _>("status") != "ACTIVE" { return Err(AppError::field("grn_id", "GRN is void")); }
    }
    // every lot must come from an active GRN of this supplier (and of grn_id when given); cap at received minus earlier returns
    let mut per_lot: HashMap<Uuid, Decimal> = HashMap::new();
    for l in &b.lines {
        let r = sqlx::query(
            "SELECT gl.qty_stock, g.supplier_id, g.status, g.id AS grn_id FROM grn_lines gl JOIN grns g ON g.id = gl.grn_id WHERE gl.id=$1 AND gl.raw_material_id=$2")
            .bind(l.lot_id).bind(l.raw_material_id).fetch_optional(&mut *tx).await?
            .ok_or_else(|| AppError::field("lot_id", "unknown lot for this material"))?;
        if r.get::<Uuid, _>("supplier_id") != b.supplier_id || r.get::<String, _>("status") != "ACTIVE"
            || b.grn_id.map(|g| g != r.get::<Uuid, _>("grn_id")).unwrap_or(false) {
            return Err(AppError::field("lot_id", "this lot was not received from this supplier / GRN"));
        }
        let e = per_lot.entry(l.lot_id).or_default();
        *e += l.qty_stock.0;
        let returned: Decimal = sqlx::query_scalar("SELECT COALESCE(SUM(qty_stock),0) FROM supplier_return_lines WHERE lot_id=$1").bind(l.lot_id).fetch_one(&mut *tx).await?;
        if returned + *e > r.get::<Decimal, _>("qty_stock") {
            return Err(AppError::conflict("BAD_REQUEST", format!("Cannot return more than was received on this lot ({} received, {} already returned)",
                r.get::<Decimal, _>("qty_stock").normalize(), returned.normalize())));
        }
    }
    let id = Uuid::new_v4();
    let rtv_no = bh::doc_no(&mut tx, "rtv_seq", "RTV", dry).await?;
    sqlx::query("INSERT INTO supplier_returns (id, rtv_no, supplier_id, grn_id, note, created_by, location_id) VALUES ($1,$2,$3,$4,$5,$6,$7)")
        .bind(id).bind(&rtv_no).bind(b.supplier_id).bind(b.grn_id).bind(&note).bind(user.id).bind(loc).execute(&mut *tx).await?;
    let mut moves = vec![];
    for (i, l) in b.lines.iter().enumerate() {
        sqlx::query("INSERT INTO supplier_return_lines (return_id, raw_material_id, lot_id, qty_stock, reason) VALUES ($1,$2,$3,$4,$5)")
            .bind(id).bind(l.raw_material_id).bind(l.lot_id).bind(l.qty_stock.0).bind(&reasons[i]).execute(&mut *tx).await?;
        moves.push(Move::new(ItemType::Raw, l.raw_material_id, "RM_STORE", l.lot_id, -l.qty_stock.0, "RTV_OUT").at(loc)
            .reference("RTV", id).by(Some(user.id)).note(format!("{rtv_no}: {}", reasons[i])));
    }
    stock::post_all(&mut tx, moves).await?;
    if dry { return finish_dry(tx, "RTV", id, &[]).await; }
    let mids: Vec<Uuid> = b.lines.iter().map(|l| l.raw_material_id).collect();
    reorder::evaluate_materials(&mut tx, &s.settings.get(), &mids).await?;
    audit::log(&mut *tx, Some(&user), "SUPPLIER_RETURN", "supplier_return", Some(id.to_string()), json!({ "rtv_no": rtv_no, "supplier_id": b.supplier_id, "lines": b.lines.len() })).await;
    let v = rtv_json(&mut tx, id).await?;
    tx.commit().await?;
    created(v)
}

#[derive(Deserialize, Default)]
pub struct RtvListQ { location_id: Option<String>, supplier_id: Option<Uuid>, from: Option<NaiveDate>, to: Option<NaiveDate>, limit: Option<i64>, offset: Option<i64> }

pub async fn rtv_list(State(s): State<AppState>, user: AuthUser, Q(q): Q<RtvListQ>) -> AppResult<Json<Value>> {
    let locs = { let mut c = s.db.acquire().await?; bh::list_scope(&mut c, &user, &q.location_id).await? };
    let pq = validate::PageQuery { limit: q.limit, offset: q.offset };
    let v = bh::paged(&s.db,
        "SELECT jsonb_build_object('id', r.id, 'rtv_no', r.rtv_no, 'location_id', r.location_id, 'supplier_id', r.supplier_id, 'supplier_name', s.name, 'grn_id', r.grn_id, 'created_at', r.created_at,
                'lines_count', (SELECT count(*) FROM supplier_return_lines l WHERE l.return_id = r.id)) AS j, r.created_at AS ca, r.id AS rid
         FROM supplier_returns r JOIN suppliers s ON s.id = r.supplier_id
         WHERE r.location_id = ANY($4) AND ($1::uuid IS NULL OR r.supplier_id = $1) AND ($2::date IS NULL OR r.created_at >= $2) AND ($3::date IS NULL OR r.created_at < $3::date + 1)",
        "ca DESC, rid", &[Arg::U(q.supplier_id), Arg::D(q.from), Arg::D(q.to), Arg::UA(locs)], pq.limit(), pq.offset()).await?;
    Ok(Json(v))
}

pub async fn rtv_get(State(s): State<AppState>, user: AuthUser, Id(id): Id) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let v = rtv_json(&mut c, id).await?;
    user.require_location(uid_of(&v["location_id"]))?;
    Ok(Json(v))
}
