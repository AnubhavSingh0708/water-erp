//! `/api/jars/*` – returnable-container fleet (20 L jars etc.), page `jars`. OWNER: Subagent C2. See API_REFERENCE.md §0.2-E / §11.
//!
//! Model: containers of products with `kind = 'RETURNABLE'` live in four places – plant stock (`JAR_EMPTY`, `JAR_CLEAN`, filled in
//! `QC_QUARANTINE`/`FG_READY`), in vehicles of DISPATCHED trips, at customers (`jar_ledger`) – and leave the fleet through
//! `SCRAP_YARD` (destroyed). Every lifecycle change writes a `jar_fleet_events` row in the same transaction. Stock changes go only
//! through `crate::stock`; a customer's holding is serialised with an advisory lock and can never go negative (DB trigger 0014 as backstop).
use super::notifications::extract::{de_int, de_opt_int, is_dry_run, opt_date, opt_uuid, ApiJson, ApiPath, ApiQuery};
use crate::{
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    state::AppState,
    stock::{self, ItemType, Move, NIL_LOT},
    util,
    validate::{self, PageQuery},
};
use axum::{
    extract::State,
    http::{StatusCode, Uri},
    routing::{get, post},
    Json, Router,
};
use chrono::NaiveDate;
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const DESTROY_REASONS: &[&str] = &["WASH_REJECT", "CRACKED", "LEAKING", "DAMAGED_RETURN", "AGE_RETIRED", "LOST", "OTHER"];
const DESTROY_LOCATIONS: &[&str] = &["PLANT_EMPTY", "PLANT_CLEAN", "CUSTOMER"];
/// Fleet is flagged NEAR_RETIREMENT from this share of `max_cycles`.
pub const RETIREMENT_ALERT_PCT: i64 = 90;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/summary", get(summary))
        .route("/fleet", get(fleet))
        .route("/customers", get(customers))
        .route("/customers/{id}/ledger", get(customer_ledger))
        .route("/opening", post(opening))
        .route("/issue", post(issue))
        .route("/receive-empties", post(receive_empties))
        .route("/receive-new", post(receive_new))
        .route("/wash", post(wash))
        .route("/destroy", post(destroy))
        .route("/destroyed", get(destroyed))
}

// ---------------------------------------------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ReturnableProduct { pub id: Uuid, pub code: String, pub name: String, pub size_ml: Option<i32>, pub max_cycles: Option<i32> }

/// Product must exist, be active and `RETURNABLE` (BOTTLED -> 400).
pub async fn load_returnable(conn: &mut PgConnection, id: Uuid) -> AppResult<ReturnableProduct> {
    let r = sqlx::query("SELECT id, code, name, size_ml, kind, is_active, max_cycles FROM products WHERE id = $1")
        .bind(id).fetch_optional(&mut *conn).await?.ok_or_else(|| AppError::not_found("product"))?;
    if r.get::<String, _>("kind") != "RETURNABLE" {
        return Err(AppError::bad("This product is not a returnable container; only RETURNABLE products can be used on the jars pages"));
    }
    if !r.get::<bool, _>("is_active") { return Err(AppError::bad("This product is inactive")); }
    Ok(ReturnableProduct { id, code: r.get("code"), name: r.get("name"), size_ml: r.get("size_ml"), max_cycles: r.get("max_cycles") })
}

pub async fn active_returnable_products(conn: &mut PgConnection) -> AppResult<Vec<ReturnableProduct>> {
    let rows = sqlx::query("SELECT id, code, name, size_ml, max_cycles FROM products WHERE kind = 'RETURNABLE' AND is_active ORDER BY name, id")
        .fetch_all(&mut *conn).await?;
    Ok(rows.iter().map(|r| ReturnableProduct { id: r.get("id"), code: r.get("code"), name: r.get("name"), size_ml: r.get("size_ml"), max_cycles: r.get("max_cycles") }).collect())
}

async fn load_customer(conn: &mut PgConnection, id: Uuid) -> AppResult<(String, bool)> {
    let r = sqlx::query("SELECT name, is_active FROM customers WHERE id = $1").bind(id).fetch_optional(&mut *conn).await?
        .ok_or_else(|| AppError::not_found("customer"))?;
    Ok((r.get("name"), r.get("is_active")))
}

async fn load_active_customer(conn: &mut PgConnection, id: Uuid) -> AppResult<String> {
    let (name, active) = load_customer(conn, id).await?;
    if !active { return Err(AppError::bad("This customer is inactive")); }
    Ok(name)
}

/// Serialises every writer of one (customer, product) holding and returns the current holding. Same key as migration 0014.
async fn lock_holding(conn: &mut PgConnection, customer: Uuid, product: Uuid) -> AppResult<i64> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))").bind(format!("jar:{customer}:{product}")).execute(&mut *conn).await?;
    holding(conn, customer, product).await
}

async fn holding(conn: &mut PgConnection, customer: Uuid, product: Uuid) -> AppResult<i64> {
    Ok(sqlx::query_scalar("SELECT COALESCE(SUM(delta), 0)::bigint FROM jar_ledger WHERE customer_id = $1 AND product_id = $2")
        .bind(customer).bind(product).fetch_one(&mut *conn).await?)
}

async fn zone_qty(conn: &mut PgConnection, loc: Uuid, product: Uuid, zone: &str) -> AppResult<i64> {
    let d = stock::available_at(conn, loc, ItemType::Product, product, zone).await?;
    Ok(d.to_i64().unwrap_or(0))
}

/// Godowns the caller may use (active, never TRANSIT), main plant first.
pub async fn allowed_locations(conn: &mut PgConnection, user: &AuthUser) -> AppResult<Vec<(Uuid, String)>> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, name FROM locations WHERE is_active AND kind <> 'TRANSIT' ORDER BY (kind = 'PLANT') DESC, name, id")
        .fetch_all(&mut *conn).await?;
    Ok(rows.into_iter().filter(|(id, _)| user.can_use_location(*id)).collect())
}

/// Which godown(s) a read covers: one resolved godown, or `location_id=all` = every godown the caller may use.
pub enum Scope { One(Uuid), All(Vec<Uuid>) }
impl Scope {
    pub fn ids(&self) -> Vec<Uuid> { match self { Scope::One(i) => vec![*i], Scope::All(v) => v.clone() } }
    fn label(&self) -> Value { match self { Scope::One(i) => json!(i), Scope::All(_) => json!("all") } }
}
pub async fn read_scope(conn: &mut PgConnection, user: &AuthUser, raw: &Option<String>) -> AppResult<Scope> {
    match raw.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some("all") => Ok(Scope::All(allowed_locations(conn, user).await?.into_iter().map(|l| l.0).collect())),
        _ => {
            let id = opt_uuid("location_id", raw)?;
            Ok(Scope::One(util::resolve_location(conn, user, id).await?))
        }
    }
}

fn fmt4(n: i64) -> String { format!("{n}.0000") }
fn fmt2(d: Decimal) -> String { format!("{:.2}", d.round_dp(2)) }

