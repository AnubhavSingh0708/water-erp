//! `/api/raw-materials/*` – raw-material stock views, purchase orders, reorder recommendations (OWNER B, page `raw-materials`).
//! GRNs, GRN void and supplier returns live in `materials/grn.rs`.
use crate::{
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    routes::ref_data::bh::{self, created, ok, Arg, Id, J, Q, D},
    state::AppState,
    util, validate,
};
use axum::{
    extract::State,
    response::Response,
    routing::{get, post},
    Router,
};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

mod grn;

pub const CATEGORIES: &[&str] = &["PREFORM", "CAP", "LABEL", "SLEEVE", "SHRINK_FILM", "CARTON", "MINERAL", "CHEMICAL", "EMPTY_BOTTLE", "OTHER"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list))
        .route("/suppliers", get(suppliers))
        .route("/purchase-orders", get(po_list).post(po_create))
        .route("/purchase-orders/{id}", get(po_get))
        .route("/purchase-orders/{id}/cancel", post(po_cancel))
        .route("/grns", get(grn::list).post(grn::create))
        .route("/grns/{id}", get(grn::get))
        .route("/grns/{id}/void", post(grn::void))
        .route("/supplier-returns", get(grn::rtv_list).post(grn::rtv_create))
        .route("/supplier-returns/{id}", get(grn::rtv_get))
        .route("/reorder-recommendations", get(rec_list))
        .route("/reorder-recommendations/run", post(rec_run))
        .route("/reorder-recommendations/{id}/dismiss", post(rec_dismiss))
        .route("/reorder-recommendations/{id}/create-po", post(rec_create_po))
        .route("/{id}", get(detail))
}

// ------------------------------------------------------------------------------------------------------------------
// Raw materials
// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize, Default)]
struct ListQ { category: Option<String>, q: Option<String>, low: Option<String>, format: Option<String>, limit: Option<i64>, offset: Option<i64> }

const MAT_BASE: &str = "
WITH burn AS (
  SELECT item_id, -SUM(qty_delta) AS used FROM stock_ledger
  WHERE item_type='RAW' AND txn_type IN ('BLOW_ISSUE','BATCH_CONSUME','BATCH_WASTE') AND at >= now() - make_interval(days => $4::int) GROUP BY item_id),
st AS (
  SELECT item_id, SUM(qty) FILTER (WHERE zone='RM_STORE') AS rm, SUM(qty) FILTER (WHERE zone <> 'SCRAP_YARD') AS total
  FROM stock_balances WHERE item_type='RAW' GROUP BY item_id)
SELECT to_jsonb(m) || jsonb_build_object('stock_per_purchase_unit', m.stock_per_purchase_unit::text, 'unit_weight_g', m.unit_weight_g::text,
         'min_stock', m.min_stock::text, 'supplier_name', sp.name,
         'stock_rm_store', COALESCE(st.rm,0)::text, 'stock_total', COALESCE(st.total,0)::text,
         'status', CASE WHEN COALESCE(st.rm,0) <= 0 THEN 'OUT' WHEN COALESCE(st.rm,0) < m.min_stock THEN 'LOW' ELSE 'OK' END,
         'avg_daily_burn', round(COALESCE(b.used,0) / $4::numeric, 4)::text,
         'days_cover', CASE WHEN COALESCE(b.used,0) > 0 THEN round(COALESCE(st.rm,0) / (b.used / $4::numeric), 1)::text END,
         'open_reorder', EXISTS(SELECT 1 FROM reorder_recommendations r WHERE r.raw_material_id = m.id AND r.status = 'OPEN')) AS j,
       m.name AS name, m.code AS code
FROM raw_materials m LEFT JOIN st ON st.item_id = m.id LEFT JOIN burn b ON b.item_id = m.id LEFT JOIN suppliers sp ON sp.id = m.default_supplier_id
WHERE m.is_active AND ($1::text IS NULL OR m.category = $1) AND ($2::text IS NULL OR m.name ILIKE $2 OR m.code ILIKE $2)
  AND (NOT $3::bool OR COALESCE(st.rm,0) <= 0 OR COALESCE(st.rm,0) < m.min_stock)";

