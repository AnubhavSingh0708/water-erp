//! Admin: master data CRUD – suppliers, raw materials, customers, routes, vehicles, equipment, maintenance schedules.
//! GET list (incl. inactive, `?q=`), POST, GET/{id}, PUT/{id} (partial: absent field = unchanged, `null` clears a nullable field),
//! DELETE/{id} = soft delete (`is_active = false`, never cascades). Every mutation is audited inside its transaction.
use super::common::{audit, ApiJson, ApiPath, ApiQuery, ClientInfo};
use crate::{auth::AuthUser, error::{AppError, AppResult}, state::AppState, validate};
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;
use sqlx::{postgres::PgRow, FromRow, PgConnection, PgPool};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/suppliers", get(suppliers_list).post(supplier_create))
        .route("/suppliers/{id}", get(supplier_get).put(supplier_update).delete(supplier_delete))
        .route("/raw-materials", get(materials_list).post(material_create))
        .route("/raw-materials/{id}", get(material_get).put(material_update).delete(material_delete))
        .route("/customers", get(customers_list).post(customer_create))
        .route("/customers/{id}", get(customer_get).put(customer_update).delete(customer_delete))
        .route("/routes", get(routes_list).post(route_create))
        .route("/routes/{id}", get(route_get).put(route_update).delete(route_delete))
        .route("/vehicles", get(vehicles_list).post(vehicle_create))
        .route("/vehicles/{id}", get(vehicle_get).put(vehicle_update).delete(vehicle_delete))
        .route("/equipment", get(equipment_list).post(equipment_create))
        .route("/equipment/{id}", get(equipment_get).put(equipment_update).delete(equipment_delete))
        .route("/maintenance-schedules", get(schedules_list).post(schedule_create))
        .route("/maintenance-schedules/{id}", get(schedule_get).put(schedule_update).delete(schedule_delete))
}

// ---------------------------------------------------------------------------------------------------------------------
// Shared helpers (also used by admin_products)
// ---------------------------------------------------------------------------------------------------------------------

/// Distinguishes "field absent" (None) from "field: null" (Some(None)).
pub fn double_opt<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error> where D: Deserializer<'de>, T: Deserialize<'de> {
    Ok(Some(Option::<T>::deserialize(d)?))
}

#[derive(Deserialize)]
pub struct ListQ { pub q: Option<String>, pub active: Option<bool>, pub limit: Option<i64>, pub offset: Option<i64> }

pub async fn list_rows<T>(db: &PgPool, sel: &str, search: &str, order: &str, q: &ListQ) -> AppResult<Vec<T>>
where T: for<'r> FromRow<'r, PgRow> + Send + Unpin {
    let pat = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(validate::like_pattern(&validate::text("q", s, 100)?)),
        None => None,
    };
    // `sel`, `search` and `order` are compile-time constants of this crate, never user input
    let sql = format!("{sel} WHERE ($1::text IS NULL OR ({search})) AND ($2::bool IS NULL OR t.is_active = $2) ORDER BY {order} LIMIT $3 OFFSET $4");
    Ok(sqlx::query_as::<_, T>(&sql).bind(pat).bind(q.active)
        .bind(q.limit.unwrap_or(500).clamp(1, 500)).bind(q.offset.unwrap_or(0).max(0)).fetch_all(db).await?)
}

pub async fn get_row<T>(conn: &mut PgConnection, sel: &str, id: Uuid, what: &str) -> AppResult<T>
where T: for<'r> FromRow<'r, PgRow> + Send + Unpin {
    sqlx::query_as::<_, T>(&format!("{sel} WHERE t.id = $1")).bind(id).fetch_optional(&mut *conn).await?
        .ok_or_else(|| AppError::not_found(what))
}

/// Soft delete: `is_active = false` (idempotent), audited.
pub async fn soft_delete(st: &AppState, actor: &AuthUser, ci: &ClientInfo, table: &'static str, entity: &str, action: &str, id: Uuid, what: &str) -> AppResult<StatusCode> {
    let mut tx = st.db.begin().await?;
    let r = sqlx::query(&format!("UPDATE {table} SET is_active = false WHERE id = $1")).bind(id).execute(&mut *tx).await?;
    if r.rows_affected() == 0 { return Err(AppError::not_found(what)); }
    audit(&mut tx, actor, ci, action, entity, Some(id.to_string()), json!({ "is_active": false })).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Unique violation -> 409 DUPLICATE with a readable message (other errors use the shared mapping).
pub fn dup(e: sqlx::Error, msg: &str) -> AppError {
    if let Some(d) = e.as_database_error() { if d.code().as_deref() == Some("23505") { return AppError::conflict("DUPLICATE", msg); } }
    e.into()
}

/// Reference to another master row must exist and be active.
pub async fn require_active(conn: &mut PgConnection, table: &'static str, id: Uuid, field: &str) -> AppResult<()> {
    let r: Option<bool> = sqlx::query_scalar(&format!("SELECT is_active FROM {table} WHERE id = $1")).bind(id).fetch_optional(&mut *conn).await?;
    match r { None => Err(AppError::field(field, "does not exist")), Some(false) => Err(AppError::field(field, "refers to an inactive record")), Some(true) => Ok(()) }
}

/// Business code: 1..=40 chars of `[A-Za-z0-9._/-]`.
pub fn code(field: &str, s: &str) -> AppResult<String> {
    let t = s.trim();
    if t.is_empty() { return Err(AppError::field(field, "is required")); }
    if t.chars().count() > 40 || !t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-')) {
        return Err(AppError::field(field, "must be at most 40 characters: letters, digits, '.', '_', '/' or '-'"));
    }
    Ok(t.to_string())
}

pub fn int_range(field: &str, v: i64, lo: i64, hi: i64) -> AppResult<()> {
    if v < lo || v > hi { Err(AppError::field(field, &format!("must be between {lo} and {hi}"))) } else { Ok(()) }
}