async fn stock_fx(conn: &mut PgConnection, loc: Uuid, p: &ReturnableProduct, zone: &'static str, delta: i64) -> AppResult<Value> {
    let after = zone_qty(conn, loc, p.id, zone).await?;
    Ok(json!({ "item": p.name, "zone": zone, "location_id": loc, "delta": fmt4(delta), "after": fmt4(after) }))
}

fn customer_fx(name: &str, p: &ReturnableProduct, delta: i64, after: i64) -> Value {
    json!({ "item": format!("{} held by {}", p.name, name), "zone": "CUSTOMER", "delta": fmt4(delta), "after": fmt4(after) })
}

/// Record a fleet event (the CHECK constraint ties `reason_code` to DESTROYED).
#[allow(clippy::too_many_arguments)]
async fn fleet_event(conn: &mut PgConnection, product: Uuid, kind: &str, qty: i64, reason: Option<&str>, location: Option<&str>,
                     customer: Option<Uuid>, supplier: Option<Uuid>, note: &str, user: &AuthUser, loc: Uuid) -> AppResult<Uuid> {
    Ok(sqlx::query_scalar(
        "INSERT INTO jar_fleet_events (product_id, event_type, qty, reason_code, location, customer_id, supplier_id, note, created_by, location_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id")
        .bind(product).bind(kind).bind(qty as i32).bind(reason).bind(location).bind(customer).bind(supplier).bind(note).bind(user.id).bind(loc)
        .fetch_one(&mut *conn).await?)
}

#[allow(clippy::too_many_arguments)]
async fn ledger_row(conn: &mut PgConnection, customer: Uuid, product: Uuid, txn: &str, delta: i64, date: NaiveDate, note: &str, user: &AuthUser) -> AppResult<Uuid> {
    Ok(sqlx::query_scalar(
        "INSERT INTO jar_ledger (customer_id, product_id, txn_type, delta, txn_date, note, created_by) VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id")
        .bind(customer).bind(product).bind(txn).bind(delta as i32).bind(date).bind(note).bind(user.id).fetch_one(&mut *conn).await?)
}

fn count_field(field: &str, n: i64) -> AppResult<()> { validate::count(field, n) }
fn count_or_zero(field: &str, n: i64) -> AppResult<()> { if n == 0 { Ok(()) } else { validate::count(field, n) } }

fn opt_note(note: &Option<String>) -> AppResult<String> {
    match note { Some(n) => validate::text("note", n, 500), None => Ok(String::new()) }
}

fn need_confirm(confirm: bool, msg: &str, warnings: &[String]) -> AppResult<()> {
    if !warnings.is_empty() && !confirm { Err(AppError::confirm(msg, warnings.to_vec())) } else { Ok(()) }
}

struct Done { status: StatusCode, body: Value, effects: Vec<Value>, warnings: Vec<String> }

/// Dry run: roll back and describe the effects. Real run: commit and return the body with `effects` and `warnings` merged in.
async fn finish(tx: sqlx::Transaction<'_, sqlx::Postgres>, dry: bool, d: Done) -> AppResult<(StatusCode, Json<Value>)> {
    if dry {
        tx.rollback().await?;
        return Ok((StatusCode::OK, Json(json!({ "dry_run": true, "effects": d.effects, "warnings": d.warnings }))));
    }
    tx.commit().await?;
    let mut body = d.body;
    body["effects"] = json!(d.effects);
    body["warnings"] = json!(d.warnings);
    Ok((d.status, Json(body)))
}

// ---------------------------------------------------------------------------------------------------------------
// fleet counts + analytics
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct FleetCounts { pub plant_empty: i64, pub plant_clean: i64, pub filled_ready: i64, pub filled_quarantine: i64, pub with_customers: i64, pub in_vehicles: i64 }
impl FleetCounts {
    pub fn total(&self) -> i64 { self.plant_empty + self.plant_clean + self.filled_ready + self.filled_quarantine + self.with_customers + self.in_vehicles }
    fn json(&self) -> Value {
        json!({ "total": self.total(), "plant_empty": self.plant_empty, "plant_clean": self.plant_clean, "filled_ready": self.filled_ready,
                "filled_quarantine": self.filled_quarantine, "with_customers": self.with_customers, "in_vehicles": self.in_vehicles })
    }
}

