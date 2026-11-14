//! Read side of per-customer deliveries recorded at trip settlement (`trip_customer_deliveries`): a flat list and a per-customer summary.
//! Godown-scoped by the trip's source godown. Quantities only - no prices, tax or payments.
use super::common::*;
use crate::{auth::AuthUser, error::AppResult, state::AppState, validate};
use axum::{extract::State, Json};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Deserialize, Default)]
pub struct DelQ {
    customer_id: Option<Uuid>,
    product_id: Option<Uuid>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    location_id: Option<Uuid>,
    limit: Option<i64>,
    offset: Option<i64>,
}

const FROM_WHERE: &str =
    "FROM trip_customer_deliveries d
     JOIN loading_trips t ON t.id = d.trip_id
     JOIN customers c ON c.id = d.customer_id
     JOIN products p ON p.id = d.product_id
     JOIN locations l ON l.id = t.location_id
     WHERE t.status = 'SETTLED'
       AND ($1::uuid IS NULL OR d.customer_id = $1)
       AND ($2::uuid IS NULL OR d.product_id = $2)
       AND ($3::date IS NULL OR ((t.settled_at AT TIME ZONE 'UTC') + make_interval(mins => $5))::date >= $3)
       AND ($4::date IS NULL OR ((t.settled_at AT TIME ZONE 'UTC') + make_interval(mins => $5))::date <= $4)
       AND ($6::uuid[] IS NULL OR t.location_id = ANY($6))";

fn scope_of(user: &AuthUser, location: Option<Uuid>) -> AppResult<Option<Vec<Uuid>>> {
    Ok(match location {
        Some(l) => { user.require_location(l)?; Some(vec![l]) }
        None => location_scope(user),
    })
}

#[derive(FromRow, Serialize)]
struct Row {
    trip_id: Uuid, trip_no: String, settled_at: Option<DateTime<Utc>>, customer_id: Uuid, customer_name: String, customer_code: String,
    product_id: Uuid, product_name: String, crates: i32, pallets: Option<i32>, loose_crates: Option<i32>, location_id: Uuid, location_name: String,
}

/// `GET /deliveries?customer_id=&product_id=&from=&to=&location_id=&limit=&offset=` -> `{items,total}`, newest settlement first.
pub async fn list(State(st): State<AppState>, user: AuthUser, Q(q): Q<DelQ>) -> AppResult<Json<Value>> {
    let scope = scope_of(&user, q.location_id)?;
    let off = st.settings.get().utc_offset_minutes as i32;
    let page = validate::PageQuery { limit: q.limit, offset: q.offset };
    let rows = sqlx::query_as::<_, Row>(&format!(
        "SELECT t.id AS trip_id, t.trip_no, t.settled_at, d.customer_id, c.name AS customer_name, c.code AS customer_code, d.product_id, p.name AS product_name, d.crates,
                CASE WHEN p.crates_per_pallet IS NULL THEN NULL ELSE d.crates / p.crates_per_pallet END AS pallets,
                CASE WHEN p.crates_per_pallet IS NULL THEN NULL ELSE d.crates % p.crates_per_pallet END AS loose_crates,
                t.location_id, l.name AS location_name
         {FROM_WHERE} ORDER BY t.settled_at DESC, t.id ASC, c.name ASC, p.name ASC, d.id ASC LIMIT $7 OFFSET $8"))
        .bind(q.customer_id).bind(q.product_id).bind(q.from).bind(q.to).bind(off).bind(&scope).bind(page.limit()).bind(page.offset())
        .fetch_all(&st.db).await?;
    let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) {FROM_WHERE}"))
        .bind(q.customer_id).bind(q.product_id).bind(q.from).bind(q.to).bind(off).bind(&scope).fetch_one(&st.db).await?;
    Ok(Json(json!({ "items": rows, "total": total })))
}

#[derive(FromRow)]
struct CustRow { customer_id: Uuid, customer_name: String, customer_code: String, crates: i64, trips: i64 }
#[derive(FromRow)]
struct ProdRow { customer_id: Uuid, product_id: Uuid, product_name: String, crates: i64, crates_per_pallet: Option<i32> }

/// `GET /deliveries/summary?from=&to=` (+ optional `customer_id`, `product_id`, `location_id`, `limit`, `offset` over customers)
/// -> `{items:[{customer_id,customer_name,customer_code,total_crates,trips,products:[{product_id,product_name,crates,pallets,loose_crates}]}],total,totals:{customers,crates}}`.
pub async fn summary(State(st): State<AppState>, user: AuthUser, Q(q): Q<DelQ>) -> AppResult<Json<Value>> {
    let scope = scope_of(&user, q.location_id)?;
    let off = st.settings.get().utc_offset_minutes as i32;
    let page = validate::PageQuery { limit: q.limit, offset: q.offset };
    let (customers, crates): (i64, i64) = sqlx::query_as(&format!("SELECT COUNT(DISTINCT d.customer_id), COALESCE(SUM(d.crates), 0)::bigint {FROM_WHERE}"))
        .bind(q.customer_id).bind(q.product_id).bind(q.from).bind(q.to).bind(off).bind(&scope).fetch_one(&st.db).await?;
    let custs = sqlx::query_as::<_, CustRow>(&format!(
        "SELECT d.customer_id, c.name AS customer_name, c.code AS customer_code, SUM(d.crates)::bigint AS crates, COUNT(DISTINCT d.trip_id)::bigint AS trips
         {FROM_WHERE} GROUP BY d.customer_id, c.name, c.code ORDER BY c.name ASC, d.customer_id ASC LIMIT $7 OFFSET $8"))
        .bind(q.customer_id).bind(q.product_id).bind(q.from).bind(q.to).bind(off).bind(&scope).bind(page.limit()).bind(page.offset())
        .fetch_all(&st.db).await?;
    let ids: Vec<Uuid> = custs.iter().map(|c| c.customer_id).collect();
    let prods = sqlx::query_as::<_, ProdRow>(&format!(
        "SELECT d.customer_id, d.product_id, p.name AS product_name, SUM(d.crates)::bigint AS crates, p.crates_per_pallet
         {FROM_WHERE} AND d.customer_id = ANY($7) GROUP BY d.customer_id, d.product_id, p.name, p.crates_per_pallet ORDER BY p.name ASC, d.product_id ASC"))
        .bind(q.customer_id).bind(q.product_id).bind(q.from).bind(q.to).bind(off).bind(&scope).bind(&ids).fetch_all(&st.db).await?;
    let mut by_cust: HashMap<Uuid, Vec<Value>> = HashMap::new();
    for p in prods {
        let cpp = p.crates_per_pallet.map(|c| c as i64).filter(|c| *c > 0);
        by_cust.entry(p.customer_id).or_default().push(json!({
            "product_id": p.product_id, "product_name": p.product_name, "crates": p.crates,
            "pallets": cpp.map(|c| p.crates / c), "loose_crates": cpp.map(|c| p.crates % c),
        }));
    }
    let items: Vec<Value> = custs.into_iter().map(|c| json!({
        "customer_id": c.customer_id, "customer_name": c.customer_name, "customer_code": c.customer_code, "total_crates": c.crates, "trips": c.trips,
        "products": by_cust.remove(&c.customer_id).unwrap_or_default(),
    })).collect();
    Ok(Json(json!({ "from": q.from, "to": q.to, "items": items, "total": customers, "totals": { "customers": customers, "crates": crates } })))
}