pub fn date_sane(field: &str, d: NaiveDate) -> AppResult<()> {
    if d < NaiveDate::from_ymd_opt(2000, 1, 1).unwrap() || d > NaiveDate::from_ymd_opt(2100, 12, 31).unwrap() {
        Err(AppError::field(field, "must be a date between 2000 and 2100"))
    } else { Ok(()) }
}

/// Max `digits` decimal places, 0 <= v <= 1e9.
pub fn dec_non_negative(field: &str, d: Decimal, dp: u32) -> AppResult<()> {
    if d < Decimal::ZERO { return Err(AppError::field(field, "must not be negative")); }
    if d > Decimal::from(validate::MAX_QTY) { return Err(AppError::field(field, "is too large")); }
    if d.scale() > dp { return Err(AppError::field(field, &format!("has too many decimal places (max {dp})"))); }
    Ok(())
}
pub fn dec_positive(field: &str, d: Decimal, dp: u32) -> AppResult<()> {
    if d <= Decimal::ZERO { return Err(AppError::field(field, "must be greater than 0")); }
    dec_non_negative(field, d, dp)
}

fn req<T>(field: &str, v: Option<T>) -> AppResult<T> { v.ok_or_else(|| AppError::field(field, "is required")) }

fn phone(field: &str, s: &str) -> AppResult<String> {
    let t = validate::text(field, s, 40)?;
    if !t.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | ' ' | '(' | ')' | '/' | '.')) { return Err(AppError::field(field, "contains invalid characters")); }
    Ok(t)
}

// ---------------------------------------------------------------------------------------------------------------------
// Suppliers
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Serialize, FromRow)]
struct Supplier { id: Uuid, name: String, contact_person: String, phone: String, email: String, address: String, lead_time_days: i32, is_active: bool, created_at: DateTime<Utc> }
const SUPPLIER_SEL: &str = "SELECT t.id, t.name, t.contact_person, t.phone, t.email, t.address, t.lead_time_days, t.is_active, t.created_at FROM suppliers t";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupplierIn { name: Option<String>, contact_person: Option<String>, phone: Option<String>, email: Option<String>, address: Option<String>, lead_time_days: Option<i32>, is_active: Option<bool> }

struct SupplierV { name: String, contact_person: String, phone: String, email: String, address: String, lead_time_days: i32, is_active: bool }
fn supplier_validate(b: SupplierIn, old: Option<&Supplier>) -> AppResult<SupplierV> {
    let name = validate::required_text("name", &b.name.or_else(|| old.map(|o| o.name.clone())).unwrap_or_default(), 120)?;
    let contact_person = validate::text("contact_person", &b.contact_person.or_else(|| old.map(|o| o.contact_person.clone())).unwrap_or_default(), 120)?;
    let phone_v = phone("phone", &b.phone.or_else(|| old.map(|o| o.phone.clone())).unwrap_or_default())?;
    let email = validate::text("email", &b.email.or_else(|| old.map(|o| o.email.clone())).unwrap_or_default(), 120)?;
    if !email.is_empty() && (!email.contains('@') || email.contains(' ') || email.starts_with('@') || email.ends_with('@')) { return Err(AppError::field("email", "is not a valid e-mail address")); }
    let address = validate::text("address", &b.address.or_else(|| old.map(|o| o.address.clone())).unwrap_or_default(), 500)?;
    let lead_time_days = b.lead_time_days.or_else(|| old.map(|o| o.lead_time_days)).unwrap_or(7);
    int_range("lead_time_days", lead_time_days as i64, 0, 365)?;
    Ok(SupplierV { name, contact_person, phone: phone_v, email, address, lead_time_days, is_active: b.is_active.or_else(|| old.map(|o| o.is_active)).unwrap_or(true) })
}