async fn list(State(s): State<AppState>, Q(q): Q<ListQ>) -> AppResult<Response> {
    if let Some(c) = &q.category { if !CATEGORIES.contains(&c.as_str()) { return Err(AppError::field("category", "unknown category")); } }
    let csv = bh::is_csv(&q.format);
    let (limit, offset) = if csv { (bh::CSV_MAX_ROWS, 0) } else { (q.limit.unwrap_or(500).clamp(1, 500), q.offset.unwrap_or(0).max(0)) };
    let args = [Arg::t(&q.category), Arg::like(&q.q), Arg::B(matches!(q.low.as_deref(), Some("1") | Some("true"))),
                Arg::I(s.settings.get().burn_rate_lookback_days.max(1))];
    let v = bh::paged(&s.db, MAT_BASE, "name, code", &args, limit, offset).await?;
    if csv {
        return Ok(bh::csv_response("raw-materials.csv", &[
            ("Code", "code"), ("Name", "name"), ("Category", "category"), ("Stock unit", "stock_unit"), ("Purchase unit", "purchase_unit"),
            ("Stock per purchase unit", "stock_per_purchase_unit"), ("Min stock", "min_stock"), ("In RM store", "stock_rm_store"),
            ("Total stock", "stock_total"), ("Status", "status"), ("Avg daily burn", "avg_daily_burn"), ("Days cover", "days_cover"),
        ], bh::items_of(&v)));
    }
    // contract: bare array for non-paginated lists (bounded to 500 rows)
    Ok(bh::json_resp(v.get("items").cloned().unwrap_or(json!([]))))
}

async fn suppliers(State(s): State<AppState>) -> AppResult<axum::Json<Value>> {
    Ok(axum::Json(bh::jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY n), '[]'::jsonb) FROM (SELECT to_jsonb(x) AS j, x.name AS n FROM suppliers x WHERE x.is_active ORDER BY x.name LIMIT 500) t", &[]).await?))
}

async fn detail(State(s): State<AppState>, Id(id): Id) -> AppResult<axum::Json<Value>> {
    let v = bh::jval(&s.db,
        "SELECT to_jsonb(m) || jsonb_build_object('stock_per_purchase_unit', m.stock_per_purchase_unit::text, 'unit_weight_g', m.unit_weight_g::text,
            'min_stock', m.min_stock::text,
            'stock_rm_store', COALESCE((SELECT SUM(qty) FROM stock_balances WHERE item_type='RAW' AND item_id=m.id AND zone='RM_STORE'),0)::text,
            'stock_total', COALESCE((SELECT SUM(qty) FROM stock_balances WHERE item_type='RAW' AND item_id=m.id AND zone <> 'SCRAP_YARD'),0)::text,
            'lots', COALESCE((SELECT jsonb_agg(jsonb_build_object('lot_id', sb.lot_id, 'zone', sb.zone, 'qty', sb.qty::text, 'expiry_date', sb.expiry_date,
                        'lot_no', gl.lot_no, 'grn_no', g.grn_no, 'received_at', sb.received_at)
                        ORDER BY sb.expiry_date NULLS LAST, sb.received_at, sb.lot_id)
                      FROM stock_balances sb LEFT JOIN grn_lines gl ON gl.id = sb.lot_id LEFT JOIN grns g ON g.id = gl.grn_id
                      WHERE sb.item_type='RAW' AND sb.item_id=m.id AND sb.qty > 0), '[]'::jsonb),
            'recent_ledger', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', x.id, 'at', x.at, 'zone', x.zone, 'lot_id', x.lot_id, 'qty_delta', x.qty_delta::text,
                        'balance_after', x.balance_after::text, 'txn_type', x.txn_type, 'note', x.note) ORDER BY x.id DESC)
                      FROM (SELECT * FROM stock_ledger WHERE item_type='RAW' AND item_id=m.id ORDER BY id DESC LIMIT 20) x), '[]'::jsonb)
         ) FROM raw_materials m WHERE m.id = $1", &[Arg::U(Some(id))]).await;
    match v { Ok(v) if !v.is_null() => Ok(axum::Json(v)), Ok(_) | Err(AppError::NotFound(_)) => Err(AppError::not_found("raw material")), Err(e) => Err(e) }
}

// ------------------------------------------------------------------------------------------------------------------
// Purchase orders
// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PoLineReq { raw_material_id: Uuid, qty_ordered: D }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PoReq { supplier_id: Uuid, expected_on: Option<NaiveDate>, notes: Option<String>, lines: Vec<PoLineReq> }

