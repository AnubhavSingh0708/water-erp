//! Counter returns of finished goods outside trips (`/api/loading/customer-returns`, page `loading`).
//! A customer / walk-in party brings crates back: INTACT crates of an APPROVED, unexpired batch go back into `FG_READY` of the godown
//! (same batch lot, ledger `CUSTOMER_RETURN`, never above what the batch produced), DAMAGED crates go to `SCRAP_YARD`
//! (`CUSTOMER_RETURN_DAMAGED`). Returnable containers are refused (they use the jars flow). Voidable (`CUSTOMER_RETURN_VOID`) only while the
//! intact crates are still free in `FG_READY`. Quantities only. Lock order: batch rows -> stock rows -> customer row (shared).
use super::common::*;
use super::trips::id1;
use crate::{
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    state::AppState,
    stock, util, validate,
};
use axum::{extract::State, http::StatusCode, response::Response, Json};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

const MAX_LINES: usize = 100;
const MAX_CRATES: i64 = 1_000_000;
/// A single batch return of at least this many crates always needs confirmation.
const LARGE_CRATES: i64 = 100;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RLine {
    product_id: Uuid,
    batch_id: Uuid,
    #[serde(default, deserialize_with = "de_i64")] crates_intact: i64,
    #[serde(default, deserialize_with = "de_i64")] crates_damaged: i64,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewReturn {
    /// godown that takes the goods back (default: the caller's only godown, else MAIN)
    location_id: Option<Uuid>,
    customer_id: Option<Uuid>,
    party_name: Option<String>,
    note: Option<String>,
    lines: Vec<RLine>,
    #[serde(default)]
    confirm: bool,
}

#[derive(FromRow, Serialize)]
struct CrRow {
    id: Uuid, cr_no: String, status: String, customer_id: Option<Uuid>, customer_name: Option<String>, customer_code: Option<String>,
    party_name: String, note: String, void_reason: Option<String>, voided_at: Option<DateTime<Utc>>, voided_by_name: Option<String>,
    created_at: DateTime<Utc>, created_by_name: Option<String>, location_id: Uuid, location_name: String, total_intact: i64, total_damaged: i64,
}
const CR_SELECT: &str =
    "SELECT r.id, r.cr_no, r.status, r.customer_id, c.name AS customer_name, c.code AS customer_code, r.party_name, r.note, r.void_reason, r.voided_at,
            vu.full_name AS voided_by_name, r.created_at, u.full_name AS created_by_name, r.location_id, loc.name AS location_name,
            COALESCE((SELECT SUM(l.crates_intact) FROM customer_return_lines l WHERE l.cr_id = r.id), 0)::bigint AS total_intact,
            COALESCE((SELECT SUM(l.crates_damaged) FROM customer_return_lines l WHERE l.cr_id = r.id), 0)::bigint AS total_damaged
     FROM customer_returns r JOIN locations loc ON loc.id = r.location_id LEFT JOIN customers c ON c.id = r.customer_id
     LEFT JOIN users u ON u.id = r.created_by LEFT JOIN users vu ON vu.id = r.voided_by";

#[derive(FromRow, Serialize)]
struct CrLine {
    id: Uuid, product_id: Uuid, product_name: String, units_per_crate: i32, crates_per_pallet: Option<i32>, batch_id: Uuid, batch_no: String,
    expiry_date: NaiveDate, crates_intact: i32, crates_damaged: i32, reason: String,
    pallets_intact: Option<i32>, loose_intact: Option<i32>,
}