async fn suppliers_list(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQ>) -> AppResult<Json<Vec<Supplier>>> {
    Ok(Json(list_rows(&st.db, SUPPLIER_SEL, "t.name ILIKE $1 OR t.contact_person ILIKE $1 OR t.phone ILIKE $1", "lower(t.name), t.id", &q).await?))
}
async fn supplier_get(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<Supplier>> {
    Ok(Json(get_row(&mut *st.db.acquire().await?, SUPPLIER_SEL, id, "Supplier").await?))
}
async fn supplier_create(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiJson(b): ApiJson<SupplierIn>) -> AppResult<(StatusCode, Json<Supplier>)> {
    let v = supplier_validate(b, None)?;
    let mut tx = st.db.begin().await?;
    let id: Uuid = sqlx::query_scalar("INSERT INTO suppliers (name, contact_person, phone, email, address, lead_time_days, is_active) VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id")
        .bind(&v.name).bind(&v.contact_person).bind(&v.phone).bind(&v.email).bind(&v.address).bind(v.lead_time_days).bind(v.is_active)
        .fetch_one(&mut *tx).await?;
    let row: Supplier = get_row(&mut tx, SUPPLIER_SEL, id, "Supplier").await?;
    audit(&mut tx, &actor, &ci, "SUPPLIER_CREATE", "supplier", Some(id.to_string()), json!({ "after": row })).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}
async fn supplier_update(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<SupplierIn>) -> AppResult<Json<Supplier>> {
    let mut tx = st.db.begin().await?;
    lock_row(&mut tx, "suppliers", id, "Supplier").await?;
    let old: Supplier = get_row(&mut tx, SUPPLIER_SEL, id, "Supplier").await?;
    let v = supplier_validate(b, Some(&old))?;
    sqlx::query("UPDATE suppliers SET name=$2, contact_person=$3, phone=$4, email=$5, address=$6, lead_time_days=$7, is_active=$8 WHERE id=$1")
        .bind(id).bind(&v.name).bind(&v.contact_person).bind(&v.phone).bind(&v.email).bind(&v.address).bind(v.lead_time_days).bind(v.is_active)
        .execute(&mut *tx).await?;
    let row: Supplier = get_row(&mut tx, SUPPLIER_SEL, id, "Supplier").await?;
    audit(&mut tx, &actor, &ci, "SUPPLIER_UPDATE", "supplier", Some(id.to_string()), json!({ "before": old, "after": row })).await?;
    tx.commit().await?;
    Ok(Json(row))
}
async fn supplier_delete(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    soft_delete(&st, &actor, &ci, "suppliers", "supplier", "SUPPLIER_DEACTIVATE", id, "Supplier").await
}

/// `SELECT 1 ... FOR UPDATE` on the row we are about to modify (serialises concurrent admin edits).
pub async fn lock_row(conn: &mut PgConnection, table: &'static str, id: Uuid, what: &str) -> AppResult<()> {
    let r: Option<Uuid> = sqlx::query_scalar(&format!("SELECT id FROM {table} WHERE id = $1 FOR UPDATE")).bind(id).fetch_optional(&mut *conn).await?;
    r.map(|_| ()).ok_or_else(|| AppError::not_found(what))
}

// ---------------------------------------------------------------------------------------------------------------------
// Raw materials
// ---------------------------------------------------------------------------------------------------------------------

pub const MATERIAL_CATEGORIES: &[&str] = &["PREFORM", "CAP", "LABEL", "SLEEVE", "SHRINK_FILM", "CARTON", "MINERAL", "CHEMICAL", "EMPTY_BOTTLE", "OTHER"];

#[derive(Serialize, FromRow, Clone)]
struct Material {
    id: Uuid, code: String, name: String, category: String, stock_unit: String, purchase_unit: String,
    stock_per_purchase_unit: Decimal, unit_weight_g: Option<Decimal>, min_stock: Decimal,
    default_supplier_id: Option<Uuid>, default_supplier_name: Option<String>, is_active: bool, created_at: DateTime<Utc>,
}
const MATERIAL_SEL: &str = "SELECT t.id, t.code, t.name, t.category, t.stock_unit, t.purchase_unit, t.stock_per_purchase_unit, t.unit_weight_g, t.min_stock,
    t.default_supplier_id, s.name AS default_supplier_name, t.is_active, t.created_at
    FROM raw_materials t LEFT JOIN suppliers s ON s.id = t.default_supplier_id";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterialIn {
    code: Option<String>, name: Option<String>, category: Option<String>, stock_unit: Option<String>, purchase_unit: Option<String>,
    stock_per_purchase_unit: Option<Decimal>,
    #[serde(default, deserialize_with = "double_opt")] unit_weight_g: Option<Option<Decimal>>,
    min_stock: Option<Decimal>,
    #[serde(default, deserialize_with = "double_opt")] default_supplier_id: Option<Option<Uuid>>,
    is_active: Option<bool>,
}
struct MaterialV { code: String, name: String, category: String, stock_unit: String, purchase_unit: String, spp: Decimal, unit_weight_g: Option<Decimal>, min_stock: Decimal, supplier: Option<Uuid>, is_active: bool }

fn material_validate(b: MaterialIn, old: Option<&Material>) -> AppResult<MaterialV> {
    let code = code("code", &b.code.or_else(|| old.map(|o| o.code.clone())).unwrap_or_default())?;
    let name = validate::required_text("name", &b.name.or_else(|| old.map(|o| o.name.clone())).unwrap_or_default(), 120)?;
    let category = b.category.or_else(|| old.map(|o| o.category.clone())).unwrap_or_default();
    if !MATERIAL_CATEGORIES.contains(&category.as_str()) { return Err(AppError::field("category", &format!("must be one of {}", MATERIAL_CATEGORIES.join(", ")))); }
    let unit = |f: &str, v: Option<String>, o: Option<String>| -> AppResult<String> {
        let t = validate::required_text(f, &v.or(o).unwrap_or_else(|| "pcs".into()), 20)?;
        Ok(t)
    };
    let stock_unit = unit("stock_unit", b.stock_unit, old.map(|o| o.stock_unit.clone()))?;
    let purchase_unit = unit("purchase_unit", b.purchase_unit, old.map(|o| o.purchase_unit.clone()))?;
    let spp = b.stock_per_purchase_unit.or_else(|| old.map(|o| o.stock_per_purchase_unit)).unwrap_or(Decimal::ONE);
    dec_positive("stock_per_purchase_unit", spp, 4)?;
    let unit_weight_g = match b.unit_weight_g { Some(v) => v, None => old.and_then(|o| o.unit_weight_g) };
    if let Some(w) = unit_weight_g { dec_positive("unit_weight_g", w, 4)?; }
    let min_stock = b.min_stock.or_else(|| old.map(|o| o.min_stock)).unwrap_or_default();
    dec_non_negative("min_stock", min_stock, 4)?;
    let supplier = match b.default_supplier_id { Some(v) => v, None => old.and_then(|o| o.default_supplier_id) };
    Ok(MaterialV { code, name, category, stock_unit, purchase_unit, spp, unit_weight_g, min_stock, supplier, is_active: b.is_active.or_else(|| old.map(|o| o.is_active)).unwrap_or(true) })
}