pub(crate) async fn po_json(conn: &mut PgConnection, id: Uuid) -> AppResult<Value> {
    let v = bh::jval(&mut *conn,
        "SELECT jsonb_build_object('id', o.id, 'po_no', o.po_no, 'supplier_id', o.supplier_id, 'supplier_name', s.name, 'status', o.status,
            'ordered_on', o.ordered_on, 'expected_on', o.expected_on, 'notes', o.notes, 'created_at', o.created_at,
            'lines', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', l.id, 'raw_material_id', l.raw_material_id, 'name', m.name, 'code', m.code,
                    'purchase_unit', m.purchase_unit, 'stock_unit', m.stock_unit, 'stock_per_purchase_unit', m.stock_per_purchase_unit::text,
                    'qty_ordered', l.qty_ordered::text, 'qty_received', l.qty_received::text) ORDER BY m.name, l.id)
                 FROM po_lines l JOIN raw_materials m ON m.id = l.raw_material_id WHERE l.po_id = o.id), '[]'::jsonb))
         FROM purchase_orders o JOIN suppliers s ON s.id = o.supplier_id WHERE o.id = $1", &[Arg::U(Some(id))]).await;
    match v { Ok(v) if !v.is_null() => Ok(v), Ok(_) | Err(AppError::NotFound(_)) => Err(AppError::not_found("purchase order")), Err(e) => Err(e) }
}

/// Creates the PO header + lines inside the caller's transaction (also used by "create PO from recommendation").
pub(crate) async fn insert_po(tx: &mut PgConnection, user: &AuthUser, supplier_id: Uuid, expected_on: Option<NaiveDate>, notes: &str,
                              lines: &[(Uuid, rust_decimal::Decimal)]) -> AppResult<Uuid> {
    let today = util::plant_today(&mut *tx).await?;
    if let Some(e) = expected_on { if e < today { return Err(AppError::field("expected_on", "must not be in the past")); } }
    let active: Option<bool> = sqlx::query_scalar("SELECT is_active FROM suppliers WHERE id=$1").bind(supplier_id).fetch_optional(&mut *tx).await?;
    if active != Some(true) { return Err(AppError::field("supplier_id", "unknown or inactive supplier")); }
    let ids: Vec<Uuid> = lines.iter().map(|l| l.0).collect();
    let mut uniq = ids.clone();
    uniq.sort(); uniq.dedup();
    if uniq.len() != ids.len() { return Err(AppError::field("lines", "each material may appear only once")); }
    let ok_cnt: i64 = sqlx::query_scalar("SELECT count(*) FROM raw_materials WHERE id = ANY($1) AND is_active").bind(&ids).fetch_one(&mut *tx).await?;
    if ok_cnt as usize != ids.len() { return Err(AppError::field("lines", "unknown or inactive raw material")); }
    let po_no = util::next_no(&mut *tx, "po_seq", "PO").await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO purchase_orders (po_no, supplier_id, ordered_on, expected_on, notes, created_by) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id")
        .bind(&po_no).bind(supplier_id).bind(today).bind(expected_on).bind(notes).bind(user.id).fetch_one(&mut *tx).await?;
    for (rm, q) in lines {
        sqlx::query("INSERT INTO po_lines (po_id, raw_material_id, qty_ordered) VALUES ($1,$2,$3)").bind(id).bind(rm).bind(q).execute(&mut *tx).await?;
    }
    audit::log(&mut *tx, Some(user), "PO_CREATE", "purchase_order", Some(id.to_string()), json!({ "po_no": po_no, "lines": lines.len() })).await;
    Ok(id)
}

async fn po_create(State(s): State<AppState>, user: AuthUser, J(b): J<PoReq>) -> AppResult<(axum::http::StatusCode, axum::Json<Value>)> {
    validate::max_items("lines", b.lines.len(), validate::MAX_LINES)?;
    let notes = bh::opt_text("notes", &b.notes, 500)?;
    let mut lines = vec![];
    for l in &b.lines { validate::qty_positive("qty_ordered", l.qty_ordered.0)?; lines.push((l.raw_material_id, l.qty_ordered.0)); }
    let mut tx = s.db.begin().await?;
    let id = insert_po(&mut tx, &user, b.supplier_id, b.expected_on, &notes, &lines).await?;
    let v = po_json(&mut tx, id).await?;
    tx.commit().await?;
    created(v)
}

