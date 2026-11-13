//! Bottling batches: BOM check, creation (BOM + dosing + waste consumption, QC quarantine output), list/detail, trace, summary.
use crate::{
    alerts, audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    routes::ref_data::bh::{self, batch_j, created, finish_dry, Arg, DryQ, Id, J, Q, D},
    state::AppState,
    stock::{self, ItemType, Move},
    util, validate,
};
use axum::{extract::State, http::StatusCode, response::Response, Json};
use chrono::NaiveDate;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::HashMap;
use uuid::Uuid;

pub struct ProductInfo { pub id: Uuid, pub code: String, pub name: String, pub kind: String, pub size_ml: Option<i32>, pub units_per_crate: i32,
                         pub crates_per_pallet: Option<i32>, pub shelf_life_days: i32, pub is_active: bool }

#[derive(Clone, Debug)]
pub struct Req {
    pub item: ItemType, pub item_id: Uuid, pub name: String, pub unit: String, pub zone: &'static str, pub required: Decimal,
    pub basis: String, pub qty_per: Decimal, pub container: bool, pub category: String,
}

fn static_zone(z: &str) -> &'static str { if z == "BLOW_FLOOR" { "BLOW_FLOOR" } else { "RM_STORE" } }

pub async fn load_product(conn: &mut PgConnection, id: Uuid) -> AppResult<ProductInfo> {
    let r = sqlx::query("SELECT id, code, name, kind, size_ml, units_per_crate, crates_per_pallet, shelf_life_days, is_active FROM products WHERE id=$1")
        .bind(id).fetch_optional(&mut *conn).await?.ok_or_else(|| AppError::field("product_id", "unknown product"))?;
    Ok(ProductInfo { id: r.get("id"), code: r.get("code"), name: r.get("name"), kind: r.get("kind"), size_ml: r.get("size_ml"),
        units_per_crate: r.get("units_per_crate"), crates_per_pallet: r.get("crates_per_pallet"), shelf_life_days: r.get("shelf_life_days"), is_active: r.get("is_active") })
}

fn up4(d: Decimal) -> Decimal { d.round_dp_with_strategy(4, RoundingStrategy::AwayFromZero) }