/// Plant-side counts for the given godowns (`None` = every location incl. TRANSIT = the whole fleet). Customer holdings are global.
pub async fn fleet_counts(conn: &mut PgConnection, product: Uuid, locs: Option<&[Uuid]>) -> AppResult<FleetCounts> {
    let rows = sqlx::query("SELECT zone, COALESCE(SUM(qty), 0) AS q FROM stock_balances WHERE item_type = 'PRODUCT' AND item_id = $1 AND ($2::uuid[] IS NULL OR location_id = ANY($2)) GROUP BY zone")
        .bind(product).bind(locs).fetch_all(&mut *conn).await?;
    let mut c = FleetCounts::default();
    for r in rows {
        let q = r.get::<Decimal, _>("q").to_i64().unwrap_or(0);
        match r.get::<String, _>("zone").as_str() {
            "JAR_EMPTY" => c.plant_empty = q,
            "JAR_CLEAN" => c.plant_clean = q,
            "FG_READY" => c.filled_ready = q,
            "QC_QUARANTINE" => c.filled_quarantine = q,
            _ => {}
        }
    }
    c.with_customers = sqlx::query_scalar("SELECT COALESCE(SUM(delta), 0)::bigint FROM jar_ledger WHERE product_id = $1").bind(product).fetch_one(&mut *conn).await?;
    c.in_vehicles = sqlx::query_scalar(
        "SELECT COALESCE(SUM(ti.crates), 0)::bigint FROM trip_items ti JOIN loading_trips t ON t.id = ti.trip_id
         WHERE t.status = 'DISPATCHED' AND ti.product_id = $1 AND ($2::uuid[] IS NULL OR t.location_id = ANY($2))")
        .bind(product).bind(locs).fetch_one(&mut *conn).await?;
    Ok(c)
}

#[derive(Clone, Debug, Default)]
pub struct PeriodStats {
    pub fills: i64, pub delivered: i64, pub empties_returned: i64, pub new_purchased: i64, pub washed: i64,
    pub destroyed_by_reason: BTreeMap<String, i64>, pub destroyed_total: i64,
}

/// Activity between two plant-local dates (inclusive).
pub async fn period_stats(conn: &mut PgConnection, product: Uuid, from: NaiveDate, to: NaiveDate, offset_min: i64, locs: Option<&[Uuid]>) -> AppResult<PeriodStats> {
    let mut s = PeriodStats::default();
    for r in DESTROY_REASONS { s.destroyed_by_reason.insert((*r).to_string(), 0); }
    s.fills = sqlx::query_scalar("SELECT COALESCE(SUM(crates), 0)::bigint FROM production_batches WHERE product_id = $1 AND produced_on BETWEEN $2 AND $3 AND ($4::uuid[] IS NULL OR location_id = ANY($4))")
        .bind(product).bind(from).bind(to).bind(locs).fetch_one(&mut *conn).await?;
    let l = sqlx::query(
        "SELECT COALESCE(SUM(delta) FILTER (WHERE txn_type = 'ISSUE_FILLED'), 0)::bigint AS delivered,
                COALESCE(-SUM(delta) FILTER (WHERE txn_type = 'RETURN_EMPTY'), 0)::bigint AS returned
         FROM jar_ledger WHERE product_id = $1 AND txn_date BETWEEN $2 AND $3")
        .bind(product).bind(from).bind(to).fetch_one(&mut *conn).await?;
    s.delivered = l.get("delivered");
    s.empties_returned = l.get("returned");
    let ev = sqlx::query(
        "SELECT event_type, reason_code, COALESCE(SUM(qty), 0)::bigint AS q FROM jar_fleet_events
         WHERE product_id = $1 AND (created_at + ($4::int * interval '1 minute'))::date BETWEEN $2 AND $3
           AND ($5::uuid[] IS NULL OR COALESCE(location_id, '00000000-0000-0000-0000-000000000001') = ANY($5)) GROUP BY event_type, reason_code")
        .bind(product).bind(from).bind(to).bind(offset_min as i32).bind(locs).fetch_all(&mut *conn).await?;
    for r in ev {
        let q: i64 = r.get("q");
        match r.get::<String, _>("event_type").as_str() {
            "NEW_PURCHASED" => s.new_purchased += q,
            "WASHED" => s.washed += q,
            "DESTROYED" => {
                s.destroyed_total += q;
                if let Some(code) = r.get::<Option<String>, _>("reason_code") { *s.destroyed_by_reason.entry(code).or_insert(0) += q; }
            }
            _ => {}
        }
    }
    Ok(s)
}

#[derive(Clone, Debug)]
pub struct FleetReport {
    pub product: ReturnableProduct, pub from: NaiveDate, pub to: NaiveDate,
    pub counts: FleetCounts, pub period: PeriodStats,
    pub lifetime_fills: i64, pub lifetime_destroyed: i64, pub warn_pct: i64,
    /// whole fleet (all godowns + transit + customers + vehicles): the denominator of the cycle estimate
    pub global_total: i64,
}

impl FleetReport {
    /// Share of the period's fills that were destroyed (None without fills).
    pub fn destroy_rate_pct(&self) -> Option<Decimal> {
        if self.period.fills <= 0 { return None; }
        Some(Decimal::from(self.period.destroyed_total) * Decimal::from(100) / Decimal::from(self.period.fills))
    }
    /// Lifetime fills ÷ every container the plant has ever owned (current fleet + lifetime destroyed).
    pub fn avg_cycles(&self) -> Option<Decimal> {
        let denom = self.global_total + self.lifetime_destroyed;
        if denom <= 0 { return None; }
        Some(Decimal::from(self.lifetime_fills) / Decimal::from(denom))
    }
    pub fn retirement_pct(&self) -> Option<Decimal> {
        let (avg, max) = (self.avg_cycles()?, self.product.max_cycles?);
        Some(avg * Decimal::from(100) / Decimal::from(max))
    }
    /// destroyed ÷ fills > warn %. With no fills any destruction counts as high.
    pub fn high_destroy_rate(&self) -> bool {
        self.period.destroyed_total > 0 && self.period.destroyed_total * 100 > self.warn_pct * self.period.fills
    }
    pub fn near_retirement(&self) -> bool { self.retirement_pct().map_or(false, |p| p >= Decimal::from(RETIREMENT_ALERT_PCT)) }
    pub fn alert(&self) -> &'static str {
        if self.high_destroy_rate() { "HIGH_DESTROY_RATE" } else if self.near_retirement() { "NEAR_RETIREMENT" } else { "OK" }
    }
    fn json(&self) -> Value {
        let mut reasons = serde_json::Map::new();
        for (k, v) in &self.period.destroyed_by_reason { reasons.insert(k.clone(), json!(v)); }
        let mut alerts = vec![];
        if self.high_destroy_rate() { alerts.push("HIGH_DESTROY_RATE"); }
        if self.near_retirement() { alerts.push("NEAR_RETIREMENT"); }
        json!({
            "product": { "id": self.product.id, "code": self.product.code, "name": self.product.name, "size_ml": self.product.size_ml },
            "fleet": self.counts.json(),
            "period": { "from": self.from, "to": self.to, "fills": self.period.fills, "delivered": self.period.delivered,
                        "empties_returned": self.period.empties_returned, "new_purchased": self.period.new_purchased, "washed": self.period.washed,
                        "destroyed_total": self.period.destroyed_total, "destroyed_by_reason": reasons },
            "lifetime": { "fills": self.lifetime_fills, "destroyed": self.lifetime_destroyed },
            "destroy_rate_pct": self.destroy_rate_pct().map(fmt2),
            "avg_cycles_estimate": self.avg_cycles().map(fmt2),
            "max_cycles": self.product.max_cycles,
            "retirement_pct": self.retirement_pct().map(fmt2),
            "warn_pct": self.warn_pct,
            "alert": self.alert(),
            "alerts": alerts,
        })
    }
}

/// `locs = None` => whole plant (used by the alert worker); `Some(godowns)` => counts and period activity of those godowns.
pub async fn fleet_report(conn: &mut PgConnection, product: ReturnableProduct, from: NaiveDate, to: NaiveDate, offset_min: i64, warn_pct: i64, locs: Option<&[Uuid]>) -> AppResult<FleetReport> {
    let counts = fleet_counts(conn, product.id, locs).await?;
    let global_total = if locs.is_none() { counts.total() } else { fleet_counts(conn, product.id, None).await?.total() };
    let period = period_stats(conn, product.id, from, to, offset_min, locs).await?;
    let lifetime_fills: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(crates), 0)::bigint FROM production_batches WHERE product_id = $1").bind(product.id).fetch_one(&mut *conn).await?;
    let lifetime_destroyed: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(qty), 0)::bigint FROM jar_fleet_events WHERE product_id = $1 AND event_type = 'DESTROYED'").bind(product.id).fetch_one(&mut *conn).await?;
    Ok(FleetReport { product, from, to, counts, period, lifetime_fills, lifetime_destroyed, warn_pct, global_total })
}

// ---------------------------------------------------------------------------------------------------------------
// customer balances (FIFO)
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Balance {
    pub customer_id: Uuid, pub customer_name: String, pub phone: String, pub product_id: Uuid, pub product_name: String,
    pub holding: i64, pub oldest: Option<NaiveDate>, pub days_outstanding: i64, pub overdue: bool,
}

