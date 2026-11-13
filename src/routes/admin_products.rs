//! Admin: products (BOTTLED / RETURNABLE) and their bill of materials (API_REFERENCE §2, §0.2-E).
use super::common::{audit, ApiJson, ApiPath, ApiQuery, ClientInfo};
use super::master::{dec_non_negative, dec_positive, double_opt, dup, get_row, int_range, list_rows, lock_row, soft_delete, ListQ};
use crate::{auth::AuthUser, error::{AppError, AppResult}, state::AppState, validate};
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/products", get(products_list).post(product_create))
        .route("/products/{id}", get(product_get).put(product_update).delete(product_delete))
        .route("/products/{id}/bom", get(bom_get).put(bom_put))
}

#[derive(Serialize, FromRow, Clone)]
struct Product {
    id: Uuid, code: String, name: String, kind: String, size_ml: Option<i32>, units_per_crate: i32, crates_per_pallet: Option<i32>,
    shelf_life_days: i32, min_stock_crates: Decimal,
    preform_material_id: Option<Uuid>, preform_material_name: Option<String>, bottle_material_id: Option<Uuid>, bottle_material_name: Option<String>,
    max_cycles: Option<i32>, is_active: bool, created_at: DateTime<Utc>,
}
const PRODUCT_SEL: &str = "SELECT t.id, t.code, t.name, t.kind, t.size_ml, t.units_per_crate, t.crates_per_pallet, t.shelf_life_days, t.min_stock_crates,
    t.preform_material_id, pm.name AS preform_material_name, t.bottle_material_id, bm.name AS bottle_material_name, t.max_cycles, t.is_active, t.created_at
    FROM products t LEFT JOIN raw_materials pm ON pm.id = t.preform_material_id LEFT JOIN raw_materials bm ON bm.id = t.bottle_material_id";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductIn {
    code: Option<String>, name: Option<String>, kind: Option<String>,
    #[serde(default, deserialize_with = "double_opt")] size_ml: Option<Option<i32>>,
    units_per_crate: Option<i32>,
    #[serde(default, deserialize_with = "double_opt")] crates_per_pallet: Option<Option<i32>>,
    shelf_life_days: Option<i32>, min_stock_crates: Option<Decimal>,
    #[serde(default, deserialize_with = "double_opt")] preform_material_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "double_opt")] bottle_material_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "double_opt")] max_cycles: Option<Option<i32>>,
    is_active: Option<bool>,
}
struct ProductV {
    code: String, name: String, kind: String, size_ml: Option<i32>, upc: i32, cpp: Option<i32>, shelf: i32, min_stock: Decimal,
    preform: Option<Uuid>, bottle: Option<Uuid>, max_cycles: Option<i32>, is_active: bool,
}