/// Required quantities for `crates` of the product: BOM lines (PER_CRATE / PER_1000L scaling) and, for RETURNABLE products,
/// the containers themselves from JAR_CLEAN. RETURNABLE products never consume PREFORM / EMPTY_BOTTLE.
pub async fn requirements(conn: &mut PgConnection, p: &ProductInfo, crates: i64) -> AppResult<(Vec<Req>, Option<Decimal>)> {
    let litres = p.size_ml.map(|ml| Decimal::from(crates) * Decimal::from(p.units_per_crate) * Decimal::from(ml) / Decimal::from(1000));
    let rows = sqlx::query(
        "SELECT b.raw_material_id, b.qty_per_crate, b.source_zone, b.basis, m.name, m.stock_unit, m.category
         FROM bom_items b JOIN raw_materials m ON m.id = b.raw_material_id WHERE b.product_id = $1 ORDER BY m.name, b.id")
        .bind(p.id).fetch_all(&mut *conn).await?;
    let mut out = vec![];
    for r in rows {
        let category: String = r.get("category");
        if p.kind == "RETURNABLE" && (category == "PREFORM" || category == "EMPTY_BOTTLE") { continue; }
        let basis: String = r.get("basis");
        let per: Decimal = r.get("qty_per_crate");
        let required = if basis == "PER_1000L" {
            let l = litres.ok_or_else(|| AppError::bad(format!("Product {} has no size_ml, so per-1000-litre dosing cannot be calculated", p.name)))?;
            up4(per * l / Decimal::from(1000))
        } else { up4(per * Decimal::from(crates)) };
        out.push(Req { item: ItemType::Raw, item_id: r.get("raw_material_id"), name: r.get("name"), unit: r.get("stock_unit"),
            zone: if category == "EMPTY_BOTTLE" { "BLOW_FLOOR" } else { static_zone(&r.get::<String, _>("source_zone")) },
            required, basis, qty_per: per, container: false, category });
    }
    if p.kind == "RETURNABLE" {
        out.push(Req { item: ItemType::Product, item_id: p.id, name: format!("{} (clean containers)", p.name), unit: "pcs".into(), zone: "JAR_CLEAN",
            required: Decimal::from(crates), basis: "PER_CRATE".into(), qty_per: Decimal::ONE, container: true, category: "CONTAINER".into() });
    }
    Ok((out, litres))
}

async fn available(conn: &mut PgConnection, r: &Req) -> AppResult<Decimal> { Ok(stock::available(conn, r.item, r.item_id, r.zone).await?) }

fn line_json(r: &Req, avail: Decimal, extra_required: Decimal) -> Value {
    let total = r.required + extra_required;
    json!({ "raw_material_id": if r.container { Value::Null } else { json!(r.item_id) }, "item_type": r.item.as_str(), "item_id": r.item_id, "name": r.name, "unit": r.unit,
            "zone": r.zone, "basis": r.basis, "qty_per": r.qty_per.normalize().to_string(), "required": total.to_string(), "available": avail.to_string(),
            "short": avail < total, "shortfall": (total - avail).max(Decimal::ZERO).to_string(), "container": r.container })
}

#[derive(Deserialize, Default)]
pub struct BomQ { product_id: Option<Uuid>, crates: Option<i64> }

pub async fn bom_check(State(s): State<AppState>, Q(q): Q<BomQ>) -> AppResult<Json<Value>> {
    let pid = q.product_id.ok_or_else(|| AppError::field("product_id", "is required"))?;
    let crates = q.crates.ok_or_else(|| AppError::field("crates", "is required"))?;
    validate::count("crates", crates)?;
    let mut c = s.db.acquire().await?;
    let p = load_product(&mut c, pid).await?;
    let (reqs, litres) = requirements(&mut c, &p, crates).await?;
    let mut lines = vec![];
    let mut ok = !reqs.is_empty();
    for r in &reqs {
        let a = available(&mut c, r).await?;
        if a < r.required { ok = false; }
        lines.push(line_json(r, a, Decimal::ZERO));
    }
    let (pal, loose) = bh::pallets(crates, p.crates_per_pallet.map(|x| x as i64));
    Ok(Json(json!({ "ok": ok, "product_id": p.id, "product_name": p.name, "kind": p.kind, "crates": crates, "litres": litres.map(|l| l.normalize().to_string()),
        "crates_per_pallet": p.crates_per_pallet, "pallets": pal, "loose_crates": loose, "lines": lines })))
}

// ------------------------------------------------------------------------------------------------------------------
// Create
// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WasteReq { raw_material_id: Uuid, qty: D, reason: String }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchReq {
    product_id: Uuid, crates: i64, shift: String, produced_on: Option<NaiveDate>, water_log_id: Option<Uuid>, notes: Option<String>,
    #[serde(default)] waste: Vec<WasteReq>,
    #[serde(default)] confirm: bool,
}

