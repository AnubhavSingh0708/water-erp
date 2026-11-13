//! `/api/warehouse/*` – stock views, ledger, expiring lots (OWNER B, page `warehouse`).
//! Corrections (adjustments, transfers, write-offs, physical counts) live in `warehouse/corrections.rs`.
use crate::{
    error::{AppError, AppResult},
    auth::AuthUser,
    routes::ref_data::bh::{self, local_date, Arg, Q},
    state::AppState,
    util, validate,
};
use axum::{extract::State, response::Response, routing::{get, post, put}, Json, Router};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

mod corrections;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/stock", get(stock_list))
        .route("/stock/summary", get(summary))
        .route("/stock/item", get(item))
        .route("/ledger", get(ledger))
        .route("/expiring", get(expiring))
        .route("/adjustments", post(corrections::adjust))
        .route("/transfers", post(corrections::transfer))
        .route("/write-offs", get(corrections::wo_list).post(corrections::wo_create))
        .route("/write-offs/{id}", get(corrections::wo_get))
        .route("/counts", get(corrections::count_list).post(corrections::count_create))
        .route("/counts/{id}", get(corrections::count_get))
        .route("/counts/{id}/lines", put(corrections::count_lines))
        .route("/counts/{id}/post", post(corrections::count_post))
        .route("/counts/{id}/cancel", post(corrections::count_cancel))
}

fn item_type_arg(v: &Option<String>) -> AppResult<Option<String>> {
    match v.as_deref() {
        None | Some("") => Ok(None),
        Some("RAW") => Ok(Some("RAW".into())),
        Some("PRODUCT") => Ok(Some("PRODUCT".into())),
        _ => Err(AppError::field("item_type", "must be RAW or PRODUCT")),
    }
}
fn zone_arg(v: &Option<String>) -> AppResult<Option<String>> {
    match v.as_deref() { None | Some("") => Ok(None), Some(z) => Ok(Some(bh::zone("zone", z)?.to_string())) }
}

#[derive(Deserialize, Default)]
struct StockQ { zone: Option<String>, item_type: Option<String>, q: Option<String>, low: Option<String>, format: Option<String>, location_id: Option<String> }

const STOCK_BASE: &str = "
SELECT jsonb_build_object('item_type', x.item_type, 'item_id', x.item_id, 'name', x.name, 'code', x.code, 'unit', x.unit, 'zone', x.zone,
         'location_id', x.location_id, 'location_name', lo.name,
         'qty', x.qty::text, 'lots', x.lots, 'kind', x.kind, 'crates_per_pallet', x.cpp,
         'pallets', CASE WHEN x.cpp IS NOT NULL THEN floor(x.qty / x.cpp)::bigint END,
         'loose_crates', CASE WHEN x.cpp IS NOT NULL THEN (x.qty - floor(x.qty / x.cpp) * x.cpp)::text END,
         'min_stock', x.min_q::text) AS j, x.name AS nm, lo.name AS lname, x.zone AS z, x.item_id AS iid
