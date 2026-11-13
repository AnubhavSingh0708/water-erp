//! Dispatch notes: walk-in / direct sale of finished goods (quantities only), reservation-aware, voidable within the window.
use super::common::*;
use super::trips::id1;
use crate::{
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    state::AppState,
    stock, validate,
};
use axum::{extract::State, http::StatusCode, response::Response, Json};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DLine {
    product_id: Option<Uuid>,
    batch_id: Option<Uuid>,
    #[serde(deserialize_with = "de_i64")]
    crates: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewDispatch {
    customer_id: Option<Uuid>,
    party_name: Option<String>,
    vehicle_no: Option<String>,
    note: Option<String>,
    lines: Vec<DLine>,
    /// accepted for UI symmetry with the other stock documents; a dispatch has no confirm-gated cases
    #[serde(default)]
    #[allow(dead_code)]
    confirm: bool,
}

#[derive(FromRow, Serialize)]
struct DnRow {
    id: Uuid, dn_no: String, status: String, customer_id: Option<Uuid>, customer_name: Option<String>, customer_code: Option<String>,
    party_name: String, vehicle_no: String, note: String, void_reason: Option<String>, voided_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>, created_by_name: Option<String>, total_crates: i64,
}
const DN_SELECT: &str =
    "SELECT d.id, d.dn_no, d.status, d.customer_id, c.name AS customer_name, c.code AS customer_code, d.party_name, d.vehicle_no, d.note,
            d.void_reason, d.voided_at, d.created_at, u.full_name AS created_by_name,
            COALESCE((SELECT SUM(l.crates) FROM dispatch_lines l WHERE l.dn_id = d.id), 0)::bigint AS total_crates
     FROM dispatch_notes d LEFT JOIN customers c ON c.id = d.customer_id LEFT JOIN users u ON u.id = d.created_by";

#[derive(FromRow, Serialize)]
struct DlRow {
    id: Uuid, product_id: Uuid, product_name: String, units_per_crate: i32, crates_per_pallet: Option<i32>, batch_id: Uuid, batch_no: String,
    expiry_date: NaiveDate, crates: i32, pallets: Option<i32>, loose_crates: Option<i32>,
}

async fn dn_json(conn: &mut sqlx::PgConnection, id: Uuid, plant: &str) -> AppResult<Value> {
    let d = sqlx::query_as::<_, DnRow>(&format!("{DN_SELECT} WHERE d.id = $1")).bind(id).fetch_optional(&mut *conn).await?
        .ok_or_else(|| AppError::not_found("Dispatch note"))?;
    let lines = sqlx::query_as::<_, DlRow>(
        "SELECT l.id, l.product_id, p.name AS product_name, p.units_per_crate, p.crates_per_pallet, l.batch_id, b.batch_no, b.expiry_date, l.crates,
                CASE WHEN p.crates_per_pallet IS NULL THEN NULL ELSE l.crates / p.crates_per_pallet END AS pallets,
                CASE WHEN p.crates_per_pallet IS NULL THEN NULL ELSE l.crates % p.crates_per_pallet END AS loose_crates
         FROM dispatch_lines l JOIN products p ON p.id = l.product_id JOIN production_batches b ON b.id = l.batch_id
         WHERE l.dn_id = $1 ORDER BY b.expiry_date, b.batch_no, l.id")
        .bind(id).fetch_all(&mut *conn).await?;
    let bottles: i64 = lines.iter().map(|l| l.crates as i64 * l.units_per_crate as i64).sum();
    let mut v = serde_json::to_value(&d).unwrap_or(Value::Null);
    v["plant_name"] = json!(plant);
    v["totals"] = json!({ "crates": d.total_crates, "bottles": bottles });
    v["lines"] = serde_json::to_value(&lines).unwrap_or(Value::Null);
    Ok(v)
}

pub async fn create(State(st): State<AppState>, user: AuthUser, Q(dq): Q<DryQ>, J(b): J<NewDispatch>) -> AppResult<Response> {
    let dry = dq.on();
    validate::max_items("lines", b.lines.len(), validate::MAX_LINES)?;
    let party = validate::text("party_name", b.party_name.as_deref().unwrap_or(""), 120)?;
    let vehicle_no = validate::text("vehicle_no", b.vehicle_no.as_deref().unwrap_or(""), 40)?;
    let note = validate::text("note", b.note.as_deref().unwrap_or(""), 500)?;
    if b.customer_id.is_none() && party.is_empty() { return Err(AppError::field("party_name", "give a customer or a walk-in party name")); }
    for (i, l) in b.lines.iter().enumerate() {
        if l.product_id.is_some() == l.batch_id.is_some() { return Err(AppError::field(&format!("lines[{i}]"), "give exactly one of product_id or batch_id")); }
        validate::count(&format!("lines[{i}].crates"), l.crates)?;
    }
    let mut tx = st.db.begin().await?;
    let mut fx = Fx::default();
    if let Some(c) = b.customer_id {
        let ok: Option<bool> = sqlx::query_scalar("SELECT is_active FROM customers WHERE id = $1").bind(c).fetch_optional(&mut *tx).await?;
        if ok != Some(true) { return Err(AppError::field("customer_id", "unknown or inactive customer")); }
    }
    // take the batch locks in the canonical order up front (explicit batches first, then product candidates by product id)
    let mut explicit: Vec<Uuid> = b.lines.iter().filter_map(|l| l.batch_id).collect();
    explicit.sort(); explicit.dedup();
    lock_batches(&mut tx, &explicit).await?;
    let mut products: Vec<Uuid> = b.lines.iter().filter_map(|l| l.product_id).collect();
    products.sort(); products.dedup();
    for p in &products { prelock_product_batches(&mut tx, *p).await?; }

    // the real number is assigned after every line succeeded (no gaps in the series for failed attempts)
    let mut dn_no = format!("PENDING-{}", Uuid::new_v4());
    let dn_id: Uuid = sqlx::query_scalar(
        "INSERT INTO dispatch_notes (dn_no, customer_id, party_name, vehicle_no, note, created_by) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id")
        .bind(&dn_no).bind(b.customer_id).bind(&party).bind(&vehicle_no).bind(&note).bind(user.id).fetch_one(&mut *tx).await?;

    let mut total = 0i64;
    for (i, l) in b.lines.iter().enumerate() {
        let allocs: Vec<(Uuid, i64)> = if let Some(pid) = l.product_id {
            check_kind(&mut tx, pid, i).await?;
            // lines of the same product see the stock already taken by earlier lines (they are posted immediately)
            stock::allocate_free_fefo(&mut tx, pid, l.crates).await.map_err(whole_crates_error)?
        } else {
            vec![(l.batch_id.unwrap_or_default(), l.crates)]
        };
        for (bid, n) in allocs {
            let bt = lock_batches(&mut tx, &[bid]).await?.into_iter().next().ok_or_else(|| AppError::not_found("Batch"))?;
            if l.batch_id.is_some() { check_kind(&mut tx, bt.product_id, i).await?; }
            if bt.qc_status != "APPROVED" { return Err(qc_locked(&bt)); }
            let av = lot_avail(&mut tx, bt.product_id, bt.id, "FG_READY").await?;
            if Decimal::from(n) > av.free { return Err(insufficient(&bt, av.free, n)); }
            sqlx::query("INSERT INTO dispatch_lines (dn_id, product_id, batch_id, crates) VALUES ($1,$2,$3,$4)")
                .bind(dn_id).bind(bt.product_id).bind(bt.id).bind(n as i32).execute(&mut *tx).await?;
            let mut labels = Labels::new();
            labels.insert((bt.product_id, bt.id), format!("{} · {}", bt.product_name, bt.batch_no));
            post_moves(&mut tx, vec![product_move(bt.product_id, "FG_READY", bt.id, -n, "DISPATCH").reference("DISPATCH", dn_id).by(Some(user.id)).note("dispatch note".to_string())], &labels, &mut fx).await?;
            total += n;
        }
    }
    dn_no = doc_no(&mut tx, dry, "dn_seq", "DN").await?;
    if !dry { sqlx::query("UPDATE dispatch_notes SET dn_no = $2 WHERE id = $1").bind(dn_id).bind(&dn_no).execute(&mut *tx).await?; }
    let out = dn_json(&mut tx, dn_id, &st.settings.get().plant_name).await?;
    audit::log(&mut *tx, Some(&user), "DISPATCH_NOTE", "dispatch_note", Some(dn_id.to_string()),
        json!({ "dn_no": dn_no, "customer_id": b.customer_id, "party_name": party, "crates": total })).await;
    conclude(tx, dry, fx, StatusCode::CREATED, out).await
}

async fn check_kind(tx: &mut sqlx::PgConnection, product: Uuid, line: usize) -> AppResult<()> {
    let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM products WHERE id = $1").bind(product).fetch_optional(&mut *tx).await?;
    match kind.as_deref() {
        None => Err(AppError::field(&format!("lines[{line}].product_id"), "unknown product")),
        Some("RETURNABLE") => Err(AppError::field(&format!("lines[{line}].product_id"),
            "returnable containers must be issued through Jars (customer deposit tracking), not a dispatch note")),
        _ => Ok(()),
    }
}

#[derive(Deserialize, Default)]
pub struct DnListQ { status: Option<String>, q: Option<String>, from: Option<NaiveDate>, to: Option<NaiveDate>, limit: Option<i64>, offset: Option<i64> }

pub async fn list(State(st): State<AppState>, Q(q): Q<DnListQ>) -> AppResult<Json<Value>> {
    let status = match q.status.as_deref().map(str::trim) {
        None | Some("") | Some("ALL") => None,
        Some(s) if s == "ISSUED" || s == "VOID" => Some(s.to_string()),
        Some(_) => return Err(AppError::field("status", "must be ISSUED, VOID or ALL")),
    };
    let pat = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => { validate::text("q", s, 100)?; Some(validate::like_pattern(s)) }
        None => None,
    };
    let off = st.settings.get().utc_offset_minutes as i32;
    let page = validate::PageQuery { limit: q.limit, offset: q.offset };
    let filter = "WHERE ($1::text IS NULL OR d.status = $1)
        AND ($2::text IS NULL OR d.dn_no ILIKE $2 OR d.party_name ILIKE $2 OR COALESCE(c.name,'') ILIKE $2 OR d.vehicle_no ILIKE $2)
        AND ($3::date IS NULL OR ((d.created_at AT TIME ZONE 'UTC') + make_interval(mins => $5))::date >= $3)
        AND ($4::date IS NULL OR ((d.created_at AT TIME ZONE 'UTC') + make_interval(mins => $5))::date <= $4)";
    let rows = sqlx::query_as::<_, DnRow>(&format!("{DN_SELECT} {filter} ORDER BY d.created_at DESC, d.id ASC LIMIT $6 OFFSET $7"))
        .bind(&status).bind(&pat).bind(q.from).bind(q.to).bind(off).bind(page.limit()).bind(page.offset()).fetch_all(&st.db).await?;
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM dispatch_notes d LEFT JOIN customers c ON c.id = d.customer_id {filter}"))
        .bind(&status).bind(&pat).bind(q.from).bind(q.to).bind(off).fetch_one(&st.db).await?;
    Ok(Json(json!({ "items": rows, "total": total })))
}

pub async fn get(State(st): State<AppState>, ids: Ids) -> AppResult<Json<Value>> {
    let mut conn = st.db.acquire().await?;
    Ok(Json(dn_json(&mut conn, id1(&ids)?, &st.settings.get().plant_name).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoidBody {
    reason: String,
    #[serde(default)]
    #[allow(dead_code)]
    confirm: bool,
}

pub async fn void(State(st): State<AppState>, user: AuthUser, ids: Ids, Q(dq): Q<DryQ>, J(b): J<VoidBody>) -> AppResult<Response> {
    let dry = dq.on();
    let reason = validate::required_text("reason", &b.reason, 300)?;
    if reason.chars().count() < 5 { return Err(AppError::field("reason", "must be at least 5 characters")); }
    let id = id1(&ids)?;
    let mut tx = st.db.begin().await?;
    let mut fx = Fx::default();
    let head: Option<(String, String, DateTime<Utc>)> = sqlx::query_as("SELECT dn_no, status, created_at FROM dispatch_notes WHERE id = $1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?;
    let (dn_no, status, created) = head.ok_or_else(|| AppError::not_found("Dispatch note"))?;
    if status != "ISSUED" { return Err(invalid_state(format!("Dispatch note {dn_no} is already {status}"), &status)); }
    if !user.is_admin {
        let window = st.settings.get().void_window_hours;
        if Utc::now() - created > Duration::hours(window) {
            return Err(AppError::forbidden(format!("The {window}-hour void window for this dispatch note has passed. Ask an Administrator.")));
        }
    }
    let lines: Vec<(Uuid, Uuid, i32)> = sqlx::query_as("SELECT product_id, batch_id, crates FROM dispatch_lines WHERE dn_id = $1").bind(id).fetch_all(&mut *tx).await?;
    let mut sums: BTreeMap<Uuid, (Uuid, i64)> = BTreeMap::new();
    for (p, bt, c) in &lines { sums.entry(*bt).or_insert((*p, 0)).1 += *c as i64; }
    let ids_v: Vec<Uuid> = sums.keys().cloned().collect();
    let batches = lock_batches(&mut tx, &ids_v).await?;
    let mut labels = Labels::new();
    let mut moves = vec![];
    for bt in &batches {
        let n = sums[&bt.id].1;
        labels.insert((bt.product_id, bt.id), format!("{} · {}", bt.product_name, bt.batch_no));
        moves.push(product_move(bt.product_id, restock_zone(&bt.qc_status), bt.id, n, "DISPATCH_VOID")
            .reference("DISPATCH", id).by(Some(user.id)).expiry(Some(bt.expiry_date)).note(format!("void {dn_no}: {reason}")));
    }
    post_moves(&mut tx, moves, &labels, &mut fx).await?;
    sqlx::query("UPDATE dispatch_notes SET status = 'VOID', void_reason = $2, voided_by = $3, voided_at = now() WHERE id = $1")
        .bind(id).bind(&reason).bind(user.id).execute(&mut *tx).await?;
    let out = dn_json(&mut tx, id, &st.settings.get().plant_name).await?;
    audit::log(&mut *tx, Some(&user), "DISPATCH_VOID", "dispatch_note", Some(id.to_string()), json!({ "dn_no": dn_no, "reason": reason })).await;
    conclude(tx, dry, fx, StatusCode::OK, out).await
}