async fn cr_json(conn: &mut sqlx::PgConnection, id: Uuid, plant: &str) -> AppResult<Value> {
    let r = sqlx::query_as::<_, CrRow>(&format!("{CR_SELECT} WHERE r.id = $1")).bind(id).fetch_optional(&mut *conn).await?
        .ok_or_else(|| AppError::not_found("Customer return"))?;
    let lines = sqlx::query_as::<_, CrLine>(
        "SELECT l.id, l.product_id, p.name AS product_name, p.units_per_crate, p.crates_per_pallet, l.batch_id, b.batch_no, b.expiry_date,
                l.crates_intact, l.crates_damaged, l.reason,
                CASE WHEN p.crates_per_pallet IS NULL THEN NULL ELSE l.crates_intact / p.crates_per_pallet END AS pallets_intact,
                CASE WHEN p.crates_per_pallet IS NULL THEN NULL ELSE l.crates_intact % p.crates_per_pallet END AS loose_intact
         FROM customer_return_lines l JOIN products p ON p.id = l.product_id JOIN production_batches b ON b.id = l.batch_id
         WHERE l.cr_id = $1 ORDER BY b.expiry_date, b.batch_no, l.id")
        .bind(id).fetch_all(&mut *conn).await?;
    let bottles: i64 = lines.iter().map(|l| (l.crates_intact + l.crates_damaged) as i64 * l.units_per_crate as i64).sum();
    let mut v = serde_json::to_value(&r).unwrap_or(Value::Null);
    v["plant_name"] = json!(plant);
    // same keys as `ui.printHeader({title, subtitle, docNo, date})`
    v["printHeader"] = json!({ "title": "CUSTOMER RETURN", "docNo": r.cr_no, "subtitle": r.location_name, "date": r.created_at, "plant_name": plant });
    v["totals"] = json!({ "crates_intact": r.total_intact, "crates_damaged": r.total_damaged, "crates": r.total_intact + r.total_damaged, "bottles": bottles });
    v["lines"] = serde_json::to_value(&lines).unwrap_or(Value::Null);
    Ok(v)
}

/// `CR-yymmdd-0001` from `cr_seq` (taken only after every check and stock move succeeded: no gaps from failed attempts).
async fn next_cr_no(conn: &mut sqlx::PgConnection) -> AppResult<String> {
    let n: i64 = sqlx::query_scalar("SELECT nextval('cr_seq')").fetch_one(&mut *conn).await?;
    let d = util::plant_today(&mut *conn).await?.format("%y%m%d");
    Ok(format!("CR-{d}-{n:04}"))
}

fn not_restockable(b: &BatchRow, today: NaiveDate) -> AppError {
    let (reason, msg) = if b.qc_status != "APPROVED" {
        ("NOT_APPROVED", format!("Batch {} is {} - intact crates of it cannot go back on the shelf. Book them as damaged instead.", b.batch_no, b.qc_status))
    } else {
        ("EXPIRED", format!("Batch {} expired on {} - intact crates of it cannot go back on the shelf. Book them as damaged instead.", b.batch_no, b.expiry_date))
    };
    AppError::conflict_with("NOT_RESTOCKABLE", msg,
        json!({ "batch_id": b.id, "batch_no": b.batch_no, "qc_status": b.qc_status, "expiry_date": b.expiry_date, "today": today, "reason": reason }))
}

/// Per-batch totals of a request / document.
#[derive(Default, Clone, Copy)]
struct Tot { intact: i64, damaged: i64 }

pub async fn create(State(st): State<AppState>, user: AuthUser, Q(dq): Q<DryQ>, J(b): J<NewReturn>) -> AppResult<Response> {
    let dry = dq.on();
    validate::max_items("lines", b.lines.len(), MAX_LINES)?;
    let party = validate::text("party_name", b.party_name.as_deref().unwrap_or(""), 120)?;
    let note = validate::text("note", b.note.as_deref().unwrap_or(""), 500)?;
    if b.customer_id.is_none() && party.is_empty() { return Err(AppError::field("party_name", "give a customer or a walk-in party name")); }
    let mut reasons: Vec<String> = Vec::with_capacity(b.lines.len());
    for (i, l) in b.lines.iter().enumerate() {
        for (f, v) in [("crates_intact", l.crates_intact), ("crates_damaged", l.crates_damaged)] {
            if !(0..=MAX_CRATES).contains(&v) { return Err(AppError::field(&format!("lines[{i}].{f}"), "must be a whole number between 0 and 1,000,000")); }
        }
        if l.crates_intact + l.crates_damaged < 1 { return Err(AppError::field(&format!("lines[{i}]"), "give at least one intact or damaged crate")); }
        let r = validate::text(&format!("lines[{i}].reason"), &l.reason, 200)?;
        if r.chars().count() < 3 { return Err(AppError::field(&format!("lines[{i}].reason"), "must be 3 to 200 characters (why is it coming back?)")); }
        reasons.push(r);
    }

    let mut tx = st.db.begin().await?;
    let mut fx = Fx::default();
    let loc = util::resolve_location(&mut tx, &user, b.location_id).await?; // authorisation before any lookup of the body's ids
    let mut cust_name: Option<String> = None;
    if let Some(c) = b.customer_id {
        let row: Option<(String, bool)> = sqlx::query_as("SELECT name, is_active FROM customers WHERE id = $1").bind(c).fetch_optional(&mut *tx).await?;
        match row { Some((n, true)) => cust_name = Some(n), _ => return Err(AppError::field("customer_id", "unknown or inactive customer")) }
    }
    let who = cust_name.clone().filter(|_| b.customer_id.is_some()).unwrap_or_else(|| party.clone());

    // products: must exist and must not be returnable containers
    let mut pids: Vec<Uuid> = b.lines.iter().map(|l| l.product_id).collect();
    pids.sort(); pids.dedup();
    let prods: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, kind FROM products WHERE id = ANY($1)").bind(&pids).fetch_all(&mut *tx).await?;
    let kinds: HashMap<Uuid, String> = prods.into_iter().collect();
    for (i, l) in b.lines.iter().enumerate() {
        match kinds.get(&l.product_id).map(String::as_str) {
            None => return Err(AppError::field(&format!("lines[{i}].product_id"), "unknown product")),
            Some("RETURNABLE") => return Err(AppError::field(&format!("lines[{i}].product_id"),
                "returnable containers are taken back through Jars (customer deposit tracking), not a customer return")),
            _ => {}
        }
    }

    // batch rows in the canonical order; every line needs a real batch of ITS product
    let mut bids: Vec<Uuid> = b.lines.iter().map(|l| l.batch_id).collect();
    bids.sort(); bids.dedup();
    let batches = lock_batches(&mut tx, &bids).await?;
    let bmap: HashMap<Uuid, &BatchRow> = batches.iter().map(|x| (x.id, x)).collect();
    for (i, l) in b.lines.iter().enumerate() {
        match bmap.get(&l.batch_id) {
            None => return Err(AppError::field(&format!("lines[{i}].batch_id"), "unknown batch")),
            Some(bt) if bt.product_id != l.product_id => return Err(AppError::field(&format!("lines[{i}].batch_id"), "this batch belongs to a different product")),
            _ => {}
        }
    }
    let mut tot: BTreeMap<Uuid, Tot> = BTreeMap::new();
    for l in &b.lines { let t = tot.entry(l.batch_id).or_default(); t.intact += l.crates_intact; t.damaged += l.crates_damaged; }

    // ---- hard rules first (never confirmable): restockable batch, never above what the batch produced
    let today = util::plant_today(&mut tx).await?;
    for bt in &batches {
        let t = tot[&bt.id];
        if t.intact > 0 && (bt.qc_status != "APPROVED" || bt.expiry_date < today) { return Err(not_restockable(bt, today)); }
    }
    for bt in &batches {
        let t = tot[&bt.id];
        if t.intact > 0 { stock::guard_over_produced(&mut tx, bt.id, Decimal::from(t.intact), Decimal::ZERO).await?; }
    }
    // ---- large quantities -> confirm
    let pct = st.settings.get().large_adjust_pct;
    let mut warnings: Vec<String> = vec![];
    for bt in &batches {
        let t = tot[&bt.id];
        let total = t.intact + t.damaged;
        let fg = lot_avail(&mut tx, loc, bt.product_id, bt.id, "FG_READY").await?.on_hand;
        if total >= LARGE_CRATES {
            warnings.push(format!("{total} crates of batch {} in one return is a large quantity (limit: {LARGE_CRATES} crates)", bt.batch_no));
        } else if fg > Decimal::ZERO && Decimal::from(total) * Decimal::from(100) > fg * Decimal::from(pct) {
            warnings.push(format!("{total} crates of batch {} is more than {pct}% of the {} crates of it in finished goods here", bt.batch_no, fg.normalize()));
        }
    }
    if !warnings.is_empty() {
        if !b.confirm { return Err(AppError::confirm("This return looks unusually large", warnings)); }
        for w in warnings { fx.warn(w); }
    }

    // ---- stock moves (one per batch and zone)
    let id = Uuid::new_v4();
    let mut labels = Labels::new();
    let mut moves = vec![];
    for bt in &batches {
        let t = tot[&bt.id];
        labels.insert((bt.product_id, bt.id), format!("{} · {}", bt.product_name, bt.batch_no));
        if t.intact > 0 {
            moves.push(product_move(loc, bt.product_id, "FG_READY", bt.id, t.intact, "CUSTOMER_RETURN")
                .reference("CUSTOMER_RETURN", id).by(Some(user.id)).expiry(Some(bt.expiry_date)).note(format!("counter return from {who}")));
        }
        if t.damaged > 0 {
            moves.push(product_move(loc, bt.product_id, "SCRAP_YARD", bt.id, t.damaged, "CUSTOMER_RETURN_DAMAGED")
                .reference("CUSTOMER_RETURN", id).by(Some(user.id)).expiry(Some(bt.expiry_date)).note(format!("damaged counter return from {who}")));
        }
    }
    post_moves(&mut tx, moves, &labels, &mut fx).await?;

    // the customer last in the lock order: re-check that it is still active while a share lock keeps it from being deactivated
    if let Some(c) = b.customer_id {
        let active: Option<bool> = sqlx::query_scalar("SELECT is_active FROM customers WHERE id = $1 FOR SHARE").bind(c).fetch_optional(&mut *tx).await?;
        if active != Some(true) { return Err(AppError::field("customer_id", "unknown or inactive customer")); }
    }

    // ---- the document (the number is taken last)
    let mut cr_no = format!("PENDING-{}", Uuid::new_v4());
    if !dry { cr_no = next_cr_no(&mut tx).await?; }
    sqlx::query("INSERT INTO customer_returns (id, cr_no, location_id, customer_id, party_name, note, created_by) VALUES ($1,$2,$3,$4,$5,$6,$7)")
        .bind(id).bind(&cr_no).bind(loc).bind(b.customer_id).bind(&party).bind(&note).bind(user.id).execute(&mut *tx).await?;
    for (l, r) in b.lines.iter().zip(&reasons) {
        sqlx::query("INSERT INTO customer_return_lines (cr_id, product_id, batch_id, crates_intact, crates_damaged, reason) VALUES ($1,$2,$3,$4,$5,$6)")
            .bind(id).bind(l.product_id).bind(l.batch_id).bind(l.crates_intact as i32).bind(l.crates_damaged as i32).bind(r).execute(&mut *tx).await?;
    }
    let intact: i64 = tot.values().map(|t| t.intact).sum();
    let damaged: i64 = tot.values().map(|t| t.damaged).sum();
    let out = cr_json(&mut tx, id, &st.settings.get().plant_name).await?;
    audit::log(&mut *tx, Some(&user), "CUSTOMER_RETURN", "customer_return", Some(id.to_string()),
        json!({ "cr_no": cr_no, "customer_id": b.customer_id, "party_name": party, "location_id": loc, "crates_intact": intact, "crates_damaged": damaged,
                "lines": b.lines.len(), "confirmed": b.confirm })).await;
    conclude(tx, dry, fx, StatusCode::CREATED, out).await
}

#[derive(Deserialize, Default)]
pub struct ListQ {
    status: Option<String>, q: Option<String>, customer_id: Option<Uuid>, location_id: Option<Uuid>, from: Option<NaiveDate>, to: Option<NaiveDate>,
    limit: Option<i64>, offset: Option<i64>,
}

/// `GET /customer-returns?status=&customer_id=&from=&to=&q=&location_id=&limit=&offset=` -> `{items,total}`, newest first, godown-scoped.
pub async fn list(State(st): State<AppState>, user: AuthUser, Q(q): Q<ListQ>) -> AppResult<Json<Value>> {
    let scope: Option<Vec<Uuid>> = match q.location_id {
        Some(l) => { user.require_location(l)?; Some(vec![l]) }
        None => location_scope(&user),
    };
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
    let filter = "WHERE ($1::text IS NULL OR r.status = $1)
        AND ($2::text IS NULL OR r.cr_no ILIKE $2 OR r.party_name ILIKE $2 OR COALESCE(c.name,'') ILIKE $2)
        AND ($3::date IS NULL OR ((r.created_at AT TIME ZONE 'UTC') + make_interval(mins => $5))::date >= $3)
        AND ($4::date IS NULL OR ((r.created_at AT TIME ZONE 'UTC') + make_interval(mins => $5))::date <= $4)
        AND ($6::uuid[] IS NULL OR r.location_id = ANY($6))
        AND ($7::uuid IS NULL OR r.customer_id = $7)";
    let rows = sqlx::query_as::<_, CrRow>(&format!("{CR_SELECT} {filter} ORDER BY r.created_at DESC, r.id ASC LIMIT $8 OFFSET $9"))
        .bind(&status).bind(&pat).bind(q.from).bind(q.to).bind(off).bind(&scope).bind(q.customer_id).bind(page.limit()).bind(page.offset())
        .fetch_all(&st.db).await?;
    let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM customer_returns r LEFT JOIN customers c ON c.id = r.customer_id {filter}"))
        .bind(&status).bind(&pat).bind(q.from).bind(q.to).bind(off).bind(&scope).bind(q.customer_id).fetch_one(&st.db).await?;
    Ok(Json(json!({ "items": rows, "total": total })))
}

/// `GET /customer-returns/{id}` - the printable document.
pub async fn detail(State(st): State<AppState>, user: AuthUser, ids: Ids) -> AppResult<Json<Value>> {
    let mut conn = st.db.acquire().await?;
    let id = id1(&ids)?;
    let loc: Uuid = sqlx::query_scalar("SELECT location_id FROM customer_returns WHERE id = $1").bind(id).fetch_optional(&mut *conn).await?
        .ok_or_else(|| AppError::not_found("Customer return"))?;
    user.require_location(loc)?;
    Ok(Json(cr_json(&mut conn, id, &st.settings.get().plant_name).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoidBody {
    reason: String,
    #[serde(default)]
    confirm: bool,
}

async fn reserved_error(conn: &mut sqlx::PgConnection, loc: Uuid, bt: &BatchRow, reserved: Decimal) -> AppError {
    let rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT t.trip_no, SUM(ti.crates)::bigint FROM trip_items ti JOIN loading_trips t ON t.id = ti.trip_id
         WHERE ti.batch_id = $1 AND t.status = 'LOADING' AND t.location_id = $2 GROUP BY t.trip_no ORDER BY t.trip_no")
        .bind(bt.id).bind(loc).fetch_all(&mut *conn).await.unwrap_or_default();
    let nos: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
    AppError::conflict_with("RESERVED_STOCK",
        format!("{} crates of batch {} are reserved by open loading trips ({}) - remove them from the trip first, then void this return", reserved.normalize(), bt.batch_no, nos.join(", ")),
        json!({ "batch_id": bt.id, "batch_no": bt.batch_no, "location_id": loc, "reserved": reserved,
                "trips": rows.iter().map(|r| json!({ "trip_no": r.0, "crates": r.1 })).collect::<Vec<_>>() }))
}

/// `POST /customer-returns/{id}/void {reason, confirm:true}` - reverses the stock moves, only while the returned intact crates are still
/// free in `FG_READY` (and the damaged ones still in `SCRAP_YARD`). Within `loading.void_window_hours`, Admin unlimited.
pub async fn void(State(st): State<AppState>, user: AuthUser, ids: Ids, Q(dq): Q<DryQ>, J(b): J<VoidBody>) -> AppResult<Response> {
    let dry = dq.on();
    let reason = validate::required_text("reason", &b.reason, 300)?;
    if reason.chars().count() < 5 { return Err(AppError::field("reason", "must be at least 5 characters")); }
    let id = id1(&ids)?;
    let mut tx = st.db.begin().await?;
    let mut fx = Fx::default();
    let head: Option<(String, String, DateTime<Utc>, Uuid)> = sqlx::query_as("SELECT cr_no, status, created_at, location_id FROM customer_returns WHERE id = $1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?;
    let (cr_no, status, created, loc) = head.ok_or_else(|| AppError::not_found("Customer return"))?;
    user.require_location(loc)?;
    if status != "ISSUED" { return Err(invalid_state(format!("Customer return {cr_no} is already {status}"), &status)); }
    if !user.is_admin {
        let window = st.settings.get().void_window_hours;
        if Utc::now() - created > Duration::hours(window) {
            return Err(AppError::forbidden(format!("The {window}-hour void window for this customer return has passed. Ask an Administrator.")));
        }
    }
    let lines: Vec<(Uuid, Uuid, i32, i32)> = sqlx::query_as("SELECT product_id, batch_id, crates_intact, crates_damaged FROM customer_return_lines WHERE cr_id = $1")
        .bind(id).fetch_all(&mut *tx).await?;
    let mut tot: BTreeMap<Uuid, Tot> = BTreeMap::new();
    for (_, bt, i, d) in &lines { let t = tot.entry(*bt).or_default(); t.intact += *i as i64; t.damaged += *d as i64; }
    let ids_v: Vec<Uuid> = tot.keys().cloned().collect();
    let batches = lock_batches(&mut tx, &ids_v).await?;

    // ---- the crates must still be where the return put them (batch rows locked; reads in fresh statements)
    for bt in &batches {
        let t = tot[&bt.id];
        if t.intact > 0 {
            let av = lot_avail(&mut tx, loc, bt.product_id, bt.id, "FG_READY").await?;
            if av.on_hand < Decimal::from(t.intact) {
                return Err(AppError::conflict_with("INSUFFICIENT_STOCK",
                    format!("Only {} of the {} intact crates of batch {} are still in finished goods here (the rest was sold, loaded or moved) - correct it with a write-off or stock adjustment instead of voiding",
                        av.on_hand.normalize(), t.intact, bt.batch_no),
                    json!({ "batch_id": bt.id, "batch_no": bt.batch_no, "zone": "FG_READY", "location_id": loc, "available": av.on_hand, "required": t.intact })));
            }
            if av.free < Decimal::from(t.intact) {
                return Err(reserved_error(&mut tx, loc, bt, av.on_hand - av.free).await);
            }
        }
        if t.damaged > 0 {
            let lot: Decimal = sqlx::query_scalar(
                "SELECT COALESCE(SUM(qty),0) FROM stock_balances WHERE item_type='PRODUCT' AND item_id=$1 AND zone='SCRAP_YARD' AND lot_id=$2 AND location_id=$3")
                .bind(bt.product_id).bind(bt.id).bind(loc).fetch_one(&mut *tx).await?;
            if lot < Decimal::from(t.damaged) {
                return Err(AppError::conflict_with("INSUFFICIENT_STOCK",
                    format!("Only {} of the {} damaged crates of batch {} are still in the scrap yard (they were disposed of or written off) - a void would take stock that is no longer there",
                        lot.normalize(), t.damaged, bt.batch_no),
                    json!({ "batch_id": bt.id, "batch_no": bt.batch_no, "zone": "SCRAP_YARD", "location_id": loc, "available": lot, "required": t.damaged })));
            }
        }
    }
    // ---- only now ask for the explicit confirmation (an error the operator cannot fix by confirming is reported first)
    if !b.confirm {
        let (i, d): (i64, i64) = tot.values().fold((0, 0), |a, t| (a.0 + t.intact, a.1 + t.damaged));
        return Err(AppError::confirm(format!("Voiding {cr_no} takes {i} intact and {d} damaged crates back out of stock"),
            vec![format!("{i} intact crates leave finished goods and {d} damaged crates leave the scrap yard."), "This cannot be undone.".into()]));
    }
    let mut labels = Labels::new();
    let mut moves = vec![];
    for bt in &batches {
        let t = tot[&bt.id];
        labels.insert((bt.product_id, bt.id), format!("{} · {}", bt.product_name, bt.batch_no));
        if t.intact > 0 {
            moves.push(product_move(loc, bt.product_id, "FG_READY", bt.id, -t.intact, "CUSTOMER_RETURN_VOID")
                .reference("CUSTOMER_RETURN", id).by(Some(user.id)).note(format!("void {cr_no}: {reason}")));
        }
        if t.damaged > 0 {
            moves.push(product_move(loc, bt.product_id, "SCRAP_YARD", bt.id, -t.damaged, "CUSTOMER_RETURN_VOID")
                .reference("CUSTOMER_RETURN", id).by(Some(user.id)).note(format!("void {cr_no}: {reason}")));
        }
    }
    post_moves(&mut tx, moves, &labels, &mut fx).await?;
    sqlx::query("UPDATE customer_returns SET status = 'VOID', void_reason = $2, voided_by = $3, voided_at = now() WHERE id = $1")
        .bind(id).bind(&reason).bind(user.id).execute(&mut *tx).await?;
    let out = cr_json(&mut tx, id, &st.settings.get().plant_name).await?;
    audit::log(&mut *tx, Some(&user), "CUSTOMER_RETURN_VOID", "customer_return", Some(id.to_string()), json!({ "cr_no": cr_no, "reason": reason, "location_id": loc })).await;
    conclude(tx, dry, fx, StatusCode::OK, out).await
}