FROM (
  SELECT 'RAW' AS item_type, m.id AS item_id, m.name, m.code, m.stock_unit AS unit, sb.zone, sb.location_id, SUM(sb.qty) AS qty, count(*) AS lots,
         NULL::text AS kind, NULL::int AS cpp, m.min_stock AS min_q, SUM(SUM(sb.qty)) OVER (PARTITION BY m.id, sb.zone) AS tot
  FROM stock_balances sb JOIN raw_materials m ON m.id = sb.item_id
  WHERE sb.item_type = 'RAW' AND sb.qty > 0 AND sb.location_id = ANY($5) GROUP BY m.id, m.name, m.code, m.stock_unit, m.min_stock, sb.zone, sb.location_id
  UNION ALL
  SELECT 'PRODUCT', p.id, p.name, p.code, 'crates', sb.zone, sb.location_id, SUM(sb.qty), count(*), p.kind, p.crates_per_pallet, p.min_stock_crates, SUM(SUM(sb.qty)) OVER (PARTITION BY p.id, sb.zone)
  FROM stock_balances sb JOIN products p ON p.id = sb.item_id
  WHERE sb.item_type = 'PRODUCT' AND sb.qty > 0 AND sb.location_id = ANY($5) GROUP BY p.id, p.name, p.code, p.kind, p.crates_per_pallet, p.min_stock_crates, sb.zone, sb.location_id
  UNION ALL
  SELECT 'RAW', m.id, m.name, m.code, m.stock_unit, 'RM_STORE', ($5::uuid[])[1], 0, 0, NULL, NULL, m.min_stock, 0 FROM raw_materials m
  WHERE $4::bool AND m.is_active AND m.min_stock > 0 AND NOT EXISTS (SELECT 1 FROM stock_balances s WHERE s.item_type='RAW' AND s.item_id=m.id AND s.zone='RM_STORE' AND s.qty > 0 AND s.location_id = ANY($5))
  UNION ALL
  SELECT 'PRODUCT', p.id, p.name, p.code, 'crates', 'FG_READY', ($5::uuid[])[1], 0, 0, p.kind, p.crates_per_pallet, p.min_stock_crates, 0 FROM products p
  WHERE $4::bool AND p.is_active AND p.min_stock_crates > 0 AND NOT EXISTS (SELECT 1 FROM stock_balances s WHERE s.item_type='PRODUCT' AND s.item_id=p.id AND s.zone='FG_READY' AND s.qty > 0 AND s.location_id = ANY($5))
) x JOIN locations lo ON lo.id = x.location_id
WHERE ($1::text IS NULL OR x.zone = $1) AND ($2::text IS NULL OR x.item_type = $2) AND ($3::text IS NULL OR x.name ILIKE $3 OR x.code ILIKE $3)
  AND (NOT $4::bool OR (x.zone = CASE WHEN x.item_type = 'RAW' THEN 'RM_STORE' ELSE 'FG_READY' END AND x.tot < x.min_q))";

async fn stock_list(State(s): State<AppState>, user: AuthUser, Q(q): Q<StockQ>) -> AppResult<Response> {
    let csv = bh::is_csv(&q.format);
    let locs = { let mut c = s.db.acquire().await?; bh::read_locations(&mut c, &user, &q.location_id).await? };
    if locs.is_empty() { return Ok(bh::json_resp(Value::Array(vec![]))); }
    let args = [Arg::T(zone_arg(&q.zone)?), Arg::T(item_type_arg(&q.item_type)?), Arg::like(&q.q), Arg::B(matches!(q.low.as_deref(), Some("1") | Some("true"))), Arg::UA(locs)];
    let limit = if csv { bh::CSV_MAX_ROWS } else { 500 };
    let v = bh::paged(&s.db, STOCK_BASE, "nm, lname, z, iid", &args, limit, 0).await?;
    if csv {
        return Ok(bh::csv_response("stock.csv", &[("Godown", "location_name"), ("Type", "item_type"), ("Code", "code"), ("Name", "name"), ("Unit", "unit"), ("Zone", "zone"),
            ("Quantity", "qty"), ("Lots", "lots"), ("Pallets", "pallets"), ("Loose crates", "loose_crates")], bh::items_of(&v)));
    }
    Ok(bh::json_resp(v.get("items").cloned().unwrap_or(Value::Array(vec![]))))
}

#[derive(Deserialize, Default)]
struct LocQ { location_id: Option<String> }