/// Holdings per (customer, product). FIFO: every return/damage/loss/write-off consumes the OLDEST issue/opening layers first,
/// so `oldest_outstanding_date` is the date of the oldest layer that still has jars outstanding. Overdue = holding > 0 and
/// `days_outstanding > overdue_days`.
pub async fn balances(conn: &mut PgConnection, product: Option<Uuid>, customer: Option<Uuid>, q: Option<&str>, include_zero: bool,
                      today: NaiveDate, overdue_days: i64) -> AppResult<Vec<Balance>> {
    let pattern = q.map(str::trim).filter(|s| !s.is_empty()).map(validate::like_pattern);
    let rows = sqlx::query(
        "WITH base AS (
             SELECT customer_id, product_id, txn_date, delta,
                    SUM(GREATEST(delta, 0)) OVER (PARTITION BY customer_id, product_id ORDER BY txn_date, created_at, id) AS cum_in
             FROM jar_ledger WHERE ($1::uuid IS NULL OR product_id = $1) AND ($2::uuid IS NULL OR customer_id = $2)
         ), tot AS (
             SELECT customer_id, product_id, SUM(delta)::bigint AS holding,
                    COALESCE(-SUM(delta) FILTER (WHERE delta < 0), 0)::bigint AS returned
             FROM base GROUP BY customer_id, product_id
         )
         SELECT t.customer_id, c.name AS customer_name, c.phone, t.product_id, p.name AS product_name, t.holding,
                (SELECT MIN(b.txn_date) FROM base b WHERE b.customer_id = t.customer_id AND b.product_id = t.product_id
                    AND b.delta > 0 AND b.cum_in > t.returned) AS oldest
         FROM tot t JOIN customers c ON c.id = t.customer_id JOIN products p ON p.id = t.product_id
         WHERE (t.holding > 0 OR $3::bool) AND p.kind = 'RETURNABLE'
           AND ($4::text IS NULL OR c.name ILIKE $4 OR c.code ILIKE $4 OR c.phone ILIKE $4)
         ORDER BY c.name, p.name LIMIT 20000")
        .bind(product).bind(customer).bind(include_zero).bind(pattern).fetch_all(&mut *conn).await?;
    Ok(rows.iter().map(|r| {
        let holding: i64 = r.get("holding");
        let oldest: Option<NaiveDate> = r.get("oldest");
        let days = if holding > 0 { oldest.map_or(0, |d| (today - d).num_days().max(0)) } else { 0 };
        Balance {
            customer_id: r.get("customer_id"), customer_name: r.get("customer_name"), phone: r.get("phone"), product_id: r.get("product_id"),
            product_name: r.get("product_name"), holding, oldest: if holding > 0 { oldest } else { None }, days_outstanding: days,
            overdue: holding > 0 && days > overdue_days,
        }
    }).collect())
}

impl Balance {
    fn json(&self) -> Value {
        json!({ "customer_id": self.customer_id, "name": self.customer_name, "phone": self.phone, "product_id": self.product_id, "product_name": self.product_name,
                "holding": self.holding, "oldest_outstanding_date": self.oldest, "days_outstanding": self.days_outstanding, "overdue": self.overdue })
    }
}

// ---------------------------------------------------------------------------------------------------------------
// GET endpoints
// ---------------------------------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct SummaryQuery { location_id: Option<String> }

async fn summary(State(s): State<AppState>, u: AuthUser, ApiQuery(q): ApiQuery<SummaryQuery>) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let scope = read_scope(&mut c, &u, &q.location_id).await?;
    let locs = scope.ids();
    let today = util::plant_today(&mut c).await?;
    let st = s.settings.get();
    let mut out = vec![];
    for p in active_returnable_products(&mut c).await? {
        let f = fleet_counts(&mut c, p.id, Some(&locs)).await?;
        let overdue = balances(&mut c, Some(p.id), None, None, false, today, st.jar_overdue_days).await?.iter().filter(|b| b.overdue).count();
        out.push(json!({
            "product_id": p.id, "code": p.code, "name": p.name, "size_ml": p.size_ml, "max_cycles": p.max_cycles,
            "plant_empty": f.plant_empty, "plant_clean": f.plant_clean, "filled_ready": f.filled_ready, "filled_quarantine": f.filled_quarantine,
            "with_customers": f.with_customers, "in_vehicles": f.in_vehicles, "fleet_total": f.total(), "overdue_customers": overdue,
            "location_id": scope.label(),
        }));
    }
    Ok(Json(Value::Array(out)))
}

#[derive(Deserialize, Default)]
struct FleetQuery { product_id: Option<String>, from: Option<String>, to: Option<String>, location_id: Option<String> }

async fn fleet(State(s): State<AppState>, u: AuthUser, ApiQuery(q): ApiQuery<FleetQuery>) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let scope = read_scope(&mut c, &u, &q.location_id).await?;
    let today = util::plant_today(&mut c).await?;
    let to = opt_date("to", &q.to)?.unwrap_or(today);
    let from = opt_date("from", &q.from)?.unwrap_or(to - chrono::Duration::days(29));
    if from > to { return Err(AppError::field("from", "must not be after 'to'")); }
    if (to - from).num_days() > 3660 { return Err(AppError::field("from", "period is too long (max 10 years)")); }
    let product = match opt_uuid("product_id", &q.product_id)? {
        Some(id) => load_returnable(&mut c, id).await?,
        None => {
            let mut all = active_returnable_products(&mut c).await?;
            if all.len() == 1 { all.remove(0) } else { return Err(AppError::field("product_id", "is required when there is more than one returnable product")); }
        }
    };
    let st = s.settings.get();
    let locs = scope.ids();
    let pid = product.id;
    let rep = fleet_report(&mut c, product, from, to, st.utc_offset_minutes, st.jar_destroy_warn_pct, Some(&locs)).await?;
    let mut out = rep.json();
    out["location_id"] = scope.label();
    out["by_location"] = Value::Array(by_location(&mut c, &u, pid).await?);
    Ok(Json(out))
}

/// Stock of one product per godown the caller may use (zero rows included so the UI can show every godown).
async fn by_location(conn: &mut PgConnection, user: &AuthUser, product: Uuid) -> AppResult<Vec<Value>> {
    let locs = allowed_locations(conn, user).await?;
    let ids: Vec<Uuid> = locs.iter().map(|l| l.0).collect();
    let rows = sqlx::query("SELECT location_id, zone, COALESCE(SUM(qty), 0) AS q FROM stock_balances WHERE item_type = 'PRODUCT' AND item_id = $1 AND location_id = ANY($2) GROUP BY location_id, zone")
        .bind(product).bind(&ids).fetch_all(&mut *conn).await?;
    Ok(locs.iter().map(|(id, name)| {
        let q = |zone: &str| -> i64 { rows.iter().filter(|r| r.get::<Uuid, _>("location_id") == *id && r.get::<String, _>("zone") == zone).map(|r| r.get::<Decimal, _>("q").to_i64().unwrap_or(0)).sum() };
        json!({ "location_id": id, "name": name, "plant_empty": q("JAR_EMPTY"), "plant_clean": q("JAR_CLEAN"), "filled_ready": q("FG_READY"), "filled_quarantine": q("QC_QUARANTINE") })
    }).collect())
}

#[derive(Deserialize, Default)]
struct CustomersQuery { product_id: Option<String>, overdue: Option<String>, q: Option<String>, all: Option<String> }