async fn materials_list(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQ>) -> AppResult<Json<Vec<Material>>> {
    Ok(Json(list_rows(&st.db, MATERIAL_SEL, "t.code ILIKE $1 OR t.name ILIKE $1 OR t.category ILIKE $1", "lower(t.name), t.id", &q).await?))
}
async fn material_get(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<Material>> {
    Ok(Json(get_row(&mut *st.db.acquire().await?, MATERIAL_SEL, id, "Raw material").await?))
}
async fn material_create(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiJson(b): ApiJson<MaterialIn>) -> AppResult<(StatusCode, Json<Material>)> {
    let v = material_validate(b, None)?;
    let mut tx = st.db.begin().await?;
    if let Some(s) = v.supplier { require_active(&mut tx, "suppliers", s, "default_supplier_id").await?; }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO raw_materials (code, name, category, stock_unit, purchase_unit, stock_per_purchase_unit, unit_weight_g, min_stock, default_supplier_id, is_active)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id")
        .bind(&v.code).bind(&v.name).bind(&v.category).bind(&v.stock_unit).bind(&v.purchase_unit).bind(v.spp).bind(v.unit_weight_g).bind(v.min_stock).bind(v.supplier).bind(v.is_active)
        .fetch_one(&mut *tx).await.map_err(|e| dup(e, "A raw material with this code already exists"))?;
    let row: Material = get_row(&mut tx, MATERIAL_SEL, id, "Raw material").await?;
    audit(&mut tx, &actor, &ci, "RAW_MATERIAL_CREATE", "raw_material", Some(id.to_string()), json!({ "after": row })).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}
async fn material_update(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<MaterialIn>) -> AppResult<Json<Material>> {
    let mut tx = st.db.begin().await?;
    lock_row(&mut tx, "raw_materials", id, "Raw material").await?;
    let old: Material = get_row(&mut tx, MATERIAL_SEL, id, "Raw material").await?;
    let v = material_validate(b, Some(&old))?;
    if v.supplier != old.default_supplier_id { if let Some(s) = v.supplier { require_active(&mut tx, "suppliers", s, "default_supplier_id").await?; } }
    if v.category != old.category {
        // the category drives BOM/product rules (EMPTY_BOTTLE zone, PREFORM mapping, RETURNABLE exclusions): never silently change it under users
        let used: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bom_items WHERE raw_material_id = $1)
                                              OR EXISTS(SELECT 1 FROM products WHERE preform_material_id = $1 OR bottle_material_id = $1)
                                              OR EXISTS(SELECT 1 FROM stock_balances WHERE item_type = 'RAW' AND item_id = $1 AND qty > 0)")
            .bind(id).fetch_one(&mut *tx).await?;
        if used { return Err(AppError::conflict("IN_USE", "The category cannot change while this material is used in a BOM, a product mapping or has stock")); }
    }
    sqlx::query("UPDATE raw_materials SET code=$2, name=$3, category=$4, stock_unit=$5, purchase_unit=$6, stock_per_purchase_unit=$7, unit_weight_g=$8, min_stock=$9, default_supplier_id=$10, is_active=$11 WHERE id=$1")
        .bind(id).bind(&v.code).bind(&v.name).bind(&v.category).bind(&v.stock_unit).bind(&v.purchase_unit).bind(v.spp).bind(v.unit_weight_g).bind(v.min_stock).bind(v.supplier).bind(v.is_active)
        .execute(&mut *tx).await.map_err(|e| dup(e, "A raw material with this code already exists"))?;
    let row: Material = get_row(&mut tx, MATERIAL_SEL, id, "Raw material").await?;
    audit(&mut tx, &actor, &ci, "RAW_MATERIAL_UPDATE", "raw_material", Some(id.to_string()), json!({ "before": old, "after": row })).await?;
    tx.commit().await?;
    Ok(Json(row))
}
async fn material_delete(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    soft_delete(&st, &actor, &ci, "raw_materials", "raw_material", "RAW_MATERIAL_DEACTIVATE", id, "Raw material").await
}

// ---------------------------------------------------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Serialize, FromRow)]
struct RouteRow { id: Uuid, name: String, description: String, is_active: bool }
const ROUTE_SEL: &str = "SELECT t.id, t.name, t.description, t.is_active FROM routes t";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteIn { name: Option<String>, description: Option<String>, is_active: Option<bool> }