async fn summary(State(s): State<AppState>, user: AuthUser, Q(q): Q<LocQ>) -> AppResult<Json<Value>> {
    let locs = { let mut c = s.db.acquire().await?; bh::read_locations(&mut c, &user, &q.location_id).await? };
    // by-location breakdown always covers every godown the caller may use (so the UI can show a godown picker with numbers)
    let allowed = { let mut c = s.db.acquire().await?; bh::allowed_locations(&mut c, &user).await? };
    let a = [Arg::UA(locs)];
    let zones = bh::jval(&s.db,
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('zone', z.zone, 'items', COALESCE(t.items,0), 'qty_rows', COALESCE(t.rows,0)) ORDER BY z.ord), '[]'::jsonb)
         FROM (VALUES ('RM_STORE',1),('BLOW_FLOOR',2),('QC_QUARANTINE',3),('FG_READY',4),('SCRAP_YARD',5),('JAR_EMPTY',6),('JAR_CLEAN',7)) z(zone, ord)
         LEFT JOIN (SELECT zone, count(DISTINCT (item_type, item_id)) AS items, count(*) AS rows FROM stock_balances WHERE qty > 0 AND location_id = ANY($1) GROUP BY zone) t ON t.zone = z.zone", &a).await?;
    let products = bh::jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY nm), '[]'::jsonb) FROM (
           SELECT jsonb_build_object('product_id', p.id, 'name', p.name, 'kind', p.kind, 'crates_per_pallet', p.crates_per_pallet,
                  'quarantine', COALESCE(SUM(sb.qty) FILTER (WHERE sb.zone='QC_QUARANTINE'),0)::text,
                  'fg_ready', COALESCE(SUM(sb.qty) FILTER (WHERE sb.zone='FG_READY'),0)::text,
                  'fg_ready_pallets', CASE WHEN p.crates_per_pallet IS NOT NULL THEN floor(COALESCE(SUM(sb.qty) FILTER (WHERE sb.zone='FG_READY'),0) / p.crates_per_pallet)::bigint END,
                  'fg_ready_loose_crates', CASE WHEN p.crates_per_pallet IS NOT NULL THEN (COALESCE(SUM(sb.qty) FILTER (WHERE sb.zone='FG_READY'),0)
                        - floor(COALESCE(SUM(sb.qty) FILTER (WHERE sb.zone='FG_READY'),0) / p.crates_per_pallet) * p.crates_per_pallet)::text END,
                  'scrap', COALESCE(SUM(sb.qty) FILTER (WHERE sb.zone='SCRAP_YARD'),0)::text,
                  'jar_empty', COALESCE(SUM(sb.qty) FILTER (WHERE sb.zone='JAR_EMPTY'),0)::text,
                  'jar_clean', COALESCE(SUM(sb.qty) FILTER (WHERE sb.zone='JAR_CLEAN'),0)::text) AS j, p.name AS nm
           FROM products p LEFT JOIN stock_balances sb ON sb.item_type='PRODUCT' AND sb.item_id = p.id AND sb.qty > 0 AND sb.location_id = ANY($1)
           WHERE p.is_active GROUP BY p.id) t", &a).await?;
    let by_location = bh::jval(&s.db,
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('location_id', l.id, 'code', l.code, 'name', l.name, 'kind', l.kind, 'is_production_site', l.is_production_site,
                'raw_items', COALESCE(r.items, 0), 'fg_ready_crates', COALESCE(r.fg, 0)::text, 'quarantine_crates', COALESCE(r.q, 0)::text, 'balance_rows', COALESCE(r.rows, 0)) ORDER BY l.name), '[]'::jsonb)
         FROM locations l LEFT JOIN (
            SELECT location_id, count(DISTINCT item_id) FILTER (WHERE item_type='RAW' AND zone='RM_STORE') AS items,
                   SUM(qty) FILTER (WHERE item_type='PRODUCT' AND zone='FG_READY') AS fg, SUM(qty) FILTER (WHERE item_type='PRODUCT' AND zone='QC_QUARANTINE') AS q, count(*) AS rows
            FROM stock_balances WHERE qty > 0 GROUP BY location_id) r ON r.location_id = l.id
         WHERE l.id = ANY($1)", &[Arg::UA(allowed.clone())]).await?;
    let transit = bh::jval(&s.db,
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('id', t.id, 'tf_no', t.tf_no, 'from_location_id', t.from_location_id, 'to_location_id', t.to_location_id, 'created_at', t.created_at) ORDER BY t.created_at), '[]'::jsonb)
         FROM godown_transfers t WHERE t.status = 'IN_TRANSIT' AND (t.from_location_id = ANY($1) OR t.to_location_id = ANY($1))", &[Arg::UA(allowed)]).await?;
    Ok(Json(serde_json::json!({ "zones": zones, "products": products, "by_location": by_location, "in_transit": transit })))
}

#[derive(Deserialize, Default)]
struct ItemQ { item_type: Option<String>, item_id: Option<Uuid>, location_id: Option<String> }