fn product_validate(b: ProductIn, old: Option<&Product>) -> AppResult<ProductV> {
    let kind = b.kind.or_else(|| old.map(|o| o.kind.clone())).unwrap_or_default();
    if kind != "BOTTLED" && kind != "RETURNABLE" { return Err(AppError::field("kind", "must be BOTTLED or RETURNABLE")); }
    let size_ml = match b.size_ml { Some(v) => v, None => old.and_then(|o| o.size_ml) };
    if let Some(s) = size_ml { int_range("size_ml", s as i64, 1, 1_000_000)?; }
    if kind == "BOTTLED" && size_ml.is_none() { return Err(AppError::field("size_ml", "is required for BOTTLED products")); }
    let upc = b.units_per_crate.or_else(|| old.map(|o| o.units_per_crate)).unwrap_or(if kind == "RETURNABLE" { 1 } else { 12 });
    int_range("units_per_crate", upc as i64, 1, 10_000)?;
    if kind == "RETURNABLE" && upc != 1 { return Err(AppError::field("units_per_crate", "must be 1 for RETURNABLE products (one container per unit)")); }
    let cpp = match b.crates_per_pallet { Some(v) => v, None => old.and_then(|o| o.crates_per_pallet) };
    if let Some(c) = cpp { int_range("crates_per_pallet", c as i64, 1, 100_000)?; }
    let shelf = b.shelf_life_days.or_else(|| old.map(|o| o.shelf_life_days)).unwrap_or(180);
    int_range("shelf_life_days", shelf as i64, 1, 3650)?;
    let min_stock = b.min_stock_crates.or_else(|| old.map(|o| o.min_stock_crates)).unwrap_or_default();
    dec_non_negative("min_stock_crates", min_stock, 4)?;
    let preform = match b.preform_material_id { Some(v) => v, None => old.and_then(|o| o.preform_material_id) };
    let bottle = match b.bottle_material_id { Some(v) => v, None => old.and_then(|o| o.bottle_material_id) };
    let max_cycles = match b.max_cycles { Some(v) => v, None => old.and_then(|o| o.max_cycles) };
    if let Some(m) = max_cycles { int_range("max_cycles", m as i64, 1, 100_000)?; }
    if kind == "RETURNABLE" {
        if preform.is_some() { return Err(AppError::field("preform_material_id", "must be empty for RETURNABLE products (they never consume preforms)")); }
        if bottle.is_some() { return Err(AppError::field("bottle_material_id", "must be empty for RETURNABLE products (they never consume empty bottles)")); }
    } else if max_cycles.is_some() {
        return Err(AppError::field("max_cycles", "only applies to RETURNABLE products"));
    }
    Ok(ProductV {
        code: super::master::code("code", &b.code.or_else(|| old.map(|o| o.code.clone())).unwrap_or_default())?,
        name: validate::required_text("name", &b.name.or_else(|| old.map(|o| o.name.clone())).unwrap_or_default(), 120)?,
        kind, size_ml, upc, cpp, shelf, min_stock, preform, bottle, max_cycles,
        is_active: b.is_active.or_else(|| old.map(|o| o.is_active)).unwrap_or(true),
    })
}

/// Material references must exist, be active and have the right category.
async fn check_material_ref(conn: &mut PgConnection, id: Uuid, category: &str, field: &str) -> AppResult<()> {
    let r: Option<(String, bool)> = sqlx::query_as("SELECT category, is_active FROM raw_materials WHERE id = $1").bind(id).fetch_optional(&mut *conn).await?;
    match r {
        None => Err(AppError::field(field, "does not exist")),
        Some((_, false)) => Err(AppError::field(field, "refers to an inactive material")),
        Some((c, _)) if c != category => Err(AppError::field(field, &format!("must be a material of category {category}"))),
        _ => Ok(()),
    }
}

/// Cross-entity invariants that depend on stored data (BOM contents, referenced materials).
async fn check_product_refs(conn: &mut PgConnection, v: &ProductV, id: Option<Uuid>, old: Option<&Product>) -> AppResult<()> {
    if let Some(p) = v.preform { if old.map_or(true, |o| o.preform_material_id != Some(p)) { check_material_ref(conn, p, "PREFORM", "preform_material_id").await?; } }
    if let Some(p) = v.bottle { if old.map_or(true, |o| o.bottle_material_id != Some(p)) { check_material_ref(conn, p, "EMPTY_BOTTLE", "bottle_material_id").await?; } }
    if let Some(id) = id {
        if v.kind == "RETURNABLE" {
            let bad: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bom_items b JOIN raw_materials m ON m.id = b.raw_material_id
                                                  WHERE b.product_id = $1 AND m.category IN ('PREFORM','EMPTY_BOTTLE'))").bind(id).fetch_one(&mut *conn).await?;
            if bad { return Err(AppError::field("kind", "RETURNABLE products cannot have PREFORM / EMPTY_BOTTLE in their BOM - edit the BOM first")); }
        }
        if v.size_ml.is_none() {
            let bad: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bom_items WHERE product_id = $1 AND basis = 'PER_1000L')").bind(id).fetch_one(&mut *conn).await?;
            if bad { return Err(AppError::field("size_ml", "is required while the BOM has PER_1000L (litre-based) lines")); }
        }
    }
    Ok(())
}