fn route_validate(b: RouteIn, old: Option<&RouteRow>) -> AppResult<(String, String, bool)> {
    Ok((validate::required_text("name", &b.name.or_else(|| old.map(|o| o.name.clone())).unwrap_or_default(), 120)?,
        validate::text("description", &b.description.or_else(|| old.map(|o| o.description.clone())).unwrap_or_default(), 500)?,
        b.is_active.or_else(|| old.map(|o| o.is_active)).unwrap_or(true)))
}
async fn routes_list(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQ>) -> AppResult<Json<Vec<RouteRow>>> {
    Ok(Json(list_rows(&st.db, ROUTE_SEL, "t.name ILIKE $1 OR t.description ILIKE $1", "lower(t.name), t.id", &q).await?))
}
async fn route_get(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<RouteRow>> {
    Ok(Json(get_row(&mut *st.db.acquire().await?, ROUTE_SEL, id, "Route").await?))
}
async fn route_create(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiJson(b): ApiJson<RouteIn>) -> AppResult<(StatusCode, Json<RouteRow>)> {
    let (name, description, active) = route_validate(b, None)?;
    let mut tx = st.db.begin().await?;
    let id: Uuid = sqlx::query_scalar("INSERT INTO routes (name, description, is_active) VALUES ($1,$2,$3) RETURNING id")
        .bind(&name).bind(&description).bind(active).fetch_one(&mut *tx).await.map_err(|e| dup(e, "A route with this name already exists"))?;
    let row: RouteRow = get_row(&mut tx, ROUTE_SEL, id, "Route").await?;
    audit(&mut tx, &actor, &ci, "ROUTE_CREATE", "route", Some(id.to_string()), json!({ "after": row })).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}
async fn route_update(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<RouteIn>) -> AppResult<Json<RouteRow>> {
    let mut tx = st.db.begin().await?;
    lock_row(&mut tx, "routes", id, "Route").await?;
    let old: RouteRow = get_row(&mut tx, ROUTE_SEL, id, "Route").await?;
    let (name, description, active) = route_validate(b, Some(&old))?;
    sqlx::query("UPDATE routes SET name=$2, description=$3, is_active=$4 WHERE id=$1").bind(id).bind(&name).bind(&description).bind(active)
        .execute(&mut *tx).await.map_err(|e| dup(e, "A route with this name already exists"))?;
    let row: RouteRow = get_row(&mut tx, ROUTE_SEL, id, "Route").await?;
    audit(&mut tx, &actor, &ci, "ROUTE_UPDATE", "route", Some(id.to_string()), json!({ "before": old, "after": row })).await?;
    tx.commit().await?;
    Ok(Json(row))
}
async fn route_delete(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    soft_delete(&st, &actor, &ci, "routes", "route", "ROUTE_DEACTIVATE", id, "Route").await
}

// ---------------------------------------------------------------------------------------------------------------------
// Customers
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Serialize, FromRow, Clone)]
struct Customer { id: Uuid, code: String, name: String, phone: String, address: String, route_id: Option<Uuid>, route_name: Option<String>, is_active: bool, created_at: DateTime<Utc> }
const CUSTOMER_SEL: &str = "SELECT t.id, t.code, t.name, t.phone, t.address, t.route_id, r.name AS route_name, t.is_active, t.created_at FROM customers t LEFT JOIN routes r ON r.id = t.route_id";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomerIn {
    code: Option<String>, name: Option<String>, phone: Option<String>, address: Option<String>,
    #[serde(default, deserialize_with = "double_opt")] route_id: Option<Option<Uuid>>, is_active: Option<bool>,
}
struct CustomerV { code: String, name: String, phone: String, address: String, route_id: Option<Uuid>, is_active: bool }
fn customer_validate(b: CustomerIn, old: Option<&Customer>) -> AppResult<CustomerV> {
    Ok(CustomerV {
        code: code("code", &b.code.or_else(|| old.map(|o| o.code.clone())).unwrap_or_default())?,
        name: validate::required_text("name", &b.name.or_else(|| old.map(|o| o.name.clone())).unwrap_or_default(), 120)?,
        phone: phone("phone", &b.phone.or_else(|| old.map(|o| o.phone.clone())).unwrap_or_default())?,
        address: validate::text("address", &b.address.or_else(|| old.map(|o| o.address.clone())).unwrap_or_default(), 500)?,
        route_id: match b.route_id { Some(v) => v, None => old.and_then(|o| o.route_id) },
        is_active: b.is_active.or_else(|| old.map(|o| o.is_active)).unwrap_or(true),
    })
}
async fn customers_list(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQ>) -> AppResult<Json<Vec<Customer>>> {
    Ok(Json(list_rows(&st.db, CUSTOMER_SEL, "t.code ILIKE $1 OR t.name ILIKE $1 OR t.phone ILIKE $1 OR t.address ILIKE $1", "lower(t.name), t.id", &q).await?))
}
async fn customer_get(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<Customer>> {
    Ok(Json(get_row(&mut *st.db.acquire().await?, CUSTOMER_SEL, id, "Customer").await?))
}
async fn customer_create(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiJson(b): ApiJson<CustomerIn>) -> AppResult<(StatusCode, Json<Customer>)> {
    let v = customer_validate(b, None)?;
    let mut tx = st.db.begin().await?;
    if let Some(r) = v.route_id { require_active(&mut tx, "routes", r, "route_id").await?; }
    let id: Uuid = sqlx::query_scalar("INSERT INTO customers (code, name, phone, address, route_id, is_active) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id")
        .bind(&v.code).bind(&v.name).bind(&v.phone).bind(&v.address).bind(v.route_id).bind(v.is_active)
        .fetch_one(&mut *tx).await.map_err(|e| dup(e, "A customer with this code already exists"))?;
    let row: Customer = get_row(&mut tx, CUSTOMER_SEL, id, "Customer").await?;
    audit(&mut tx, &actor, &ci, "CUSTOMER_CREATE", "customer", Some(id.to_string()), json!({ "after": row })).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}
async fn customer_update(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<CustomerIn>) -> AppResult<Json<Customer>> {
    let mut tx = st.db.begin().await?;
    lock_row(&mut tx, "customers", id, "Customer").await?;
    let old: Customer = get_row(&mut tx, CUSTOMER_SEL, id, "Customer").await?;
    let v = customer_validate(b, Some(&old))?;
    if v.route_id != old.route_id { if let Some(r) = v.route_id { require_active(&mut tx, "routes", r, "route_id").await?; } }
    sqlx::query("UPDATE customers SET code=$2, name=$3, phone=$4, address=$5, route_id=$6, is_active=$7 WHERE id=$1")
        .bind(id).bind(&v.code).bind(&v.name).bind(&v.phone).bind(&v.address).bind(v.route_id).bind(v.is_active)
        .execute(&mut *tx).await.map_err(|e| dup(e, "A customer with this code already exists"))?;
    let row: Customer = get_row(&mut tx, CUSTOMER_SEL, id, "Customer").await?;
    audit(&mut tx, &actor, &ci, "CUSTOMER_UPDATE", "customer", Some(id.to_string()), json!({ "before": old, "after": row })).await?;
    tx.commit().await?;
    Ok(Json(row))
}
async fn customer_delete(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    soft_delete(&st, &actor, &ci, "customers", "customer", "CUSTOMER_DEACTIVATE", id, "Customer").await
}

// ---------------------------------------------------------------------------------------------------------------------
// Vehicles
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Serialize, FromRow, Clone)]
struct Vehicle {
    id: Uuid, reg_no: String, name: String, capacity_crates: i32, driver_name: String, driver_phone: String,
    insurance_expiry: Option<NaiveDate>, fitness_expiry: Option<NaiveDate>, puc_expiry: Option<NaiveDate>, permit_expiry: Option<NaiveDate>,
    is_active: bool, created_at: DateTime<Utc>,
}
const VEHICLE_SEL: &str = "SELECT t.id, t.reg_no, t.name, t.capacity_crates, t.driver_name, t.driver_phone, t.insurance_expiry, t.fitness_expiry, t.puc_expiry, t.permit_expiry, t.is_active, t.created_at FROM vehicles t";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VehicleIn {
    reg_no: Option<String>, name: Option<String>, capacity_crates: Option<i32>, driver_name: Option<String>, driver_phone: Option<String>,
    #[serde(default, deserialize_with = "double_opt")] insurance_expiry: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "double_opt")] fitness_expiry: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "double_opt")] puc_expiry: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "double_opt")] permit_expiry: Option<Option<NaiveDate>>,
    is_active: Option<bool>,
}
struct VehicleV { reg_no: String, name: String, cap: i32, driver_name: String, driver_phone: String, ins: Option<NaiveDate>, fit: Option<NaiveDate>, puc: Option<NaiveDate>, permit: Option<NaiveDate>, is_active: bool }
fn vehicle_validate(b: VehicleIn, old: Option<&Vehicle>) -> AppResult<VehicleV> {
    let reg = b.reg_no.or_else(|| old.map(|o| o.reg_no.clone())).unwrap_or_default();
    let reg_no = validate::required_text("reg_no", &reg, 20)?.to_uppercase();
    if !reg_no.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-') { return Err(AppError::field("reg_no", "may only contain letters, digits, spaces and '-'")); }
    let cap = b.capacity_crates.or_else(|| old.map(|o| o.capacity_crates)).ok_or_else(|| AppError::field("capacity_crates", "is required"))?;
    int_range("capacity_crates", cap as i64, 1, 100_000)?;
    let d = |f: &str, v: Option<Option<NaiveDate>>, o: Option<NaiveDate>| -> AppResult<Option<NaiveDate>> {
        let x = match v { Some(x) => x, None => o };
        if let Some(x) = x { date_sane(f, x)?; }
        Ok(x)
    };
    Ok(VehicleV {
        reg_no, cap,
        name: validate::text("name", &b.name.or_else(|| old.map(|o| o.name.clone())).unwrap_or_default(), 120)?,
        driver_name: validate::text("driver_name", &b.driver_name.or_else(|| old.map(|o| o.driver_name.clone())).unwrap_or_default(), 120)?,
        driver_phone: phone("driver_phone", &b.driver_phone.or_else(|| old.map(|o| o.driver_phone.clone())).unwrap_or_default())?,
        ins: d("insurance_expiry", b.insurance_expiry, old.and_then(|o| o.insurance_expiry))?,
        fit: d("fitness_expiry", b.fitness_expiry, old.and_then(|o| o.fitness_expiry))?,
        puc: d("puc_expiry", b.puc_expiry, old.and_then(|o| o.puc_expiry))?,
        permit: d("permit_expiry", b.permit_expiry, old.and_then(|o| o.permit_expiry))?,
        is_active: b.is_active.or_else(|| old.map(|o| o.is_active)).unwrap_or(true),
    })
}
async fn vehicles_list(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQ>) -> AppResult<Json<Vec<Vehicle>>> {
    Ok(Json(list_rows(&st.db, VEHICLE_SEL, "t.reg_no ILIKE $1 OR t.name ILIKE $1 OR t.driver_name ILIKE $1", "t.reg_no, t.id", &q).await?))
}
async fn vehicle_get(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<Vehicle>> {
    Ok(Json(get_row(&mut *st.db.acquire().await?, VEHICLE_SEL, id, "Vehicle").await?))
}
async fn vehicle_create(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiJson(b): ApiJson<VehicleIn>) -> AppResult<(StatusCode, Json<Vehicle>)> {
    let v = vehicle_validate(b, None)?;
    let mut tx = st.db.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO vehicles (reg_no, name, capacity_crates, driver_name, driver_phone, insurance_expiry, fitness_expiry, puc_expiry, permit_expiry, is_active)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id")
        .bind(&v.reg_no).bind(&v.name).bind(v.cap).bind(&v.driver_name).bind(&v.driver_phone).bind(v.ins).bind(v.fit).bind(v.puc).bind(v.permit).bind(v.is_active)
        .fetch_one(&mut *tx).await.map_err(|e| dup(e, "A vehicle with this registration number already exists"))?;
    let row: Vehicle = get_row(&mut tx, VEHICLE_SEL, id, "Vehicle").await?;
    audit(&mut tx, &actor, &ci, "VEHICLE_CREATE", "vehicle", Some(id.to_string()), json!({ "after": row })).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}
async fn vehicle_update(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<VehicleIn>) -> AppResult<Json<Vehicle>> {
    let mut tx = st.db.begin().await?;
    lock_row(&mut tx, "vehicles", id, "Vehicle").await?;
    let old: Vehicle = get_row(&mut tx, VEHICLE_SEL, id, "Vehicle").await?;
    let v = vehicle_validate(b, Some(&old))?;
    sqlx::query("UPDATE vehicles SET reg_no=$2, name=$3, capacity_crates=$4, driver_name=$5, driver_phone=$6, insurance_expiry=$7, fitness_expiry=$8, puc_expiry=$9, permit_expiry=$10, is_active=$11 WHERE id=$1")
        .bind(id).bind(&v.reg_no).bind(&v.name).bind(v.cap).bind(&v.driver_name).bind(&v.driver_phone).bind(v.ins).bind(v.fit).bind(v.puc).bind(v.permit).bind(v.is_active)
        .execute(&mut *tx).await.map_err(|e| dup(e, "A vehicle with this registration number already exists"))?;
    let row: Vehicle = get_row(&mut tx, VEHICLE_SEL, id, "Vehicle").await?;
    audit(&mut tx, &actor, &ci, "VEHICLE_UPDATE", "vehicle", Some(id.to_string()), json!({ "before": old, "after": row })).await?;
    tx.commit().await?;
    Ok(Json(row))
}
async fn vehicle_delete(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    soft_delete(&st, &actor, &ci, "vehicles", "vehicle", "VEHICLE_DEACTIVATE", id, "Vehicle").await
}

// ---------------------------------------------------------------------------------------------------------------------
// Equipment
// ---------------------------------------------------------------------------------------------------------------------

pub const EQUIPMENT_KINDS: &[&str] = &["RO", "UV", "OZONATOR", "BLOW_MOLDER", "FILTER", "COMPRESSOR", "FILLER", "OTHER"];

#[derive(Serialize, FromRow, Clone)]
struct Equipment { id: Uuid, code: String, name: String, kind: String, location: String, is_active: bool }
const EQUIPMENT_SEL: &str = "SELECT t.id, t.code, t.name, t.kind, t.location, t.is_active FROM equipment t";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EquipmentIn { code: Option<String>, name: Option<String>, kind: Option<String>, location: Option<String>, is_active: Option<bool> }
fn equipment_validate(b: EquipmentIn, old: Option<&Equipment>) -> AppResult<Equipment> {
    let kind = b.kind.or_else(|| old.map(|o| o.kind.clone())).unwrap_or_default();
    if !EQUIPMENT_KINDS.contains(&kind.as_str()) { return Err(AppError::field("kind", &format!("must be one of {}", EQUIPMENT_KINDS.join(", ")))); }
    Ok(Equipment {
        id: old.map(|o| o.id).unwrap_or_default(),
        code: code("code", &b.code.or_else(|| old.map(|o| o.code.clone())).unwrap_or_default())?,
        name: validate::required_text("name", &b.name.or_else(|| old.map(|o| o.name.clone())).unwrap_or_default(), 120)?,
        kind,
        location: validate::text("location", &b.location.or_else(|| old.map(|o| o.location.clone())).unwrap_or_default(), 120)?,
        is_active: b.is_active.or_else(|| old.map(|o| o.is_active)).unwrap_or(true),
    })
}
async fn equipment_list(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQ>) -> AppResult<Json<Vec<Equipment>>> {
    Ok(Json(list_rows(&st.db, EQUIPMENT_SEL, "t.code ILIKE $1 OR t.name ILIKE $1 OR t.location ILIKE $1 OR t.kind ILIKE $1", "t.code, t.id", &q).await?))
}
async fn equipment_get(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<Equipment>> {
    Ok(Json(get_row(&mut *st.db.acquire().await?, EQUIPMENT_SEL, id, "Equipment").await?))
}
async fn equipment_create(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiJson(b): ApiJson<EquipmentIn>) -> AppResult<(StatusCode, Json<Equipment>)> {
    let v = equipment_validate(b, None)?;
    let mut tx = st.db.begin().await?;
    let id: Uuid = sqlx::query_scalar("INSERT INTO equipment (code, name, kind, location, is_active) VALUES ($1,$2,$3,$4,$5) RETURNING id")
        .bind(&v.code).bind(&v.name).bind(&v.kind).bind(&v.location).bind(v.is_active).fetch_one(&mut *tx).await.map_err(|e| dup(e, "Equipment with this code already exists"))?;
    let row: Equipment = get_row(&mut tx, EQUIPMENT_SEL, id, "Equipment").await?;
    audit(&mut tx, &actor, &ci, "EQUIPMENT_CREATE", "equipment", Some(id.to_string()), json!({ "after": row })).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}
async fn equipment_update(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<EquipmentIn>) -> AppResult<Json<Equipment>> {
    let mut tx = st.db.begin().await?;
    lock_row(&mut tx, "equipment", id, "Equipment").await?;
    let old: Equipment = get_row(&mut tx, EQUIPMENT_SEL, id, "Equipment").await?;
    let v = equipment_validate(b, Some(&old))?;
    sqlx::query("UPDATE equipment SET code=$2, name=$3, kind=$4, location=$5, is_active=$6 WHERE id=$1")
        .bind(id).bind(&v.code).bind(&v.name).bind(&v.kind).bind(&v.location).bind(v.is_active).execute(&mut *tx).await.map_err(|e| dup(e, "Equipment with this code already exists"))?;
    let row: Equipment = get_row(&mut tx, EQUIPMENT_SEL, id, "Equipment").await?;
    audit(&mut tx, &actor, &ci, "EQUIPMENT_UPDATE", "equipment", Some(id.to_string()), json!({ "before": old, "after": row })).await?;
    tx.commit().await?;
    Ok(Json(row))
}
async fn equipment_delete(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    soft_delete(&st, &actor, &ci, "equipment", "equipment", "EQUIPMENT_DEACTIVATE", id, "Equipment").await
}

// ---------------------------------------------------------------------------------------------------------------------
// Maintenance schedules
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Serialize, FromRow, Clone)]
struct Schedule {
    id: Uuid, subject_type: String, equipment_id: Option<Uuid>, vehicle_id: Option<Uuid>, subject_name: Option<String>, task: String,
    interval_days: Option<i32>, last_done_on: Option<NaiveDate>, next_due_on: NaiveDate, is_active: bool, created_at: DateTime<Utc>,
}
const SCHEDULE_SEL: &str = "SELECT t.id, t.subject_type, t.equipment_id, t.vehicle_id, COALESCE(e.name, v.reg_no) AS subject_name, t.task, t.interval_days,
    t.last_done_on, t.next_due_on, t.is_active, t.created_at
    FROM maintenance_schedules t LEFT JOIN equipment e ON e.id = t.equipment_id LEFT JOIN vehicles v ON v.id = t.vehicle_id";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleIn {
    subject_type: Option<String>,
    #[serde(default, deserialize_with = "double_opt")] equipment_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "double_opt")] vehicle_id: Option<Option<Uuid>>,
    task: Option<String>,
    #[serde(default, deserialize_with = "double_opt")] interval_days: Option<Option<i32>>,
    #[serde(default, deserialize_with = "double_opt")] last_done_on: Option<Option<NaiveDate>>,
    next_due_on: Option<NaiveDate>,
    is_active: Option<bool>,
}
struct ScheduleV { subject_type: String, equipment_id: Option<Uuid>, vehicle_id: Option<Uuid>, task: String, interval_days: Option<i32>, last_done_on: Option<NaiveDate>, next_due_on: NaiveDate, is_active: bool }
fn schedule_validate(b: ScheduleIn, old: Option<&Schedule>) -> AppResult<ScheduleV> {
    let subject_type = b.subject_type.clone().or_else(|| old.map(|o| o.subject_type.clone())).unwrap_or_default();
    if subject_type != "EQUIPMENT" && subject_type != "VEHICLE" { return Err(AppError::field("subject_type", "must be EQUIPMENT or VEHICLE")); }
    let switched = old.map_or(false, |o| o.subject_type != subject_type);
    // when the subject type flips, the id of the old type is dropped unless the caller sent it explicitly
    let eq_old = if switched { None } else { old.and_then(|o| o.equipment_id) };
    let ve_old = if switched { None } else { old.and_then(|o| o.vehicle_id) };
    let equipment_id = match b.equipment_id { Some(v) => v, None => eq_old };
    let vehicle_id = match b.vehicle_id { Some(v) => v, None => ve_old };
    match subject_type.as_str() {
        "EQUIPMENT" => {
            if equipment_id.is_none() { return Err(AppError::field("equipment_id", "is required for EQUIPMENT schedules")); }
            if vehicle_id.is_some() { return Err(AppError::field("vehicle_id", "must be empty for EQUIPMENT schedules")); }
        }
        _ => {
            if vehicle_id.is_none() { return Err(AppError::field("vehicle_id", "is required for VEHICLE schedules")); }
            if equipment_id.is_some() { return Err(AppError::field("equipment_id", "must be empty for VEHICLE schedules")); }
        }
    }
    let interval_days = match b.interval_days { Some(v) => v, None => old.and_then(|o| o.interval_days) };
    if let Some(i) = interval_days { int_range("interval_days", i as i64, 1, 3650)?; }
    let last_done_on = match b.last_done_on { Some(v) => v, None => old.and_then(|o| o.last_done_on) };
    if let Some(d) = last_done_on { date_sane("last_done_on", d)?; }
    let next_due_on = req("next_due_on", b.next_due_on.or_else(|| old.map(|o| o.next_due_on)))?;
    date_sane("next_due_on", next_due_on)?;
    Ok(ScheduleV {
        subject_type, equipment_id, vehicle_id, interval_days, last_done_on, next_due_on,
        task: validate::required_text("task", &b.task.or_else(|| old.map(|o| o.task.clone())).unwrap_or_default(), 200)?,
        is_active: b.is_active.or_else(|| old.map(|o| o.is_active)).unwrap_or(true),
    })
}
async fn check_schedule_refs(tx: &mut PgConnection, v: &ScheduleV, old: Option<&Schedule>) -> AppResult<()> {
    if let Some(e) = v.equipment_id { if old.map_or(true, |o| o.equipment_id != Some(e)) { require_active(tx, "equipment", e, "equipment_id").await?; } }
    if let Some(e) = v.vehicle_id { if old.map_or(true, |o| o.vehicle_id != Some(e)) { require_active(tx, "vehicles", e, "vehicle_id").await?; } }
    Ok(())
}
async fn schedules_list(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQ>) -> AppResult<Json<Vec<Schedule>>> {
    Ok(Json(list_rows(&st.db, SCHEDULE_SEL, "t.task ILIKE $1 OR e.name ILIKE $1 OR v.reg_no ILIKE $1", "t.next_due_on, t.id", &q).await?))
}
async fn schedule_get(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<Schedule>> {
    Ok(Json(get_row(&mut *st.db.acquire().await?, SCHEDULE_SEL, id, "Schedule").await?))
}
async fn schedule_create(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiJson(b): ApiJson<ScheduleIn>) -> AppResult<(StatusCode, Json<Schedule>)> {
    let v = schedule_validate(b, None)?;
    let mut tx = st.db.begin().await?;
    check_schedule_refs(&mut tx, &v, None).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO maintenance_schedules (subject_type, equipment_id, vehicle_id, task, interval_days, last_done_on, next_due_on, is_active)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id")
        .bind(&v.subject_type).bind(v.equipment_id).bind(v.vehicle_id).bind(&v.task).bind(v.interval_days).bind(v.last_done_on).bind(v.next_due_on).bind(v.is_active)
        .fetch_one(&mut *tx).await?;
    let row: Schedule = get_row(&mut tx, SCHEDULE_SEL, id, "Schedule").await?;
    audit(&mut tx, &actor, &ci, "MAINTENANCE_SCHEDULE_CREATE", "maintenance_schedule", Some(id.to_string()), json!({ "after": row })).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}
async fn schedule_update(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<ScheduleIn>) -> AppResult<Json<Schedule>> {
    let mut tx = st.db.begin().await?;
    lock_row(&mut tx, "maintenance_schedules", id, "Schedule").await?;
    let old: Schedule = get_row(&mut tx, SCHEDULE_SEL, id, "Schedule").await?;
    let v = schedule_validate(b, Some(&old))?;
    check_schedule_refs(&mut tx, &v, Some(&old)).await?;
    sqlx::query("UPDATE maintenance_schedules SET subject_type=$2, equipment_id=$3, vehicle_id=$4, task=$5, interval_days=$6, last_done_on=$7, next_due_on=$8, is_active=$9 WHERE id=$1")
        .bind(id).bind(&v.subject_type).bind(v.equipment_id).bind(v.vehicle_id).bind(&v.task).bind(v.interval_days).bind(v.last_done_on).bind(v.next_due_on).bind(v.is_active)
        .execute(&mut *tx).await?;
    let row: Schedule = get_row(&mut tx, SCHEDULE_SEL, id, "Schedule").await?;
    audit(&mut tx, &actor, &ci, "MAINTENANCE_SCHEDULE_UPDATE", "maintenance_schedule", Some(id.to_string()), json!({ "before": old, "after": row })).await?;
    tx.commit().await?;
    Ok(Json(row))
}
async fn schedule_delete(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    soft_delete(&st, &actor, &ci, "maintenance_schedules", "maintenance_schedule", "MAINTENANCE_SCHEDULE_DEACTIVATE", id, "Schedule").await
}