async fn item(State(s): State<AppState>, user: AuthUser, Q(q): Q<ItemQ>) -> AppResult<Json<Value>> {
    let it = item_type_arg(&q.item_type)?.ok_or_else(|| AppError::field("item_type", "is required"))?;
    let id = q.item_id.ok_or_else(|| AppError::field("item_id", "is required"))?;
    let locs = { let mut c = s.db.acquire().await?; bh::read_locations(&mut c, &user, &q.location_id).await? };
    let v = bh::jval(&s.db,
        "SELECT jsonb_build_object('item_type', $1::text, 'item_id', $2::uuid, 'name', h.name, 'code', h.code, 'unit', h.unit,
            'total', COALESCE((SELECT SUM(qty) FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone <> 'SCRAP_YARD' AND location_id = ANY($3)),0)::text,
            'lots', COALESCE((SELECT jsonb_agg(jsonb_build_object('location_id', sb.location_id, 'location_name', lo.name, 'zone', sb.zone, 'lot_id', sb.lot_id, 'qty', sb.qty::text, 'expiry_date', sb.expiry_date,
                    'received_at', sb.received_at, 'lot_label', COALESCE(gl.lot_no, b.batch_no), 'qc_status', b.qc_status,
                    'reserved', CASE WHEN sb.zone='FG_READY' AND b.id IS NOT NULL THEN COALESCE((SELECT SUM(ti.crates) FROM trip_items ti JOIN loading_trips t ON t.id = ti.trip_id
                                    WHERE ti.batch_id = b.id AND t.status = 'LOADING' AND t.location_id = sb.location_id),0) END)
                    ORDER BY sb.expiry_date NULLS LAST, sb.received_at, lo.name, sb.zone, sb.lot_id)
                 FROM stock_balances sb JOIN locations lo ON lo.id = sb.location_id LEFT JOIN grn_lines gl ON $1 = 'RAW' AND gl.id = sb.lot_id LEFT JOIN production_batches b ON $1 = 'PRODUCT' AND b.id = sb.lot_id
                 WHERE sb.item_type=$1 AND sb.item_id=$2 AND sb.qty > 0 AND sb.location_id = ANY($3)), '[]'::jsonb))
         FROM (SELECT name, code, stock_unit AS unit FROM raw_materials WHERE $1 = 'RAW' AND id = $2
               UNION ALL SELECT name, code, 'crates' FROM products WHERE $1 = 'PRODUCT' AND id = $2) h",
        &[Arg::T(Some(it)), Arg::U(Some(id)), Arg::UA(locs)]).await;
    match v { Ok(v) if !v.is_null() => Ok(Json(v)), Ok(_) | Err(AppError::NotFound(_)) => Err(AppError::not_found("item")), Err(e) => Err(e) }
}

#[derive(Deserialize, Default)]
struct LedgerQ { item_type: Option<String>, item_id: Option<Uuid>, zone: Option<String>, txn_type: Option<String>, from: Option<NaiveDate>, to: Option<NaiveDate>,
                 format: Option<String>, location_id: Option<String>, limit: Option<i64>, offset: Option<i64> }