pub async fn create(State(s): State<AppState>, user: AuthUser, Q(dq): Q<DryQ>, J(b): J<BatchReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let dry = dq.on();
    validate::count("crates", b.crates)?;
    let shift = bh::shift(&b.shift)?;
    let notes = bh::opt_text("notes", &b.notes, 500)?;
    if b.waste.len() > 50 { return Err(AppError::field("waste", "at most 50 lines")); }
    let mut waste_reason = vec![];
    for w in &b.waste {
        validate::qty_positive("waste.qty", w.qty.0)?;
        let r = validate::required_text("waste.reason", &w.reason, 200)?;
        if r.chars().count() < 3 { return Err(AppError::field("waste.reason", "must be at least 3 characters")); }
        waste_reason.push(r);
    }
    {
        let mut ids: Vec<Uuid> = b.waste.iter().map(|w| w.raw_material_id).collect();
        ids.sort(); let n = ids.len(); ids.dedup();
        if ids.len() != n { return Err(AppError::field("waste", "each material may appear only once")); }
    }
    let st = s.settings.get();
    let mut tx = s.db.begin().await?;
    let today = util::plant_today(&mut tx).await?;
    let produced_on = b.produced_on.unwrap_or(today);
    bh::check_entry_date("produced_on", produced_on, today, 30)?;
    let p = load_product(&mut tx, b.product_id).await?;
    if !p.is_active { return Err(AppError::field("product_id", "product is inactive")); }
    if let Some(w) = b.water_log_id {
        let e: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM water_treatment_logs WHERE id=$1)").bind(w).fetch_one(&mut *tx).await?;
        if !e { return Err(AppError::field("water_log_id", "unknown water log")); }
    }
    let (reqs, litres) = requirements(&mut tx, &p, b.crates).await?;
    if reqs.is_empty() { return Err(AppError::bad("This product has no bill of materials - ask an administrator to configure it")); }

    // waste lines: only materials that the BOM uses, never empty bottles
    let mut waste: Vec<(usize, Decimal, String)> = vec![]; // (req index, qty, reason)
    let mut soft: Vec<String> = vec![];
    let mut confirm_w: Vec<String> = vec![];
    for (i, w) in b.waste.iter().enumerate() {
        let idx = reqs.iter().position(|r| !r.container && r.item_id == w.raw_material_id && r.category != "EMPTY_BOTTLE")
            .ok_or_else(|| AppError::field("waste.raw_material_id", "must be a consumable material of this product's BOM (not an empty bottle)"))?;
        let r = &reqs[idx];
        let pct = if r.required > Decimal::ZERO { w.qty.0 * Decimal::from(100) / r.required } else { Decimal::from(100) };
        if pct > Decimal::from(25) { confirm_w.push(format!("{}: waste {} is {}% of the BOM quantity {} (more than 25%)", r.name, w.qty.0.normalize(), pct.round_dp(1).normalize(), r.required.normalize())); }
        else if pct > Decimal::from(5) { soft.push(format!("{}: waste is {}% of the BOM quantity (more than 5%)", r.name, pct.round_dp(1).normalize())); }
        waste.push((idx, w.qty.0, waste_reason[i].clone()));
    }

    // hard stop: shortages over ALL lines (BOM + waste), reported together
    let mut shortages = vec![];
    let mut lines_json = vec![];
    for (i, r) in reqs.iter().enumerate() {
        let extra: Decimal = waste.iter().filter(|w| w.0 == i).map(|w| w.1).sum();
        let a = available(&mut tx, r).await?;
        if a < r.required + extra {
            shortages.push(json!({ "raw_material_id": if r.container { Value::Null } else { json!(r.item_id) }, "item_type": r.item.as_str(), "item_id": r.item_id, "name": r.name,
                "zone": r.zone, "required": (r.required + extra).to_string(), "available": a.to_string() }));
        }
        lines_json.push(line_json(r, a, extra));
    }
    if !shortages.is_empty() {
        return Err(AppError::conflict_with("INSUFFICIENT_STOCK", format!("Not enough material for {} line(s)", shortages.len()), json!({ "shortages": shortages })));
    }

    // quality gates + anomaly checks
    let hist: Vec<i32> = sqlx::query_scalar("SELECT crates FROM production_batches WHERE product_id=$1 ORDER BY created_at DESC LIMIT 10").bind(b.product_id).fetch_all(&mut *tx).await?;
    if let Some(mx) = hist.iter().max() {
        if b.crates > 2 * (*mx as i64) { confirm_w.push(format!("{} crates is more than twice your largest recent batch ({} crates)", b.crates, mx)); }
    }
    let last_log = sqlx::query("SELECT out_of_range, logged_at < now() - interval '60 minutes' AS stale FROM water_treatment_logs ORDER BY logged_at DESC LIMIT 1")
        .fetch_optional(&mut *tx).await?;
    match &last_log {
        None => soft.push("No water log in the last hour".into()),
        Some(r) => {
            if r.get::<bool, _>("stale") { soft.push("No water log in the last hour".into()); }
            if r.get::<bool, _>("out_of_range") { confirm_w.push("The most recent water treatment reading was OUT OF RANGE".into()); }
        }
    }
    if let Some(w) = b.water_log_id {
        let oor: bool = sqlx::query_scalar("SELECT out_of_range FROM water_treatment_logs WHERE id=$1").bind(w).fetch_one(&mut *tx).await?;
        if oor { confirm_w.push("The selected water log was OUT OF RANGE".into()); }
    }
    if let Some(cpp) = p.crates_per_pallet {
        if b.crates % (cpp as i64) != 0 { soft.push(format!("{} crates is not a whole number of pallets ({} crates per pallet)", b.crates, cpp)); }
    }
    let mfg = produced_on;
    let expiry = mfg + chrono::Duration::days(p.shelf_life_days as i64);
    let left = (expiry - today).num_days();
    if left * 10 < p.shelf_life_days as i64 { soft.push("This batch is already within 10% of its shelf life".into()); }
    bh::need_confirm(&confirm_w, b.confirm, dry, "Please review before producing this batch")?;
    let mut warnings = soft;
    warnings.extend(confirm_w);

    // insert + consume (deterministic lock order across concurrent batches)
    let id = Uuid::new_v4();
    let batch_no = bh::doc_no(&mut tx, "batch_seq", &format!("{}-B", st.plant_code), dry).await?;
    sqlx::query("INSERT INTO production_batches (id, batch_no, product_id, produced_on, shift, crates, mfg_date, expiry_date, water_log_id, notes, created_by)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
        .bind(id).bind(&batch_no).bind(b.product_id).bind(produced_on).bind(shift).bind(b.crates as i32).bind(mfg).bind(expiry).bind(b.water_log_id).bind(&notes).bind(user.id)
        .execute(&mut *tx).await?;
    let mut order: Vec<usize> = (0..reqs.len()).collect();
    order.sort_by(|a, c| (reqs[*a].item, reqs[*a].item_id, reqs[*a].zone).cmp(&(reqs[*c].item, reqs[*c].item_id, reqs[*c].zone)));
    let mut raw_ids = vec![];
    for i in order {
        let r = &reqs[i];
        stock::consume_fefo(&mut tx, r.item, r.item_id, r.zone, r.required, "BATCH_CONSUME", Some("BATCH"), Some(id), Some(user.id), &batch_no).await?;
        if !r.container {
            raw_ids.push(r.item_id);
            sqlx::query("INSERT INTO batch_material_usage (batch_id, raw_material_id, qty, zone) VALUES ($1,$2,$3,$4)")
                .bind(id).bind(r.item_id).bind(r.required).bind(r.zone).execute(&mut *tx).await?;
        }
        for (_, wq, reason) in waste.iter().filter(|w| w.0 == i) {
            stock::consume_fefo(&mut tx, r.item, r.item_id, r.zone, *wq, "BATCH_WASTE", Some("BATCH"), Some(id), Some(user.id), &format!("{batch_no} waste: {reason}")).await?;
            sqlx::query("INSERT INTO batch_material_usage (batch_id, raw_material_id, qty, zone) VALUES ($1,$2,$3,$4)")
                .bind(id).bind(r.item_id).bind(*wq).bind(r.zone).execute(&mut *tx).await?;
        }
    }
    stock::post_all(&mut tx, vec![Move::new(ItemType::Product, b.product_id, "QC_QUARANTINE", id, Decimal::from(b.crates), "BATCH_OUTPUT")
        .reference("BATCH", id).by(Some(user.id)).note(batch_no.clone()).expiry(Some(expiry))]).await?;
    if dry { return finish_dry(tx, "BATCH", id, &warnings).await; }
    alerts::raise(&mut *tx, alerts::NewAlert {
        event_type: "QC_PENDING", dedupe_key: &id.to_string(), severity: "INFO", title: &format!("Batch {batch_no} awaiting QC"),
        body: &format!("{} crates of {}", b.crates, p.name), url: "/qc.html", payload: json!({ "batch_id": id }),
    }).await?;
    audit::log(&mut *tx, Some(&user), "BATCH_CREATE", "production_batch", Some(id.to_string()),
        json!({ "batch_no": batch_no, "product_id": b.product_id, "crates": b.crates, "waste_lines": waste.len() })).await;
    let mut v = batch_detail(&mut tx, id).await?;
    tx.commit().await?;
    v["warnings"] = json!(warnings);
    v["litres"] = json!(litres.map(|l| l.normalize().to_string()));
    v["lines"] = json!(lines_json);
    super::spawn_reorder_check(&s, raw_ids);
    created(v)
}

