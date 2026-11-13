//! Admin: locations (plant + godowns), API_REFERENCE 0.2-F. The system TRANSIT location is never listed and cannot be addressed.
//! Soft delete only. MAIN is protected. A godown that still holds stock or has an IN_TRANSIT transfer cannot be deactivated.
use super::common::{audit, ApiJson, ApiPath, ApiQuery, ClientInfo};
use super::master::{code, dup, lock_row, ListQ};
use crate::{auth::AuthUser, error::{AppError, AppResult}, state::AppState, stock::MAIN_LOCATION, validate};
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/locations", get(list).post(create))
        .route("/locations/{id}", get(get_one).put(update).delete(remove))
}

#[derive(Serialize, sqlx::FromRow, Clone)]
struct Location {
    id: Uuid, code: String, name: String, kind: String, address: String, city: String, contact_person: String, phone: String,
    is_production_site: bool, is_active: bool, created_at: DateTime<Utc>,
}
const SEL: &str = "SELECT t.id, t.code, t.name, t.kind, t.address, t.city, t.contact_person, t.phone, t.is_production_site, t.is_active, t.created_at FROM locations t";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocationIn {
    code: Option<String>, name: Option<String>, kind: Option<String>, address: Option<String>, city: Option<String>,
    contact_person: Option<String>, phone: Option<String>, is_production_site: Option<bool>, is_active: Option<bool>,
}

fn phone(s: &str) -> AppResult<String> {
    let t = validate::text("phone", s, 40)?;
    if !t.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | ' ' | '(' | ')' | '/' | '.')) { return Err(AppError::field("phone", "contains invalid characters")); }
    Ok(t)
}

fn validate_in(b: LocationIn, old: Option<&Location>) -> AppResult<Location> {
    let code_v = code("code", &b.code.or_else(|| old.map(|o| o.code.clone())).unwrap_or_default())?.to_ascii_uppercase();
    let name = validate::required_text("name", &b.name.or_else(|| old.map(|o| o.name.clone())).unwrap_or_default(), 120)?;
    let kind = b.kind.or_else(|| old.map(|o| o.kind.clone())).unwrap_or_else(|| "GODOWN".to_string());
    if kind != "PLANT" && kind != "GODOWN" { return Err(AppError::field("kind", "must be PLANT or GODOWN")); }
    let address = validate::text("address", &b.address.or_else(|| old.map(|o| o.address.clone())).unwrap_or_default(), 500)?;
    let city = validate::text("city", &b.city.or_else(|| old.map(|o| o.city.clone())).unwrap_or_default(), 120)?;
    let contact_person = validate::text("contact_person", &b.contact_person.or_else(|| old.map(|o| o.contact_person.clone())).unwrap_or_default(), 120)?;
    let phone_v = phone(&b.phone.or_else(|| old.map(|o| o.phone.clone())).unwrap_or_default())?;
    let is_production_site = b.is_production_site.or_else(|| old.map(|o| o.is_production_site)).unwrap_or(kind == "PLANT");
    let is_active = b.is_active.or_else(|| old.map(|o| o.is_active)).unwrap_or(true);
    Ok(Location {
        id: old.map(|o| o.id).unwrap_or_else(Uuid::nil), code: code_v, name, kind, address, city, contact_person, phone: phone_v,
        is_production_site, is_active, created_at: old.map(|o| o.created_at).unwrap_or_else(Utc::now),
    })
}

async fn fetch(conn: &mut PgConnection, id: Uuid) -> AppResult<Location> {
    let l: Location = sqlx::query_as(&format!("{SEL} WHERE t.id = $1")).bind(id).fetch_optional(&mut *conn).await?.ok_or_else(|| AppError::not_found("Location"))?;
    if l.kind == "TRANSIT" { return Err(AppError::not_found("Location")); }   // system location: not addressable
    Ok(l)
}