async fn ledger(State(s): State<AppState>, user: AuthUser, Q(q): Q<LedgerQ>) -> AppResult<Response> {
    let txn = match q.txn_type.as_deref() {
        None | Some("") => None,
        Some(t) if t.len() <= 40 && t.chars().all(|c| c.is_ascii_uppercase() || c == '_') => Some(t.to_string()),
        _ => return Err(AppError::field("txn_type", "invalid transaction type")),
    };
    let csv = bh::is_csv(&q.format);
    let pq = validate::PageQuery { limit: q.limit, offset: q.offset };
    let (limit, offset) = if csv { (bh::CSV_MAX_ROWS, 0) } else { (pq.limit(), pq.offset()) };
    let off = s.settings.get().utc_offset_minutes;
    let locs = { let mut c = s.db.acquire().await?; bh::read_locations(&mut c, &user, &q.location_id).await? };
    let args = [Arg::T(item_type_arg(&q.item_type)?), Arg::U(q.item_id), Arg::T(zone_arg(&q.zone)?), Arg::T(txn), Arg::D(q.from), Arg::D(q.to), Arg::I(off), Arg::UA(locs)];
    let v = bh::paged(&s.db,
        concat!("SELECT jsonb_build_object('id', l.id, 'at', l.at, 'txn_group', l.txn_group, 'location_id', l.location_id, 'location_name', lo.name, 'item_type', l.item_type, 'item_id', l.item_id,
                  'item_name', COALESCE(m.name, p.name), 'zone', l.zone, 'lot_id', l.lot_id, 'qty_delta', l.qty_delta::text, 'balance_after', l.balance_after::text,
                  'txn_type', l.txn_type, 'ref_type', l.ref_type, 'ref_id', l.ref_id, 'note', l.note, 'user_name', u.full_name) AS j, l.id AS lid
           FROM stock_ledger l JOIN locations lo ON lo.id = l.location_id LEFT JOIN raw_materials m ON l.item_type = 'RAW' AND m.id = l.item_id LEFT JOIN products p ON l.item_type = 'PRODUCT' AND p.id = l.item_id
           LEFT JOIN users u ON u.id = l.user_id
           WHERE l.location_id = ANY($8) AND ($1::text IS NULL OR l.item_type = $1) AND ($2::uuid IS NULL OR l.item_id = $2) AND ($3::text IS NULL OR l.zone = $3) AND ($4::text IS NULL OR l.txn_type = $4)
             AND ($5::date IS NULL OR ", local_date!("l.at", "$7"), " >= $5) AND ($6::date IS NULL OR ", local_date!("l.at", "$7"), " <= $6)"),
        "lid DESC", &args, limit, offset).await?;
    if csv {
        return Ok(bh::csv_response("ledger.csv", &[("Id", "id"), ("At", "at"), ("Godown", "location_name"), ("Item", "item_name"), ("Type", "item_type"), ("Zone", "zone"), ("Lot", "lot_id"),
            ("Change", "qty_delta"), ("Balance after", "balance_after"), ("Transaction", "txn_type"), ("Reference", "ref_type"), ("Note", "note"), ("User", "user_name")], bh::items_of(&v)));
    }
    Ok(bh::json_resp(v))
}

#[derive(Deserialize, Default)]
struct ExpQ { days: Option<i64>, location_id: Option<String> }

async fn expiring(State(s): State<AppState>, user: AuthUser, Q(q): Q<ExpQ>) -> AppResult<Json<Value>> {
    let days = q.days.unwrap_or(60).clamp(0, 730);
    let mut c = s.db.acquire().await?;
    let locs = bh::read_locations(&mut c, &user, &q.location_id).await?;
    let today = util::plant_today(&mut c).await?;
    let v = bh::jval(&mut *c,
        "SELECT COALESCE(jsonb_agg(j ORDER BY ed, nm), '[]'::jsonb) FROM (
           SELECT jsonb_build_object('item_type', sb.item_type, 'item_id', sb.item_id, 'name', COALESCE(m.name, p.name), 'location_id', sb.location_id, 'location_name', lo.name,
                    'zone', sb.zone, 'lot_id', sb.lot_id,
                    'lot_label', COALESCE(gl.lot_no, b.batch_no), 'qty', sb.qty::text, 'expiry_date', sb.expiry_date, 'days_left', (sb.expiry_date - $1::date)) AS j,
                  sb.expiry_date AS ed, COALESCE(m.name, p.name) AS nm
           FROM stock_balances sb JOIN locations lo ON lo.id = sb.location_id LEFT JOIN raw_materials m ON sb.item_type='RAW' AND m.id = sb.item_id LEFT JOIN products p ON sb.item_type='PRODUCT' AND p.id = sb.item_id
           LEFT JOIN grn_lines gl ON sb.item_type='RAW' AND gl.id = sb.lot_id LEFT JOIN production_batches b ON sb.item_type='PRODUCT' AND b.id = sb.lot_id
           WHERE sb.qty > 0 AND sb.zone <> 'SCRAP_YARD' AND sb.expiry_date IS NOT NULL AND sb.expiry_date <= $1::date + $2::int AND sb.location_id = ANY($3)
           ORDER BY sb.expiry_date LIMIT 500) t",
        &[Arg::D(Some(today)), Arg::I(days), Arg::UA(locs)]).await?;
    Ok(Json(v))
}