async fn products_list(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQ>) -> AppResult<Json<Vec<Product>>> {
    Ok(Json(list_rows(&st.db, PRODUCT_SEL, "t.code ILIKE $1 OR t.name ILIKE $1", "lower(t.name), t.id", &q).await?))
}
async fn product_get(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<Product>> {
    Ok(Json(get_row(&mut *st.db.acquire().await?, PRODUCT_SEL, id, "Product").await?))
}
async fn product_create(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiJson(b): ApiJson<ProductIn>) -> AppResult<(StatusCode, Json<Product>)> {
    let v = product_validate(b, None)?;
    let mut tx = st.db.begin().await?;
    check_product_refs(&mut tx, &v, None, None).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO products (code, name, kind, size_ml, units_per_crate, crates_per_pallet, shelf_life_days, min_stock_crates, preform_material_id, bottle_material_id, max_cycles, is_active)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING id")
        .bind(&v.code).bind(&v.name).bind(&v.kind).bind(v.size_ml).bind(v.upc).bind(v.cpp).bind(v.shelf).bind(v.min_stock).bind(v.preform).bind(v.bottle).bind(v.max_cycles).bind(v.is_active)
        .fetch_one(&mut *tx).await.map_err(|e| dup(e, "A product with this code already exists"))?;
    let row: Product = get_row(&mut tx, PRODUCT_SEL, id, "Product").await?;
    audit(&mut tx, &actor, &ci, "PRODUCT_CREATE", "product", Some(id.to_string()), json!({ "after": row })).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}
async fn product_update(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<ProductIn>) -> AppResult<Json<Product>> {
    let mut tx = st.db.begin().await?;
    lock_row(&mut tx, "products", id, "Product").await?;
    let old: Product = get_row(&mut tx, PRODUCT_SEL, id, "Product").await?;
    let v = product_validate(b, Some(&old))?;
    check_product_refs(&mut tx, &v, Some(id), Some(&old)).await?;
    if v.kind != old.kind {
        let used: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM production_batches WHERE product_id = $1)").bind(id).fetch_one(&mut *tx).await?;
        if used { return Err(AppError::conflict("IN_USE", "The kind cannot change after batches were produced for this product")); }
    }
    sqlx::query("UPDATE products SET code=$2, name=$3, kind=$4, size_ml=$5, units_per_crate=$6, crates_per_pallet=$7, shelf_life_days=$8, min_stock_crates=$9,
                                     preform_material_id=$10, bottle_material_id=$11, max_cycles=$12, is_active=$13 WHERE id=$1")
        .bind(id).bind(&v.code).bind(&v.name).bind(&v.kind).bind(v.size_ml).bind(v.upc).bind(v.cpp).bind(v.shelf).bind(v.min_stock).bind(v.preform).bind(v.bottle).bind(v.max_cycles).bind(v.is_active)
        .execute(&mut *tx).await.map_err(|e| dup(e, "A product with this code already exists"))?;
    let row: Product = get_row(&mut tx, PRODUCT_SEL, id, "Product").await?;
    audit(&mut tx, &actor, &ci, "PRODUCT_UPDATE", "product", Some(id.to_string()), json!({ "before": old, "after": row })).await?;
    tx.commit().await?;
    Ok(Json(row))
}
async fn product_delete(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>) -> AppResult<StatusCode> {
    soft_delete(&st, &actor, &ci, "products", "product", "PRODUCT_DEACTIVATE", id, "Product").await
}

// ---------------------------------------------------------------------------------------------------------------------
// BOM
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Serialize, FromRow)]
struct BomLine {
    id: Uuid, raw_material_id: Uuid, raw_material_name: String, raw_material_code: String, category: String, stock_unit: String,
    qty_per_crate: Decimal, basis: String, source_zone: String,
}
const BOM_SQL: &str = "SELECT b.id, b.raw_material_id, m.name AS raw_material_name, m.code AS raw_material_code, m.category, m.stock_unit,
    b.qty_per_crate, b.basis, b.source_zone FROM bom_items b JOIN raw_materials m ON m.id = b.raw_material_id WHERE b.product_id = $1 ORDER BY m.category, lower(m.name), b.id";

async fn bom_lines(conn: &mut PgConnection, product_id: Uuid) -> AppResult<Vec<BomLine>> {
    Ok(sqlx::query_as::<_, BomLine>(BOM_SQL).bind(product_id).fetch_all(&mut *conn).await?)
}