async fn list(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQ>) -> AppResult<Json<Vec<Location>>> {
    let pat = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(validate::like_pattern(&validate::text("q", s, 100)?)),
        None => None,
    };
    let rows = sqlx::query_as::<_, Location>(&format!(
        "{SEL} WHERE t.kind <> 'TRANSIT' AND ($1::text IS NULL OR t.code ILIKE $1 OR t.name ILIKE $1 OR t.city ILIKE $1) AND ($2::bool IS NULL OR t.is_active = $2)
         ORDER BY (t.id = $5) DESC, lower(t.name), t.id LIMIT $3 OFFSET $4"))
        .bind(pat).bind(q.active).bind(q.limit.unwrap_or(500).clamp(1, 500)).bind(q.offset.unwrap_or(0).max(0)).bind(MAIN_LOCATION)
        .fetch_all(&st.db).await?;
    Ok(Json(rows))
}

async fn get_one(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<Location>> {
    Ok(Json(fetch(&mut *st.db.acquire().await?, id).await?))
}

async fn create(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiJson(b): ApiJson<LocationIn>) -> AppResult<(StatusCode, Json<Location>)> {
    let v = validate_in(b, None)?;
    let mut tx = st.db.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO locations (code, name, kind, address, city, contact_person, phone, is_production_site, is_active) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id")
        .bind(&v.code).bind(&v.name).bind(&v.kind).bind(&v.address).bind(&v.city).bind(&v.contact_person).bind(&v.phone).bind(v.is_production_site).bind(v.is_active)
        .fetch_one(&mut *tx).await.map_err(|e| dup(e, "A location with this code already exists"))?;
    let row = fetch(&mut tx, id).await?;
    audit(&mut tx, &actor, &ci, "LOCATION_CREATE", "location", Some(id.to_string()), json!({ "after": row })).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// 409 IN_USE when the godown still holds stock (qty > 0) or takes part in an IN_TRANSIT transfer.
async fn ensure_unused(conn: &mut PgConnection, id: Uuid) -> AppResult<()> {
    let stock_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_balances WHERE location_id = $1 AND qty > 0").bind(id).fetch_one(&mut *conn).await?;
    let transfers: i64 = sqlx::query_scalar("SELECT count(*) FROM godown_transfers WHERE status = 'IN_TRANSIT' AND (from_location_id = $1 OR to_location_id = $1)")
        .bind(id).fetch_one(&mut *conn).await?;
    if stock_rows > 0 || transfers > 0 {
        return Err(AppError::conflict_with("IN_USE",
            "This godown cannot be deactivated while it holds stock or has transfers in transit. Move or receive the stock first.",
            json!({ "stock_rows": stock_rows, "transfers_in_transit": transfers })));
    }
    Ok(())
}

async fn update(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<LocationIn>) -> AppResult<Json<Location>> {
    let mut tx = st.db.begin().await?;
    lock_row(&mut tx, "locations", id, "Location").await?;
    let old = fetch(&mut tx, id).await?;
    let v = validate_in(b, Some(&old))?;
    if id == MAIN_LOCATION {
        if !v.is_active { return Err(AppError::conflict("PROTECTED", "The main plant location cannot be deactivated")); }
        if v.kind != "PLANT" || !v.is_production_site { return Err(AppError::conflict("PROTECTED", "The main plant location must stay a production PLANT")); }
        if v.code != old.code { return Err(AppError::conflict("PROTECTED", "The code of the main plant location cannot be changed")); }
    }
    if old.is_active && !v.is_active { ensure_unused(&mut tx, id).await?; }
    sqlx::query("UPDATE locations SET code=$2, name=$3, kind=$4, address=$5, city=$6, contact_person=$7, phone=$8, is_production_site=$9, is_active=$10 WHERE id=$1")
        .bind(id).bind(&v.code).bind(&v.name).bind(&v.kind).bind(&v.address).bind(&v.city).bind(&v.contact_person).bind(&v.phone).bind(v.is_production_site).bind(v.is_active)
        .execute(&mut *tx).await.map_err(|e| dup(e, "A location with this code already exists"))?;
    let row = fetch(&mut tx, id).await?;
    let action = if old.is_active && !row.is_active { "LOCATION_DEACTIVATE" } else { "LOCATION_UPDATE" };
    audit(&mut tx, &actor, &ci, action, "location", Some(id.to_string()), json!({ "before": old, "after": row })).await?;
    tx.commit().await?;
    Ok(Json(row))
}

async fn remove(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    let mut tx = st.db.begin().await?;
    lock_row(&mut tx, "locations", id, "Location").await?;
    let old = fetch(&mut tx, id).await?;
    if id == MAIN_LOCATION { return Err(AppError::conflict("PROTECTED", "The main plant location cannot be deactivated")); }
    if old.is_active { ensure_unused(&mut tx, id).await?; }
    sqlx::query("UPDATE locations SET is_active = false WHERE id = $1").bind(id).execute(&mut *tx).await?;
    audit(&mut tx, &actor, &ci, "LOCATION_DEACTIVATE", "location", Some(id.to_string()), json!({ "code": old.code, "is_active": false })).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