#[derive(Deserialize, Default)]
struct PoListQ { status: Option<String> }

async fn po_list(State(s): State<AppState>, Q(q): Q<PoListQ>) -> AppResult<axum::Json<Value>> {
    if let Some(st) = &q.status { if !["OPEN", "PARTIAL", "RECEIVED", "CANCELLED"].contains(&st.as_str()) { return Err(AppError::field("status", "unknown status")); } }
    Ok(axum::Json(bh::jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY ca DESC), '[]'::jsonb) FROM (
           SELECT jsonb_build_object('id', o.id, 'po_no', o.po_no, 'supplier_id', o.supplier_id, 'supplier_name', s.name, 'status', o.status,
                  'ordered_on', o.ordered_on, 'expected_on', o.expected_on, 'created_at', o.created_at,
                  'lines_count', (SELECT count(*) FROM po_lines l WHERE l.po_id = o.id)) AS j, o.created_at AS ca
           FROM purchase_orders o JOIN suppliers s ON s.id = o.supplier_id WHERE ($1::text IS NULL OR o.status = $1)
           ORDER BY o.created_at DESC LIMIT 500) t", &[Arg::t(&q.status)]).await?))
}

async fn po_get(State(s): State<AppState>, Id(id): Id) -> AppResult<axum::Json<Value>> {
    let mut c = s.db.acquire().await?;
    Ok(axum::Json(po_json(&mut c, id).await?))
}