async fn bom_get(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<Vec<BomLine>>> {
    let mut c = st.db.acquire().await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM products WHERE id = $1)").bind(id).fetch_one(&mut *c).await?;
    if !exists { return Err(AppError::not_found("Product")); }
    Ok(Json(bom_lines(&mut c, id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LineIn { raw_material_id: Uuid, qty_per_crate: Decimal, basis: Option<String>, source_zone: Option<String> }
/// Body: a bare array of lines, or `{lines:[…]}` / `{items:[…]}`.
#[derive(Deserialize)]
#[serde(untagged)]
enum BomBody {
    List(Vec<LineIn>),
    Obj(BomObj),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BomObj { lines: Option<Vec<LineIn>>, items: Option<Vec<LineIn>> }

async fn bom_put(State(st): State<AppState>, actor: AuthUser, ci: ClientInfo, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<BomBody>) -> AppResult<Json<Vec<BomLine>>> {
    let lines = match body { BomBody::List(l) => l, BomBody::Obj(o) => o.lines.or(o.items).ok_or_else(|| AppError::field("lines", "is required"))? };
    if lines.len() > validate::MAX_LINES { return Err(AppError::field("lines", "too many lines (max 200)")); }
    let mut tx = st.db.begin().await?;
    lock_row(&mut tx, "products", id, "Product").await?;
    let (kind, size_ml): (String, Option<i32>) = sqlx::query_as("SELECT kind, size_ml FROM products WHERE id = $1").bind(id).fetch_one(&mut *tx).await?;
    let before = bom_lines(&mut tx, id).await?;
    let mut seen = std::collections::HashSet::new();
    let mut clean: Vec<(Uuid, Decimal, String, String)> = vec![];
    for (i, l) in lines.iter().enumerate() {
        let f = |s: &str| format!("lines[{i}].{s}");
        if !seen.insert(l.raw_material_id) { return Err(AppError::field(&f("raw_material_id"), "appears more than once in the BOM")); }
        dec_positive(&f("qty_per_crate"), l.qty_per_crate, 6)?;
        let m: Option<(String, bool)> = sqlx::query_as("SELECT category, is_active FROM raw_materials WHERE id = $1").bind(l.raw_material_id).fetch_optional(&mut *tx).await?;
        let (cat, active) = m.ok_or_else(|| AppError::field(&f("raw_material_id"), "does not exist"))?;
        if !active { return Err(AppError::field(&f("raw_material_id"), "refers to an inactive material")); }
        let basis = l.basis.clone().unwrap_or_else(|| "PER_CRATE".into());
        if basis != "PER_CRATE" && basis != "PER_1000L" { return Err(AppError::field(&f("basis"), "must be PER_CRATE or PER_1000L")); }
        if basis == "PER_1000L" && size_ml.is_none() { return Err(AppError::field(&f("basis"), "PER_1000L needs the product's size_ml to be set")); }
        let want_zone = if cat == "EMPTY_BOTTLE" { "BLOW_FLOOR" } else { "RM_STORE" };
        let zone = l.source_zone.clone().unwrap_or_else(|| want_zone.to_string());
        if zone != want_zone {
            return Err(AppError::field(&f("source_zone"), &format!("{cat} materials must be drawn from {want_zone}")));
        }
        if kind == "RETURNABLE" && (cat == "PREFORM" || cat == "EMPTY_BOTTLE") {
            return Err(AppError::field(&f("raw_material_id"), "RETURNABLE products never consume preforms or empty bottles"));
        }
        clean.push((l.raw_material_id, l.qty_per_crate, basis, zone));
    }
    sqlx::query("DELETE FROM bom_items WHERE product_id = $1").bind(id).execute(&mut *tx).await?;
    for (m, q, basis, zone) in &clean {
        sqlx::query("INSERT INTO bom_items (product_id, raw_material_id, qty_per_crate, basis, source_zone) VALUES ($1,$2,$3,$4,$5)")
            .bind(id).bind(m).bind(q).bind(basis).bind(zone).execute(&mut *tx).await?;
    }
    let after = bom_lines(&mut tx, id).await?;
    audit(&mut tx, &actor, &ci, "BOM_REPLACE", "product", Some(id.to_string()), json!({ "before": before, "after": after })).await?;
    tx.commit().await?;
    Ok(Json(after))
}