// ------------------------------------------------------------------------------------------------------------------
// Read
// ------------------------------------------------------------------------------------------------------------------
pub async fn batch_detail(conn: &mut PgConnection, id: Uuid) -> AppResult<Value> {
    let v = bh::jval(&mut *conn,
        concat!("SELECT ", batch_j!(), " || jsonb_build_object(
            'litres', CASE WHEN p.size_ml IS NOT NULL THEN (b.crates::numeric * p.units_per_crate * p.size_ml / 1000)::text END,
            'water_log_id', b.water_log_id, 'notes', b.notes, 'qc_notes', b.qc_notes, 'decided_by_name', du.full_name, 'created_by_name', cu.full_name,
            'product', jsonb_build_object('id', p.id, 'code', p.code, 'name', p.name, 'kind', p.kind, 'size_ml', p.size_ml, 'units_per_crate', p.units_per_crate,
                 'crates_per_pallet', p.crates_per_pallet, 'shelf_life_days', p.shelf_life_days),
            'materials', COALESCE((SELECT jsonb_agg(jsonb_build_object('raw_material_id', x.rid, 'name', x.name, 'code', x.code, 'unit', x.unit, 'zone', x.zone,
                       'qty', x.qty::text, 'waste_qty', x.waste::text, 'basis', x.basis) ORDER BY x.name)
                  FROM (SELECT u.raw_material_id AS rid, m.name, m.code, m.stock_unit AS unit, u.zone, SUM(u.qty) AS qty,
                               COALESCE((SELECT SUM(-l.qty_delta) FROM stock_ledger l WHERE l.ref_type='BATCH' AND l.ref_id=b.id AND l.txn_type='BATCH_WASTE'
                                         AND l.item_type='RAW' AND l.item_id=u.raw_material_id), 0) AS waste,
                               (SELECT bi.basis FROM bom_items bi WHERE bi.product_id=b.product_id AND bi.raw_material_id=u.raw_material_id) AS basis
                        FROM batch_material_usage u JOIN raw_materials m ON m.id = u.raw_material_id WHERE u.batch_id = b.id
                        GROUP BY u.raw_material_id, m.name, m.code, m.stock_unit, u.zone) x), '[]'::jsonb),
            'waste', COALESCE((SELECT jsonb_agg(jsonb_build_object('raw_material_id', l.item_id, 'name', m.name, 'qty', (-l.qty_delta)::text, 'zone', l.zone, 'reason', l.note) ORDER BY l.id)
                  FROM stock_ledger l JOIN raw_materials m ON m.id = l.item_id
                  WHERE l.ref_type='BATCH' AND l.ref_id=b.id AND l.txn_type='BATCH_WASTE'), '[]'::jsonb),
            'tests', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', t.id, 'tested_at', t.tested_at, 'tested_by_name', tu.full_name, 'tds_ppm', t.tds_ppm::text,
                       'ph', t.ph::text, 'ozone_ppm', t.ozone_ppm::text, 'turbidity_ntu', t.turbidity_ntu::text, 'microbial_ok', t.microbial_ok,
                       'seal_ok', t.seal_ok, 'label_ok', t.label_ok, 'result', t.result, 'remarks', t.remarks) ORDER BY t.tested_at DESC, t.id)
                  FROM qc_tests t LEFT JOIN users tu ON tu.id = t.tested_by WHERE t.batch_id = b.id), '[]'::jsonb),
            'sets_summary', (SELECT jsonb_build_object('count', count(*), 'allocated_crates', COALESCE(SUM(original_crates) FILTER (WHERE parent_set_id IS NULL), 0))
                  FROM qr_crate_sets WHERE batch_id = b.id),
            'stock', COALESCE((SELECT jsonb_object_agg(sb.zone, sb.qty::text) FROM stock_balances sb
                  WHERE sb.item_type='PRODUCT' AND sb.item_id=b.product_id AND sb.lot_id=b.id AND sb.qty > 0), '{}'::jsonb))
         FROM production_batches b JOIN products p ON p.id = b.product_id LEFT JOIN users du ON du.id = b.qc_decided_by LEFT JOIN users cu ON cu.id = b.created_by
         WHERE b.id = $1"), &[Arg::U(Some(id))]).await;
    match v { Ok(v) if !v.is_null() => Ok(v), Ok(_) | Err(AppError::NotFound(_)) => Err(AppError::not_found("batch")), Err(e) => Err(e) }
}