async fn po_cancel(State(s): State<AppState>, user: AuthUser, Id(id): Id) -> AppResult<(axum::http::StatusCode, axum::Json<Value>)> {
    let mut tx = s.db.begin().await?;
    let st: Option<String> = sqlx::query_scalar("SELECT status FROM purchase_orders WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    let st = st.ok_or_else(|| AppError::not_found("purchase order"))?;
    let rows: Vec<rust_decimal::Decimal> = sqlx::query_scalar("SELECT qty_received FROM po_lines WHERE po_id=$1 FOR UPDATE").bind(id).fetch_all(&mut *tx).await?;
    let received: rust_decimal::Decimal = rows.iter().copied().sum();
    if st != "OPEN" || !received.is_zero() {
        return Err(AppError::conflict("INVALID_STATE", "Only an OPEN purchase order with nothing received can be cancelled"));
    }
    sqlx::query("UPDATE purchase_orders SET status='CANCELLED' WHERE id=$1").bind(id).execute(&mut *tx).await?;
    audit::log(&mut *tx, Some(&user), "PO_CANCEL", "purchase_order", Some(id.to_string()), json!({})).await;
    let v = po_json(&mut tx, id).await?;
    tx.commit().await?;
    ok(v)
}

// ------------------------------------------------------------------------------------------------------------------
// Reorder recommendations
// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize, Default)]
struct RecQ { status: Option<String> }

const REC_SQL: &str = "
SELECT COALESCE(jsonb_agg(j ORDER BY ca DESC), '[]'::jsonb) FROM (
  SELECT jsonb_build_object('id', r.id, 'raw_material_id', r.raw_material_id, 'material_name', m.name, 'material_code', m.code, 'category', m.category,
         'status', r.status, 'reason', r.reason, 'current_stock', r.current_stock::text, 'min_stock', r.min_stock::text,
         'avg_daily_burn', r.avg_daily_burn::text, 'days_cover', r.days_cover::text, 'lead_time_days', r.lead_time_days, 'safety_days', r.safety_days,
         'reorder_point', r.reorder_point::text, 'suggested_qty_purchase', r.suggested_qty_purchase::text, 'purchase_unit', m.purchase_unit,
         'stock_unit', m.stock_unit, 'stock_per_purchase_unit', m.stock_per_purchase_unit::text,
         'suggested_supplier_id', r.suggested_supplier_id, 'supplier_name', s.name, 'created_at', r.created_at, 'updated_at', r.updated_at) AS j, r.created_at AS ca
  FROM reorder_recommendations r JOIN raw_materials m ON m.id = r.raw_material_id LEFT JOIN suppliers s ON s.id = r.suggested_supplier_id
  WHERE ($1::text IS NULL OR r.status = $1) ORDER BY r.created_at DESC LIMIT 500) t";

async fn rec_list(State(s): State<AppState>, Q(q): Q<RecQ>) -> AppResult<axum::Json<Value>> {
    let st = match q.status.as_deref() {
        None | Some("OPEN") => Some("OPEN".to_string()),
        Some("ALL") => None,
        Some(x) if ["ORDERED", "DISMISSED", "RESOLVED"].contains(&x) => Some(x.to_string()),
        _ => return Err(AppError::field("status", "unknown status")),
    };
    Ok(axum::Json(bh::jval(&s.db, REC_SQL, &[Arg::T(st)]).await?))
}

async fn rec_run(State(s): State<AppState>, user: AuthUser) -> AppResult<axum::Json<Value>> {
    let v = crate::workers::reorder::run_once(&s).await?;
    audit::log(&s.db, Some(&user), "REORDER_RUN", "reorder_recommendations", None, v.clone()).await;
    Ok(axum::Json(v))
}

async fn rec_dismiss(State(s): State<AppState>, user: AuthUser, Id(id): Id) -> AppResult<axum::Json<Value>> {
    let mut tx = s.db.begin().await?;
    let row = sqlx::query("SELECT status, raw_material_id FROM reorder_recommendations WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?
        .ok_or_else(|| AppError::not_found("recommendation"))?;
    if row.get::<String, _>("status") != "OPEN" { return Err(AppError::conflict("INVALID_STATE", "This recommendation is no longer open")); }
    let rm: Uuid = row.get("raw_material_id");
    sqlx::query("UPDATE reorder_recommendations SET status='DISMISSED', updated_at=now() WHERE id=$1").bind(id).execute(&mut *tx).await?;
    crate::alerts::resolve(&mut *tx, "REORDER_RECOMMENDED", &rm.to_string()).await?;
    audit::log(&mut *tx, Some(&user), "REORDER_DISMISS", "reorder_recommendation", Some(id.to_string()), json!({ "raw_material_id": rm })).await;
    tx.commit().await?;
    Ok(axum::Json(json!({ "id": id, "status": "DISMISSED" })))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CreatePoReq { supplier_id: Option<Uuid> }

async fn rec_create_po(State(s): State<AppState>, user: AuthUser, Id(id): Id, body: axum::body::Bytes) -> AppResult<(axum::http::StatusCode, axum::Json<Value>)> {
    let req: CreatePoReq = if body.iter().all(|b| b.is_ascii_whitespace()) { CreatePoReq::default() }
        else { serde_json::from_slice(&body).map_err(|_| AppError::bad("Invalid request body"))? };
    let mut tx = s.db.begin().await?;
    let row = sqlx::query(
        "SELECT r.status, r.raw_material_id, r.suggested_qty_purchase, r.suggested_supplier_id, m.default_supplier_id
         FROM reorder_recommendations r JOIN raw_materials m ON m.id = r.raw_material_id WHERE r.id=$1 FOR UPDATE OF r")
        .bind(id).fetch_optional(&mut *tx).await?.ok_or_else(|| AppError::not_found("recommendation"))?;
    if row.get::<String, _>("status") != "OPEN" { return Err(AppError::conflict("INVALID_STATE", "This recommendation is no longer open")); }
    let rm: Uuid = row.get("raw_material_id");
    let supplier = req.supplier_id.or(row.get::<Option<Uuid>, _>("suggested_supplier_id")).or(row.get::<Option<Uuid>, _>("default_supplier_id"))
        .ok_or_else(|| AppError::field("supplier_id", "no supplier is set for this material - choose one"))?;
    let qty: rust_decimal::Decimal = row.get("suggested_qty_purchase");
    let po = insert_po(&mut tx, &user, supplier, None, "Created from reorder recommendation", &[(rm, qty)]).await?;
    sqlx::query("UPDATE reorder_recommendations SET status='ORDERED', updated_at=now() WHERE id=$1").bind(id).execute(&mut *tx).await?;
    crate::alerts::resolve(&mut *tx, "REORDER_RECOMMENDED", &rm.to_string()).await?;
    audit::log(&mut *tx, Some(&user), "REORDER_CREATE_PO", "reorder_recommendation", Some(id.to_string()), json!({ "po_id": po })).await;
    let v = po_json(&mut tx, po).await?;
    tx.commit().await?;
    created(v)
}