async fn customers(State(s): State<AppState>, _u: AuthUser, ApiQuery(q): ApiQuery<CustomersQuery>) -> AppResult<Json<Value>> {
    let product = opt_uuid("product_id", &q.product_id)?;
    if let Some(t) = &q.q { if t.chars().count() > 100 { return Err(AppError::field("q", "must be at most 100 characters")); } }
    let flag = |v: &Option<String>| matches!(v.as_deref(), Some("1") | Some("true") | Some("yes"));
    let mut c = s.db.acquire().await?;
    let today = util::plant_today(&mut c).await?;
    let mut rows = balances(&mut c, product, None, q.q.as_deref(), flag(&q.all), today, s.settings.get().jar_overdue_days).await?;
    if flag(&q.overdue) { rows.retain(|b| b.overdue); }
    rows.sort_by(|a, b| b.overdue.cmp(&a.overdue).then(b.days_outstanding.cmp(&a.days_outstanding)).then(a.customer_name.cmp(&b.customer_name)));
    rows.truncate(500);
    Ok(Json(Value::Array(rows.iter().map(Balance::json).collect())))
}

#[derive(Deserialize, Default)]
struct LedgerQuery { product_id: Option<String>, limit: Option<i64>, offset: Option<i64> }

async fn customer_ledger(State(s): State<AppState>, _u: AuthUser, ApiPath(id): ApiPath<Uuid>, ApiQuery(q): ApiQuery<LedgerQuery>) -> AppResult<Json<Value>> {
    let product = opt_uuid("product_id", &q.product_id)?;
    let p = PageQuery { limit: q.limit, offset: q.offset };
    let mut c = s.db.acquire().await?;
    let (name, _) = load_customer(&mut c, id).await?;
    let rows = sqlx::query(
        "SELECT * FROM (
             SELECT l.id, l.product_id, p.name AS product_name, l.txn_type, l.delta, l.txn_date, l.trip_id, t.trip_no, l.note, l.created_at,
                    u.full_name AS created_by_name,
                    SUM(l.delta) OVER (PARTITION BY l.product_id ORDER BY l.txn_date, l.created_at, l.id)::bigint AS balance_after
             FROM jar_ledger l JOIN products p ON p.id = l.product_id LEFT JOIN loading_trips t ON t.id = l.trip_id LEFT JOIN users u ON u.id = l.created_by
             WHERE l.customer_id = $1 AND ($2::uuid IS NULL OR l.product_id = $2)) x
         ORDER BY txn_date DESC, created_at DESC, id DESC LIMIT $3 OFFSET $4")
        .bind(id).bind(product).bind(p.limit()).bind(p.offset()).fetch_all(&mut *c).await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jar_ledger WHERE customer_id = $1 AND ($2::uuid IS NULL OR product_id = $2)").bind(id).bind(product).fetch_one(&mut *c).await?;
    let items: Vec<Value> = rows.iter().map(|r| json!({
        "id": r.get::<Uuid, _>("id"), "product_id": r.get::<Uuid, _>("product_id"), "product_name": r.get::<String, _>("product_name"),
        "txn_type": r.get::<String, _>("txn_type"), "delta": r.get::<i32, _>("delta"), "balance_after": r.get::<i64, _>("balance_after"),
        "txn_date": r.get::<NaiveDate, _>("txn_date"), "trip_id": r.get::<Option<Uuid>, _>("trip_id"), "trip_no": r.get::<Option<String>, _>("trip_no"),
        "note": r.get::<String, _>("note"), "created_by_name": r.get::<Option<String>, _>("created_by_name"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
    })).collect();
    Ok(Json(json!({ "customer": { "id": id, "name": name }, "items": items, "total": total })))
}

#[derive(Deserialize, Default)]
struct DestroyedQuery { product_id: Option<String>, reason: Option<String>, from: Option<String>, to: Option<String>, limit: Option<i64>, offset: Option<i64> }

async fn destroyed(State(s): State<AppState>, _u: AuthUser, ApiQuery(q): ApiQuery<DestroyedQuery>) -> AppResult<Json<Value>> {
    let product = opt_uuid("product_id", &q.product_id)?;
    let (from, to) = (opt_date("from", &q.from)?, opt_date("to", &q.to)?);
    let reason = q.reason.as_deref().map(str::trim).filter(|r| !r.is_empty());
    if let Some(r) = reason { if !DESTROY_REASONS.contains(&r) { return Err(AppError::field("reason", "unknown reason code")); } }
    let p = PageQuery { limit: q.limit, offset: q.offset };
    let off = s.settings.get().utc_offset_minutes as i32;
    const FILTER: &str = "e.event_type = 'DESTROYED' AND ($1::uuid IS NULL OR e.product_id = $1) AND ($2::text IS NULL OR e.reason_code = $2)
        AND ($3::date IS NULL OR (e.created_at + ($5::int * interval '1 minute'))::date >= $3) AND ($4::date IS NULL OR (e.created_at + ($5::int * interval '1 minute'))::date <= $4)";
    let rows = sqlx::query(&format!(
        "SELECT e.id, e.product_id, e.location_id, p.name AS product_name, e.qty, e.reason_code, e.location, e.customer_id, c.name AS customer_name, e.trip_id, t.trip_no, e.note,
                e.created_at, u.full_name AS created_by_name
         FROM jar_fleet_events e JOIN products p ON p.id = e.product_id LEFT JOIN customers c ON c.id = e.customer_id
              LEFT JOIN loading_trips t ON t.id = e.trip_id LEFT JOIN users u ON u.id = e.created_by
         WHERE {FILTER} ORDER BY e.created_at DESC, e.id DESC LIMIT $6 OFFSET $7"))
        .bind(product).bind(reason).bind(from).bind(to).bind(off).bind(p.limit()).bind(p.offset()).fetch_all(&s.db).await?;
    let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM jar_fleet_events e WHERE {FILTER}"))
        .bind(product).bind(reason).bind(from).bind(to).bind(off).fetch_one(&s.db).await?;
    let items: Vec<Value> = rows.iter().map(|r| json!({
        "id": r.get::<Uuid, _>("id"), "product_id": r.get::<Uuid, _>("product_id"), "product_name": r.get::<String, _>("product_name"),
        "location_id": r.get::<Option<Uuid>, _>("location_id"), "qty": r.get::<i32, _>("qty"), "reason_code": r.get::<Option<String>, _>("reason_code"), "location": r.get::<Option<String>, _>("location"),
        "customer_id": r.get::<Option<Uuid>, _>("customer_id"), "customer_name": r.get::<Option<String>, _>("customer_name"),
        "trip_id": r.get::<Option<Uuid>, _>("trip_id"), "trip_no": r.get::<Option<String>, _>("trip_no"), "note": r.get::<String, _>("note"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"), "created_by_name": r.get::<Option<String>, _>("created_by_name"),
    })).collect();
    Ok(Json(json!({ "items": items, "total": total })))
}

// ---------------------------------------------------------------------------------------------------------------
// POST endpoints
// ---------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpeningReq {
    customer_id: Uuid, product_id: Uuid,
    #[serde(deserialize_with = "de_int")] qty: i64,
    /// date the containers were handed out (default today); drives the overdue calculation
    as_of: Option<NaiveDate>,
    note: Option<String>,
    #[serde(default)] confirm: bool,
}

/// Opening balance: containers a customer already holds when the system starts.
async fn opening(State(s): State<AppState>, u: AuthUser, uri: Uri, ApiJson(b): ApiJson<OpeningReq>) -> AppResult<(StatusCode, Json<Value>)> {
    count_field("qty", b.qty)?;
    let note = opt_note(&b.note)?;
    let dry = is_dry_run(&uri);
    let mut tx = s.db.begin().await?;
    let product = load_returnable(&mut tx, b.product_id).await?;
    let cname = load_active_customer(&mut tx, b.customer_id).await?;
    let before = lock_holding(&mut tx, b.customer_id, product.id).await?;
    let today = util::plant_today(&mut tx).await?;
    let date = b.as_of.unwrap_or(today);
    if date > today { return Err(AppError::field("as_of", "must not be in the future")); }
    if (today - date).num_days() > 3660 { return Err(AppError::field("as_of", "is too far in the past")); }
    let mut warnings = vec![];
    let existing: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(delta), 0)::bigint FROM jar_ledger WHERE customer_id = $1 AND product_id = $2 AND txn_type = 'OPENING'")
        .bind(b.customer_id).bind(product.id).fetch_one(&mut *tx).await?;
    if existing > 0 { warnings.push(format!("An opening balance of {existing} was already recorded for {cname}. This adds {} more.", b.qty)); }
    if b.qty >= 500 { warnings.push(format!("{} containers is an unusually large opening balance.", b.qty)); }
    need_confirm(b.confirm, "Please confirm the opening balance", &warnings)?;
    let id = ledger_row(&mut tx, b.customer_id, product.id, "OPENING", b.qty, date, &note, &u).await?;
    audit::log(&mut *tx, Some(&u), "jars.opening", "jar_ledger", Some(id.to_string()), json!({ "customer_id": b.customer_id, "product_id": product.id, "qty": b.qty, "as_of": date })).await;
    let effects = vec![customer_fx(&cname, &product, b.qty, before + b.qty)];
    finish(tx, dry, Done { status: StatusCode::CREATED, body: json!({ "id": id, "customer_id": b.customer_id, "product_id": product.id, "qty": b.qty, "holding": before + b.qty }), effects, warnings }).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueReq {
    customer_id: Uuid, product_id: Uuid,
    #[serde(deserialize_with = "de_int")] qty: i64,
    note: Option<String>,
    location_id: Option<Uuid>,
    #[serde(default)] confirm: bool,
}

/// Counter sale of filled containers: FEFO from FG_READY, reservation-aware (`stock::allocate_free_fefo`).
async fn issue(State(s): State<AppState>, u: AuthUser, uri: Uri, ApiJson(b): ApiJson<IssueReq>) -> AppResult<(StatusCode, Json<Value>)> {
    count_field("qty", b.qty)?;
    let note = opt_note(&b.note)?;
    let dry = is_dry_run(&uri);
    let mut tx = s.db.begin().await?;
    let product = load_returnable(&mut tx, b.product_id).await?;
    let loc = util::resolve_location(&mut tx, &u, b.location_id).await?;
    let cname = load_active_customer(&mut tx, b.customer_id).await?;
    let before = lock_holding(&mut tx, b.customer_id, product.id).await?;
    let today = util::plant_today(&mut tx).await?;
    let mut warnings = vec![];
    if before > 0 {
        let cur = balances(&mut tx, Some(product.id), Some(b.customer_id), None, false, today, s.settings.get().jar_overdue_days).await?;
        if let Some(bal) = cur.first().filter(|x| x.overdue) {
            warnings.push(format!("{cname} already holds {} containers, the oldest for {} days (limit {}).", bal.holding, bal.days_outstanding, s.settings.get().jar_overdue_days));
        }
    }
    if b.qty >= 100 { warnings.push(format!("{} containers in one counter sale is unusually large.", b.qty)); }
    need_confirm(b.confirm, "Please confirm this counter issue", &warnings)?;
    let alloc = stock::allocate_free_fefo_at(&mut tx, loc, product.id, b.qty).await?;
    let id = ledger_row(&mut tx, b.customer_id, product.id, "ISSUE_FILLED", b.qty, today, &note, &u).await?;
    let moves: Vec<Move> = alloc.iter().map(|(batch, n)| {
        Move::new(ItemType::Product, product.id, "FG_READY", *batch, Decimal::from(-*n), "JAR_ISSUE").at(loc)
            .reference("JAR_LEDGER", id).by(Some(u.id)).note(format!("counter issue to {cname}"))
    }).collect();
    stock::post_all(&mut tx, moves).await?;
    let mut batches = vec![];
    for (batch, n) in &alloc {
        let no: String = sqlx::query_scalar("SELECT batch_no FROM production_batches WHERE id = $1").bind(batch).fetch_one(&mut *tx).await?;
        batches.push(json!({ "batch_id": batch, "batch_no": no, "qty": n }));
    }
    audit::log(&mut *tx, Some(&u), "jars.issue", "jar_ledger", Some(id.to_string()), json!({ "customer_id": b.customer_id, "product_id": product.id, "qty": b.qty, "batches": batches })).await;
    let effects = vec![stock_fx(&mut tx, loc, &product, "FG_READY", -b.qty).await?, customer_fx(&cname, &product, b.qty, before + b.qty)];
    finish(tx, dry, Done { status: StatusCode::CREATED, body: json!({ "id": id, "customer_id": b.customer_id, "product_id": product.id, "qty": b.qty, "holding": before + b.qty, "batches": batches }), effects, warnings }).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiveEmptiesReq {
    customer_id: Uuid, product_id: Uuid,
    #[serde(default, deserialize_with = "de_int")] qty_good: i64,
    #[serde(default, deserialize_with = "de_int")] qty_damaged: i64,
    note: Option<String>,
    location_id: Option<Uuid>,
    #[serde(default)] confirm: bool,
}

/// Empties coming back from a customer at the counter: good -> JAR_EMPTY, damaged -> SCRAP_YARD (+ DAMAGED_RETURN event).
async fn receive_empties(State(s): State<AppState>, u: AuthUser, uri: Uri, ApiJson(b): ApiJson<ReceiveEmptiesReq>) -> AppResult<(StatusCode, Json<Value>)> {
    count_or_zero("qty_good", b.qty_good)?;
    count_or_zero("qty_damaged", b.qty_damaged)?;
    let total = b.qty_good + b.qty_damaged;
    if total < 1 { return Err(AppError::field("qty_good", "enter at least one returned container")); }
    let note = opt_note(&b.note)?;
    let dry = is_dry_run(&uri);
    let mut tx = s.db.begin().await?;
    let product = load_returnable(&mut tx, b.product_id).await?;
    let loc = util::resolve_location(&mut tx, &u, b.location_id).await?;
    let cname = load_active_customer(&mut tx, b.customer_id).await?;
    let before = lock_holding(&mut tx, b.customer_id, product.id).await?;
    if total > before {
        return Err(AppError::bad(format!("{cname} holds only {before} of this container; cannot receive {total}. Record the customer's opening balance first if the system did not know about these containers.")));
    }
    let today = util::plant_today(&mut tx).await?;
    let mut warnings = vec![];
    let dmg_pct = s.settings.get().damage_warn_pct;
    if b.qty_damaged > 0 && b.qty_damaged * 100 > dmg_pct * total {
        warnings.push(format!("{} of {} returned containers are damaged ({}%).", b.qty_damaged, total, b.qty_damaged * 100 / total));
    }
    need_confirm(b.confirm, "Please confirm the damaged return", &warnings)?;
    let mut moves = vec![];
    let mut last_id = None;
    if b.qty_good > 0 {
        let id = ledger_row(&mut tx, b.customer_id, product.id, "RETURN_EMPTY", -b.qty_good, today, &note, &u).await?;
        last_id = Some(id);
        moves.push(Move::new(ItemType::Product, product.id, "JAR_EMPTY", NIL_LOT, Decimal::from(b.qty_good), "JAR_RECEIVE").at(loc).reference("JAR_LEDGER", id).by(Some(u.id)).note(format!("empties from {cname}")));
    }
    let mut event_id = None;
    if b.qty_damaged > 0 {
        let id = ledger_row(&mut tx, b.customer_id, product.id, "DAMAGED", -b.qty_damaged, today, &note, &u).await?;
        last_id = Some(id);
        moves.push(Move::new(ItemType::Product, product.id, "SCRAP_YARD", NIL_LOT, Decimal::from(b.qty_damaged), "JAR_DAMAGED").at(loc).reference("JAR_LEDGER", id).by(Some(u.id)).note(format!("damaged return from {cname}")));
        event_id = Some(fleet_event(&mut tx, product.id, "DESTROYED", b.qty_damaged, Some("DAMAGED_RETURN"), Some("CUSTOMER"), Some(b.customer_id), None, &note, &u, loc).await?);
    }
    stock::post_all(&mut tx, moves).await?;
    audit::log(&mut *tx, Some(&u), "jars.receive_empties", "jar_ledger", last_id.map(|i| i.to_string()),
               json!({ "customer_id": b.customer_id, "product_id": product.id, "good": b.qty_good, "damaged": b.qty_damaged })).await;
    let mut effects = vec![];
    if b.qty_good > 0 { effects.push(stock_fx(&mut tx, loc, &product, "JAR_EMPTY", b.qty_good).await?); }
    if b.qty_damaged > 0 { effects.push(stock_fx(&mut tx, loc, &product, "SCRAP_YARD", b.qty_damaged).await?); }
    effects.push(customer_fx(&cname, &product, -total, before - total));
    finish(tx, dry, Done { status: StatusCode::CREATED,
        body: json!({ "customer_id": b.customer_id, "product_id": product.id, "qty_good": b.qty_good, "qty_damaged": b.qty_damaged, "holding": before - total, "event_id": event_id }),
        effects, warnings }).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiveNewReq {
    product_id: Uuid,
    #[serde(deserialize_with = "de_int")] qty: i64,
    supplier_id: Option<Uuid>,
    note: Option<String>,
    location_id: Option<Uuid>,
    #[serde(default)] confirm: bool,
}

/// Newly bought containers join the fleet (JAR_EMPTY, ledger JAR_PURCHASE, event NEW_PURCHASED).
async fn receive_new(State(s): State<AppState>, u: AuthUser, uri: Uri, ApiJson(b): ApiJson<ReceiveNewReq>) -> AppResult<(StatusCode, Json<Value>)> {
    count_field("qty", b.qty)?;
    let note = opt_note(&b.note)?;
    let dry = is_dry_run(&uri);
    let mut tx = s.db.begin().await?;
    let product = load_returnable(&mut tx, b.product_id).await?;
    let loc = util::resolve_location(&mut tx, &u, b.location_id).await?;
    if let Some(sup) = b.supplier_id {
        let ok: Option<bool> = sqlx::query_scalar("SELECT is_active FROM suppliers WHERE id = $1").bind(sup).fetch_optional(&mut *tx).await?;
        match ok { None => return Err(AppError::field("supplier_id", "unknown supplier")), Some(false) => return Err(AppError::field("supplier_id", "supplier is inactive")), _ => {} }
    }
    let fleet = fleet_counts(&mut tx, product.id, None).await?.total();
    let mut warnings = vec![];
    if b.qty >= 1000 { warnings.push(format!("{} new containers in one receipt is unusually large.", b.qty)); }
    if fleet > 0 && b.qty * 100 > fleet * 25 { warnings.push(format!("This is more than 25% of the current fleet ({fleet}).")); }
    need_confirm(b.confirm, "Please confirm the new containers", &warnings)?;
    let event = fleet_event(&mut tx, product.id, "NEW_PURCHASED", b.qty, None, None, None, b.supplier_id, &note, &u, loc).await?;
    stock::post_all(&mut tx, vec![Move::new(ItemType::Product, product.id, "JAR_EMPTY", NIL_LOT, Decimal::from(b.qty), "JAR_PURCHASE").at(loc).reference("JAR_EVENT", event).by(Some(u.id)).note(note.clone())]).await?;
    audit::log(&mut *tx, Some(&u), "jars.receive_new", "jar_fleet_events", Some(event.to_string()), json!({ "product_id": product.id, "qty": b.qty, "supplier_id": b.supplier_id })).await;
    let effects = vec![stock_fx(&mut tx, loc, &product, "JAR_EMPTY", b.qty).await?];
    finish(tx, dry, Done { status: StatusCode::CREATED, body: json!({ "event_id": event, "product_id": product.id, "qty": b.qty }), effects, warnings }).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WashReq {
    product_id: Uuid,
    #[serde(deserialize_with = "de_int")] qty: i64,
    #[serde(default, deserialize_with = "de_opt_int")] rejected: Option<i64>,
    note: Option<String>,
    location_id: Option<Uuid>,
    #[serde(default)] confirm: bool,
}

/// Wash line: `qty` containers come out clean (JAR_EMPTY -> JAR_CLEAN); `rejected` additional ones fail inspection and go to
/// SCRAP_YARD (DESTROYED/WASH_REJECT). JAR_EMPTY is reduced by qty + rejected.
async fn wash(State(s): State<AppState>, u: AuthUser, uri: Uri, ApiJson(b): ApiJson<WashReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let rejected = b.rejected.unwrap_or(0);
    count_or_zero("qty", b.qty)?;
    count_or_zero("rejected", rejected)?;
    if b.qty + rejected < 1 { return Err(AppError::field("qty", "enter at least one washed container")); }
    let total = b.qty + rejected;
    let note = opt_note(&b.note)?;
    let dry = is_dry_run(&uri);
    let mut tx = s.db.begin().await?;
    let product = load_returnable(&mut tx, b.product_id).await?;
    let loc = util::resolve_location(&mut tx, &u, b.location_id).await?;
    let mut warnings = vec![];
    if rejected > 0 && rejected * 100 > s.settings.get().damage_warn_pct * total {
        warnings.push(format!("{rejected} of {total} washed containers were rejected ({}%).", rejected * 100 / total));
    }
    need_confirm(b.confirm, "Please confirm the wash result", &warnings)?;
    stock::consume_fefo_at(&mut tx, loc, ItemType::Product, product.id, "JAR_EMPTY", Decimal::from(total), "JAR_WASH", None, None, Some(u.id), "wash line").await?;
    let mut credits = vec![];
    if b.qty > 0 { credits.push(Move::new(ItemType::Product, product.id, "JAR_CLEAN", NIL_LOT, Decimal::from(b.qty), "JAR_WASH").at(loc).by(Some(u.id)).note("washed")); }
    if rejected > 0 { credits.push(Move::new(ItemType::Product, product.id, "SCRAP_YARD", NIL_LOT, Decimal::from(rejected), "JAR_WASH_REJECT").at(loc).by(Some(u.id)).note("wash reject")); }
    stock::post_all(&mut tx, credits).await?;
    let washed_event = if b.qty > 0 { Some(fleet_event(&mut tx, product.id, "WASHED", b.qty, None, None, None, None, &note, &u, loc).await?) } else { None };
    let reject_event = if rejected > 0 { Some(fleet_event(&mut tx, product.id, "DESTROYED", rejected, Some("WASH_REJECT"), Some("PLANT_EMPTY"), None, None, &note, &u, loc).await?) } else { None };
    audit::log(&mut *tx, Some(&u), "jars.wash", "jar_fleet_events", washed_event.or(reject_event).map(|i| i.to_string()), json!({ "product_id": product.id, "washed": b.qty, "rejected": rejected })).await;
    let mut effects = vec![stock_fx(&mut tx, loc, &product, "JAR_EMPTY", -total).await?];
    if b.qty > 0 { effects.push(stock_fx(&mut tx, loc, &product, "JAR_CLEAN", b.qty).await?); }
    if rejected > 0 { effects.push(stock_fx(&mut tx, loc, &product, "SCRAP_YARD", rejected).await?); }
    finish(tx, dry, Done { status: StatusCode::CREATED, body: json!({ "product_id": product.id, "washed": b.qty, "rejected": rejected, "washed_event_id": washed_event, "reject_event_id": reject_event }), effects, warnings }).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DestroyReq {
    product_id: Uuid,
    #[serde(deserialize_with = "de_int")] qty: i64,
    location: String,
    customer_id: Option<Uuid>,
    reason_code: String,
    note: String,
    location_id: Option<Uuid>,
    #[serde(default)] confirm: bool,
}

/// Permanently remove containers from the fleet (irreversible: always needs `confirm:true`).
async fn destroy(State(s): State<AppState>, u: AuthUser, uri: Uri, ApiJson(b): ApiJson<DestroyReq>) -> AppResult<(StatusCode, Json<Value>)> {
    count_field("qty", b.qty)?;
    if !DESTROY_LOCATIONS.contains(&b.location.as_str()) { return Err(AppError::field("location", "must be PLANT_EMPTY, PLANT_CLEAN or CUSTOMER")); }
    if !DESTROY_REASONS.contains(&b.reason_code.as_str()) { return Err(AppError::field("reason_code", "unknown reason code")); }
    let note = validate::required_text("note", &b.note, 500)?;
    if note.chars().count() < 5 { return Err(AppError::field("note", "must be at least 5 characters")); }
    match (b.location.as_str(), b.customer_id) {
        ("CUSTOMER", None) => return Err(AppError::field("customer_id", "is required when the containers are at a customer")),
        ("CUSTOMER", Some(_)) => {}
        (_, Some(_)) => return Err(AppError::field("customer_id", "is only allowed for location CUSTOMER")),
        _ => {}
    }
    let dry = is_dry_run(&uri);
    let mut tx = s.db.begin().await?;
    let product = load_returnable(&mut tx, b.product_id).await?;
    let loc = util::resolve_location(&mut tx, &u, b.location_id).await?;
    let mut effects = vec![];
    let mut cust_name = None;
    let event: Uuid;
    if b.location == "CUSTOMER" {
        let cid = b.customer_id.unwrap();
        let cname = load_customer(&mut tx, cid).await?.0;
        let before = lock_holding(&mut tx, cid, product.id).await?;
        if b.qty > before { return Err(AppError::bad(format!("{cname} holds only {before} of this container; cannot write off {}", b.qty))); }
        need_confirm(b.confirm, "Please confirm this write-off", &[format!("{} containers held by {cname} will be written off as {} and removed from the fleet. This cannot be undone.", b.qty, b.reason_code)])?;
        let today = util::plant_today(&mut tx).await?;
        ledger_row(&mut tx, cid, product.id, "WRITE_OFF", -b.qty, today, &format!("{}: {}", b.reason_code, note), &u).await?;
        event = fleet_event(&mut tx, product.id, "DESTROYED", b.qty, Some(&b.reason_code), Some(&b.location), b.customer_id, None, &note, &u, loc).await?;
        effects.push(customer_fx(&cname, &product, -b.qty, before - b.qty));
        cust_name = Some(cname);
    } else {
        let (zone, label): (&'static str, &str) = if b.location == "PLANT_EMPTY" { ("JAR_EMPTY", "empty") } else { ("JAR_CLEAN", "clean") };
        let have = zone_qty(&mut tx, loc, product.id, zone).await?;
        need_confirm(b.confirm, "Please confirm this write-off", &[format!("{} {label} containers will be moved to the scrap yard and removed from the fleet (stock {have} -> {}). This cannot be undone.", b.qty, have - b.qty)])?;
        event = fleet_event(&mut tx, product.id, "DESTROYED", b.qty, Some(&b.reason_code), Some(&b.location), None, None, &note, &u, loc).await?;
        stock::consume_fefo_at(&mut tx, loc, ItemType::Product, product.id, zone, Decimal::from(b.qty), "JAR_DESTROY", Some("JAR_EVENT"), Some(event), Some(u.id), &format!("{}: {}", b.reason_code, note)).await?;
        stock::post_all(&mut tx, vec![Move::new(ItemType::Product, product.id, "SCRAP_YARD", NIL_LOT, Decimal::from(b.qty), "JAR_DESTROY").at(loc).reference("JAR_EVENT", event).by(Some(u.id)).note(format!("{}: {}", b.reason_code, note))]).await?;
        effects.push(stock_fx(&mut tx, loc, &product, zone, -b.qty).await?);
        effects.push(stock_fx(&mut tx, loc, &product, "SCRAP_YARD", b.qty).await?);
    }
    audit::log(&mut *tx, Some(&u), "jars.destroy", "jar_fleet_events", Some(event.to_string()),
               json!({ "product_id": product.id, "qty": b.qty, "location": b.location, "reason_code": b.reason_code, "customer_id": b.customer_id, "customer": cust_name, "note": note })).await;
    let warnings = vec![];
    finish(tx, dry, Done { status: StatusCode::CREATED, body: json!({ "event_id": event, "product_id": product.id, "qty": b.qty, "location": b.location, "reason_code": b.reason_code }), effects, warnings }).await
}