pub async fn get(State(s): State<AppState>, Id(id): Id) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    Ok(Json(batch_detail(&mut c, id).await?))
}

#[derive(Deserialize, Default)]
pub struct ListQ { qc_status: Option<String>, product_id: Option<Uuid>, from: Option<NaiveDate>, to: Option<NaiveDate>, q: Option<String>, format: Option<String>,
                   limit: Option<i64>, offset: Option<i64> }

pub async fn list(State(s): State<AppState>, Q(q): Q<ListQ>) -> AppResult<Response> {
    if let Some(st) = &q.qc_status { if !["QUARANTINE", "APPROVED", "REJECTED"].contains(&st.as_str()) { return Err(AppError::field("qc_status", "unknown status")); } }
    let csv = bh::is_csv(&q.format);
    let pq = validate::PageQuery { limit: q.limit, offset: q.offset };
    let (limit, offset) = if csv { (bh::CSV_MAX_ROWS, 0) } else { (pq.limit(), pq.offset()) };
    let v = bh::paged(&s.db,
        concat!("SELECT ", batch_j!(), " AS j, b.created_at AS ca, b.id AS bid FROM production_batches b JOIN products p ON p.id = b.product_id
         WHERE ($1::text IS NULL OR b.qc_status = $1) AND ($2::uuid IS NULL OR b.product_id = $2) AND ($3::date IS NULL OR b.produced_on >= $3)
           AND ($4::date IS NULL OR b.produced_on <= $4) AND ($5::text IS NULL OR b.batch_no ILIKE $5 OR p.name ILIKE $5)"),
        "ca DESC, bid", &[Arg::t(&q.qc_status), Arg::U(q.product_id), Arg::D(q.from), Arg::D(q.to), Arg::like(&q.q)], limit, offset).await?;
    if csv {
        return Ok(bh::csv_response("batches.csv", &[("Batch", "batch_no"), ("Product", "product_name"), ("Crates", "crates"), ("QC status", "qc_status"),
            ("Produced on", "produced_on"), ("Mfg date", "mfg_date"), ("Expiry", "expiry_date"), ("Shift", "shift"), ("Created", "created_at")], bh::items_of(&v)));
    }
    Ok(bh::json_resp(v))
}

// ------------------------------------------------------------------------------------------------------------------
// Traceability / recall view
// ------------------------------------------------------------------------------------------------------------------
pub async fn trace(State(s): State<AppState>, Id(id): Id) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let batch = batch_detail(&mut c, id).await?;
    let a = [Arg::U(Some(id))];
    let materials = bh::jval(&mut *c,
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('item_type', l.item_type, 'raw_material_id', CASE WHEN l.item_type='RAW' THEN l.item_id END, 'name', COALESCE(m.name, pr.name),
            'kind', CASE WHEN l.txn_type='BATCH_WASTE' THEN 'WASTE' ELSE 'CONSUMED' END, 'zone', l.zone, 'qty', (-l.qty_delta)::text, 'lot_id', l.lot_id,
            'lot_no', gl.lot_no, 'grn_no', g.grn_no, 'challan_no', g.challan_no, 'received_on', g.received_on, 'expiry_date', gl.expiry_date,
            'supplier_id', g.supplier_id, 'supplier_name', sp.name) ORDER BY l.id), '[]'::jsonb)
         FROM stock_ledger l LEFT JOIN raw_materials m ON l.item_type='RAW' AND m.id = l.item_id LEFT JOIN products pr ON l.item_type='PRODUCT' AND pr.id = l.item_id
         LEFT JOIN grn_lines gl ON gl.id = l.lot_id LEFT JOIN grns g ON g.id = gl.grn_id LEFT JOIN suppliers sp ON sp.id = g.supplier_id
         WHERE l.ref_type='BATCH' AND l.ref_id=$1 AND l.txn_type IN ('BATCH_CONSUME','BATCH_WASTE')", &a).await?;
    let sets = bh::jval(&mut *c,
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('id', q.id, 'code', q.code, 'set_label', q.set_label, 'status', q.status, 'original_crates', q.original_crates,
            'crates', q.crates, 'trip_id', q.trip_id, 'trip_no', t.trip_no, 'trip_status', t.status) ORDER BY q.seq, q.id), '[]'::jsonb)
         FROM qr_crate_sets q LEFT JOIN loading_trips t ON t.id = q.trip_id WHERE q.batch_id = $1", &a).await?;
    let trips = bh::jval(&mut *c,
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('trip_id', t.id, 'trip_no', t.trip_no, 'status', t.status, 'vehicle_reg', v.reg_no, 'driver_name', t.driver_name,
            'route_name', r.name, 'gate_pass_no', t.gate_pass_no, 'gate_pass_issued_at', t.gate_pass_issued_at, 'crates', ti.crates, 'delivered', ti.crates_delivered,
            'returned', ti.crates_returned, 'damaged', ti.crates_damaged, 'qr_set_id', ti.qr_set_id) ORDER BY t.created_at, ti.id), '[]'::jsonb)
         FROM trip_items ti JOIN loading_trips t ON t.id = ti.trip_id JOIN vehicles v ON v.id = t.vehicle_id LEFT JOIN routes r ON r.id = t.route_id WHERE ti.batch_id = $1", &a).await?;
    let dispatches = bh::jval(&mut *c,
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('dn_id', n.id, 'dn_no', n.dn_no, 'status', n.status, 'customer_id', n.customer_id,
            'party', COALESCE(cu.name, n.party_name), 'crates', dl.crates, 'created_at', n.created_at) ORDER BY n.created_at), '[]'::jsonb)
         FROM dispatch_lines dl JOIN dispatch_notes n ON n.id = dl.dn_id LEFT JOIN customers cu ON cu.id = n.customer_id WHERE dl.batch_id = $1", &a).await?;
    let totals = bh::jval(&mut *c,
        "SELECT jsonb_build_object(
            'produced', (SELECT crates FROM production_batches WHERE id=$1),
            'loaded', COALESCE((SELECT SUM(ti.crates) FROM trip_items ti JOIN loading_trips t ON t.id=ti.trip_id WHERE ti.batch_id=$1 AND t.status IN ('DISPATCHED','SETTLED')), 0),
            'on_open_trips', COALESCE((SELECT SUM(ti.crates) FROM trip_items ti JOIN loading_trips t ON t.id=ti.trip_id WHERE ti.batch_id=$1 AND t.status='LOADING'), 0),
            'delivered', COALESCE((SELECT SUM(ti.crates_delivered) FROM trip_items ti WHERE ti.batch_id=$1), 0),
            'returned', COALESCE((SELECT SUM(ti.crates_returned) FROM trip_items ti WHERE ti.batch_id=$1), 0),
            'damaged', COALESCE((SELECT SUM(ti.crates_damaged) FROM trip_items ti WHERE ti.batch_id=$1), 0),
            'direct_dispatched', COALESCE((SELECT SUM(dl.crates) FROM dispatch_lines dl JOIN dispatch_notes n ON n.id=dl.dn_id WHERE dl.batch_id=$1 AND n.status='ISSUED'), 0),
            'still_in_stock', COALESCE((SELECT SUM(qty) FROM stock_balances WHERE item_type='PRODUCT' AND lot_id=$1 AND zone IN ('QC_QUARANTINE','FG_READY')), 0)::text)", &a).await?;
    Ok(Json(json!({ "batch": batch, "materials": materials, "tests": batch["tests"], "sets": sets, "trips": trips, "dispatches": dispatches, "totals": totals })))
}

