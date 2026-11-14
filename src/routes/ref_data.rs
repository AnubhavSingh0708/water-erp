//! `/api/ref/*` – read-only reference data for dropdowns (any logged-in user). Active rows only, bounded to 500 rows.
use crate::{auth::AuthUser, error::AppResult, state::AppState, stock};
use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};

pub mod bh;
use bh::jval;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/products", get(products))
        .route("/raw-materials", get(raw_materials))
        .route("/suppliers", get(suppliers))
        .route("/customers", get(customers))
        .route("/vehicles", get(vehicles))
        .route("/routes", get(routes))
        .route("/equipment", get(equipment))
        .route("/locations", get(locations))
        .route("/zones", get(zones))
        .route("/settings", get(settings))
}

async fn products(State(s): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY n), '[]'::jsonb) FROM (
           SELECT to_jsonb(p) || jsonb_build_object('min_stock_crates', p.min_stock_crates::text) AS j, p.name AS n
           FROM products p WHERE p.is_active ORDER BY p.name LIMIT 500) t", &[]).await?))
}

async fn raw_materials(State(s): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY n), '[]'::jsonb) FROM (
           SELECT to_jsonb(m) || jsonb_build_object('stock_per_purchase_unit', m.stock_per_purchase_unit::text,
                  'unit_weight_g', m.unit_weight_g::text, 'min_stock', m.min_stock::text) AS j, m.name AS n
           FROM raw_materials m WHERE m.is_active ORDER BY m.name LIMIT 500) t", &[]).await?))
}

async fn suppliers(State(s): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY n), '[]'::jsonb) FROM (
           SELECT to_jsonb(x) AS j, x.name AS n FROM suppliers x WHERE x.is_active ORDER BY x.name LIMIT 500) t", &[]).await?))
}

async fn customers(State(s): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY n), '[]'::jsonb) FROM (
           SELECT to_jsonb(x) AS j, x.name AS n FROM customers x WHERE x.is_active ORDER BY x.name LIMIT 500) t", &[]).await?))
}

async fn vehicles(State(s): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY n), '[]'::jsonb) FROM (
           SELECT to_jsonb(x) AS j, x.reg_no AS n FROM vehicles x WHERE x.is_active ORDER BY x.reg_no LIMIT 500) t", &[]).await?))
}

async fn routes(State(s): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY n), '[]'::jsonb) FROM (
           SELECT to_jsonb(x) AS j, x.name AS n FROM routes x WHERE x.is_active ORDER BY x.name LIMIT 500) t", &[]).await?))
}

async fn equipment(State(s): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(jval(&s.db,
        "SELECT COALESCE(jsonb_agg(j ORDER BY n), '[]'::jsonb) FROM (
           SELECT to_jsonb(x) AS j, x.name AS n FROM equipment x WHERE x.is_active ORDER BY x.name LIMIT 500) t", &[]).await?))
}

async fn zones() -> Json<Value> { Json(json!(stock::ZONES)) }

/// Public subset of the settings (no secrets, no URLs).
async fn settings(State(s): State<AppState>) -> Json<Value> {
    let c = s.settings.get();
    Json(json!({
        "plant_name": c.plant_name, "plant_code": c.plant_code, "default_crate_set_size": c.default_crate_set_size,
        "strict_qc_lock": c.strict_qc_lock, "jar_overdue_days": c.jar_overdue_days, "expiry_lead_days": c.expiry_lead_days,
        "qr_labels_enabled": c.qr_labels_enabled, "utc_offset_minutes": c.utc_offset_minutes,
        "large_adjust_pct": c.large_adjust_pct, "scrap_warn_pct": c.scrap_warn_pct, "void_window_hours": c.void_window_hours,
        "damage_warn_pct": c.damage_warn_pct, "jar_destroy_warn_pct": c.jar_destroy_warn_pct,
        "ph_min": c.ph_min.to_string(), "ph_max": c.ph_max.to_string(), "tds_min": c.tds_min.to_string(), "tds_max": c.tds_max.to_string(),
    }))
}

/// Active selectable godowns the caller may use (TRANSIT is a system location and never listed).
/// `?all=1` returns ONLY id/code/name/kind of every active godown (no contact details) so a user restricted to
/// some godowns can still pick a *destination* for an inter-godown transfer. Their own access rules still apply when they act.
async fn locations(State(s): State<AppState>, user: AuthUser, axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    if matches!(q.get("all").map(String::as_str), Some("1") | Some("true")) {
        return Ok(Json(jval(&mut *c,
            "SELECT COALESCE(jsonb_agg(j ORDER BY n), '[]'::jsonb) FROM (
               SELECT jsonb_build_object('id', l.id, 'code', l.code, 'name', l.name, 'kind', l.kind) AS j, l.name AS n
               FROM locations l WHERE l.is_active AND l.kind <> 'TRANSIT' ORDER BY l.name LIMIT 500) t", &[]).await?));
    }
    let allowed = bh::allowed_locations(&mut c, &user).await?;
    Ok(Json(jval(&mut *c,
        "SELECT COALESCE(jsonb_agg(j ORDER BY n), '[]'::jsonb) FROM (
           SELECT jsonb_build_object('id', l.id, 'code', l.code, 'name', l.name, 'kind', l.kind, 'address', l.address, 'city', l.city,
                  'contact_person', l.contact_person, 'phone', l.phone, 'is_production_site', l.is_production_site, 'is_active', l.is_active) AS j, l.name AS n
           FROM locations l WHERE l.is_active AND l.kind <> 'TRANSIT' AND l.id = ANY($1) ORDER BY l.name LIMIT 500) t", &[bh::Arg::UA(allowed)]).await?))
}