// ------------------------------------------------------------------------------------------------------------------
// Production summary
// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize, Default)]
pub struct SumQ { from: Option<NaiveDate>, to: Option<NaiveDate> }

pub async fn summary(State(s): State<AppState>, Q(q): Q<SumQ>) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let today = util::plant_today(&mut c).await?;
    let to = q.to.unwrap_or(today);
    let from = q.from.unwrap_or(to - chrono::Duration::days(29));
    if from > to { return Err(AppError::field("from", "must not be after 'to'")); }
    if (to - from).num_days() > 366 { return Err(AppError::field("from", "range is limited to 366 days")); }
    let a = [Arg::D(Some(from)), Arg::D(Some(to))];
    let days = bh::jval(&mut *c,
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('date', d, 'batches', COALESCE(bt.n,0), 'crates', COALESCE(bt.crates,0), 'bottles', COALESCE(bt.bottles,0),
            'blow_good', COALESCE(bl.good,0)::text, 'blow_scrap', COALESCE(bl.scrap,0)::text,
            'scrap_pct', CASE WHEN COALESCE(bl.good,0)+COALESCE(bl.scrap,0) > 0 THEN round(bl.scrap * 100 / (bl.good+bl.scrap), 2)::text ELSE '0' END) ORDER BY d), '[]'::jsonb)
         FROM (SELECT d::date AS d FROM generate_series($1::date, $2::date, interval '1 day') d) days
         LEFT JOIN (SELECT b.produced_on AS pd, count(*) AS n, SUM(b.crates) AS crates, SUM(b.crates * p.units_per_crate) AS bottles
                    FROM production_batches b JOIN products p ON p.id=b.product_id WHERE b.produced_on BETWEEN $1 AND $2 GROUP BY b.produced_on) bt ON bt.pd = days.d
         LEFT JOIN (SELECT run_date AS rd, SUM(good_bottles) AS good, SUM(scrap_pcs) AS scrap FROM blow_molding_runs WHERE run_date BETWEEN $1 AND $2 GROUP BY run_date) bl ON bl.rd = days.d
         WHERE bt.pd IS NOT NULL OR bl.rd IS NOT NULL", &a).await?;
    let by_product = bh::jval(&mut *c,
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('product_id', x.pid, 'name', x.name, 'batches', x.n, 'crates', x.crates, 'bottles', x.bottles) ORDER BY x.name), '[]'::jsonb)
         FROM (SELECT p.id AS pid, p.name, count(*) AS n, SUM(b.crates) AS crates, SUM(b.crates * p.units_per_crate) AS bottles
               FROM production_batches b JOIN products p ON p.id=b.product_id WHERE b.produced_on BETWEEN $1 AND $2 GROUP BY p.id, p.name) x", &a).await?;
    let usage = bh::jval(&mut *c,
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('raw_material_id', x.rid, 'name', x.name, 'unit', x.unit, 'qty', x.qty::text) ORDER BY x.name), '[]'::jsonb)
         FROM (SELECT m.id AS rid, m.name, m.stock_unit AS unit, SUM(u.qty) AS qty FROM batch_material_usage u JOIN production_batches b ON b.id=u.batch_id
               JOIN raw_materials m ON m.id=u.raw_material_id WHERE b.produced_on BETWEEN $1 AND $2 GROUP BY m.id, m.name, m.stock_unit) x", &a).await?;
    Ok(Json(json!({ "from": from, "to": to, "days": days, "by_product": by_product, "material_usage": usage })))
}

#[allow(dead_code)]
fn _unused(_: HashMap<u8, u8>) {}
