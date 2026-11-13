//! `/api/production/*` – blow molding, water treatment logs, bottling batches, traceability, summary (OWNER B, page `production`).
//! Batches, trace and summary live in `production/batches.rs`.
use crate::{
    alerts, audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    routes::ref_data::bh::{self, created, finish_dry, local_date, Arg, DryQ, Id, J, Q, D},
    state::AppState,
    stock::{self, ItemType, Move},
    util, validate,
    workers::reorder,
};
use axum::{extract::State, http::StatusCode, response::Response, routing::get, Json, Router};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use uuid::Uuid;

pub mod batches;
pub use batches::batch_detail;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/blow-runs", get(blow_list).post(blow_create))
        .route("/blow-runs/{id}", get(blow_get))
        .route("/water-logs", get(water_list).post(water_create))
        .route("/water-logs/status", get(water_status))
        .route("/bom-check", get(batches::bom_check))
        .route("/batches", get(batches::list).post(batches::create))
        .route("/batches/{id}", get(batches::get))
        .route("/batches/{id}/trace", get(batches::trace))
        .route("/summary", get(batches::summary))
}

// ------------------------------------------------------------------------------------------------------------------
// Blow molding
// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlowReq {
    run_date: Option<NaiveDate>, shift: String, equipment_id: Option<Uuid>, preform_material_id: Uuid, bottle_material_id: Uuid,
    preforms_issued: D, good_bottles: D, notes: Option<String>,
}

async fn blow_json(conn: &mut PgConnection, id: Uuid) -> AppResult<Value> {
    let v = bh::jval(&mut *conn,
        "SELECT jsonb_build_object('id', r.id, 'run_no', r.run_no, 'run_date', r.run_date, 'shift', r.shift, 'equipment_id', r.equipment_id, 'equipment_name', e.name,
            'preform_material_id', r.preform_material_id, 'preform_name', pm.name, 'bottle_material_id', r.bottle_material_id, 'bottle_name', bm.name,
            'preforms_issued', r.preforms_issued::text, 'good_bottles', r.good_bottles::text, 'scrap_pcs', r.scrap_pcs::text, 'scrap_pct', r.scrap_pct::text,
            'scrap_weight_kg', r.scrap_weight_kg::text, 'notes', r.notes, 'created_at', r.created_at, 'created_by_name', u.full_name)
         FROM blow_molding_runs r JOIN raw_materials pm ON pm.id = r.preform_material_id JOIN raw_materials bm ON bm.id = r.bottle_material_id
         LEFT JOIN equipment e ON e.id = r.equipment_id LEFT JOIN users u ON u.id = r.created_by WHERE r.id = $1", &[Arg::U(Some(id))]).await;
    match v { Ok(v) if !v.is_null() => Ok(v), Ok(_) | Err(AppError::NotFound(_)) => Err(AppError::not_found("blow run")), Err(e) => Err(e) }
}

async fn blow_create(State(s): State<AppState>, user: AuthUser, Q(dq): Q<DryQ>, J(b): J<BlowReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let dry = dq.on();
    let shift = bh::shift(&b.shift)?;
    let issued = bh::whole_qty("preforms_issued", b.preforms_issued.0)?;
    validate::qty_non_negative("good_bottles", b.good_bottles.0)?;
    if b.good_bottles.0.fract() != Decimal::ZERO { return Err(AppError::field("good_bottles", "must be a whole number")); }
    let good = b.good_bottles.0;
    if good > issued { return Err(AppError::field("good_bottles", "cannot be more than the preforms issued")); }
    let notes = bh::opt_text("notes", &b.notes, 500)?;
    let scrap = issued - good;
    let scrap_pct = (scrap * Decimal::from(100) / issued).round_dp(3);
    let st = s.settings.get();
    let mut warnings = vec![];
    if scrap_pct > Decimal::from(st.scrap_warn_pct) {
        if notes.is_empty() {
            return Err(AppError::field("notes", &format!("a note explaining the scrap is required when scrap is above {}%", st.scrap_warn_pct)));
        }
        warnings.push(format!("Scrap {}% is above the {}% limit", scrap_pct.normalize(), st.scrap_warn_pct));
    }
    let mut tx = s.db.begin().await?;
    let today = util::plant_today(&mut tx).await?;
    let run_date = b.run_date.unwrap_or(today);
    bh::check_entry_date("run_date", run_date, today, 31)?;
    let pre = sqlx::query("SELECT category, is_active, unit_weight_g FROM raw_materials WHERE id=$1").bind(b.preform_material_id).fetch_optional(&mut *tx).await?;
    let pre = pre.filter(|r| r.get::<bool, _>("is_active") && r.get::<String, _>("category") == "PREFORM")
        .ok_or_else(|| AppError::field("preform_material_id", "must be an active PREFORM material"))?;
    let bot: Option<(String, bool)> = sqlx::query_as("SELECT category, is_active FROM raw_materials WHERE id=$1").bind(b.bottle_material_id).fetch_optional(&mut *tx).await?;
    if !bot.map(|x| x.1 && x.0 == "EMPTY_BOTTLE").unwrap_or(false) { return Err(AppError::field("bottle_material_id", "must be an active EMPTY_BOTTLE material")); }
    if let Some(e) = b.equipment_id {
        let a: Option<bool> = sqlx::query_scalar("SELECT is_active FROM equipment WHERE id=$1").bind(e).fetch_optional(&mut *tx).await?;
        if a != Some(true) { return Err(AppError::field("equipment_id", "unknown or inactive equipment")); }
    }
    let weight_kg: Option<Decimal> = pre.get::<Option<Decimal>, _>("unit_weight_g").map(|w| (scrap * w / Decimal::from(1000)).round_dp(4));
    let id = Uuid::new_v4();
    let run_no = bh::doc_no(&mut tx, "blow_seq", "BLW", dry).await?;
    sqlx::query("INSERT INTO blow_molding_runs (id, run_no, run_date, shift, equipment_id, preform_material_id, bottle_material_id, preforms_issued, good_bottles,
                 scrap_pcs, scrap_pct, scrap_weight_kg, notes, created_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)")
        .bind(id).bind(&run_no).bind(run_date).bind(shift).bind(b.equipment_id).bind(b.preform_material_id).bind(b.bottle_material_id)
        .bind(issued).bind(good).bind(scrap).bind(scrap_pct).bind(weight_kg).bind(&notes).bind(user.id).execute(&mut *tx).await?;
    stock::consume_fefo(&mut tx, ItemType::Raw, b.preform_material_id, "RM_STORE", issued, "BLOW_ISSUE", Some("BLOW"), Some(id), Some(user.id), &run_no).await?;
    let mut moves = vec![];
    if good > Decimal::ZERO {
        moves.push(Move::new(ItemType::Raw, b.bottle_material_id, "BLOW_FLOOR", stock::NIL_LOT, good, "BLOW_OUTPUT").reference("BLOW", id).by(Some(user.id)).note(run_no.clone()));
    }
    if scrap > Decimal::ZERO {
        moves.push(Move::new(ItemType::Raw, b.preform_material_id, "SCRAP_YARD", stock::NIL_LOT, scrap, "BLOW_SCRAP").reference("BLOW", id).by(Some(user.id)).note(run_no.clone()));
    }
    if !moves.is_empty() { stock::post_all(&mut tx, moves).await?; }
    if dry { return finish_dry(tx, "BLOW", id, &warnings).await; }
    audit::log(&mut *tx, Some(&user), "BLOW_RUN", "blow_run", Some(id.to_string()),
        json!({ "run_no": run_no, "issued": issued.to_string(), "good": good.to_string(), "scrap_pct": scrap_pct.to_string() })).await;
    let mut v = blow_json(&mut tx, id).await?;
    tx.commit().await?;
    v["warnings"] = json!(warnings);
    spawn_reorder_check(&s, vec![b.preform_material_id]);
    created(v)
}

/// Best-effort, after commit: re-evaluate the reorder engine for materials a production flow just consumed.
pub(crate) fn spawn_reorder_check(s: &AppState, ids: Vec<Uuid>) {
    let s = s.clone();
    tokio::spawn(async move {
        if let Ok(mut tx) = s.db.begin().await {
            if reorder::evaluate_materials(&mut tx, &s.settings.get(), &ids).await.is_ok() { let _ = tx.commit().await; }
        }
    });
}

#[derive(Deserialize, Default)]
struct RangeQ { from: Option<NaiveDate>, to: Option<NaiveDate>, format: Option<String>, limit: Option<i64>, offset: Option<i64> }

async fn blow_list(State(s): State<AppState>, Q(q): Q<RangeQ>) -> AppResult<Response> {
    let csv = bh::is_csv(&q.format);
    let pq = validate::PageQuery { limit: q.limit, offset: q.offset };
    let (limit, offset) = if csv { (bh::CSV_MAX_ROWS, 0) } else { (pq.limit(), pq.offset()) };
    let v = bh::paged(&s.db,
        "SELECT jsonb_build_object('id', r.id, 'run_no', r.run_no, 'run_date', r.run_date, 'shift', r.shift, 'equipment_name', e.name,
            'preform_name', pm.name, 'bottle_name', bm.name, 'preforms_issued', r.preforms_issued::text, 'good_bottles', r.good_bottles::text,
            'scrap_pcs', r.scrap_pcs::text, 'scrap_pct', r.scrap_pct::text, 'scrap_weight_kg', r.scrap_weight_kg::text, 'notes', r.notes, 'created_at', r.created_at) AS j,
            r.run_date AS rd, r.created_at AS ca, r.id AS rid
         FROM blow_molding_runs r JOIN raw_materials pm ON pm.id = r.preform_material_id JOIN raw_materials bm ON bm.id = r.bottle_material_id
         LEFT JOIN equipment e ON e.id = r.equipment_id
         WHERE ($1::date IS NULL OR r.run_date >= $1) AND ($2::date IS NULL OR r.run_date <= $2)",
        "rd DESC, ca DESC, rid", &[Arg::D(q.from), Arg::D(q.to)], limit, offset).await?;
    if csv {
        return Ok(bh::csv_response("blow-runs.csv", &[("Run", "run_no"), ("Date", "run_date"), ("Shift", "shift"), ("Machine", "equipment_name"),
            ("Preform", "preform_name"), ("Bottle", "bottle_name"), ("Preforms issued", "preforms_issued"), ("Good bottles", "good_bottles"),
            ("Scrap pcs", "scrap_pcs"), ("Scrap %", "scrap_pct"), ("Scrap kg", "scrap_weight_kg"), ("Notes", "notes")], bh::items_of(&v)));
    }
    Ok(bh::json_resp(v))
}

async fn blow_get(State(s): State<AppState>, Id(id): Id) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    Ok(Json(blow_json(&mut c, id).await?))
}

// ------------------------------------------------------------------------------------------------------------------
// Water treatment logs
// ------------------------------------------------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaterReq { tds_ppm: D, ph: D, ozone_ppm: Option<D>, uv_ok: bool, #[serde(default)] minerals: BTreeMap<String, D>, notes: Option<String> }

fn range_check(field: &str, d: Decimal, lo: i64, hi: i64) -> AppResult<()> {
    if d < Decimal::from(lo) || d > Decimal::from(hi) { return Err(AppError::field(field, &format!("must be between {lo} and {hi}"))); }
    if d.scale() > 3 { return Err(AppError::field(field, "has too many decimal places")); }
    Ok(())
}

async fn water_create(State(s): State<AppState>, user: AuthUser, J(b): J<WaterReq>) -> AppResult<(StatusCode, Json<Value>)> {
    range_check("tds_ppm", b.tds_ppm.0, 0, 10000)?;
    range_check("ph", b.ph.0, 0, 14)?;
    if let Some(o) = b.ozone_ppm { range_check("ozone_ppm", o.0, 0, 100)?; }
    let notes = bh::opt_text("notes", &b.notes, 500)?;
    if b.minerals.len() > 20 { return Err(AppError::field("minerals", "at most 20 entries")); }
    let mut minerals = serde_json::Map::new();
    for (k, v) in &b.minerals {
        let name = validate::required_text("minerals", k, 60)?;
        validate::qty_non_negative("minerals", v.0)?;
        minerals.insert(name, serde_json::from_str::<Value>(&v.0.normalize().to_string()).unwrap_or(Value::Null));
    }
    let st = s.settings.get();
    let mut reasons = vec![];
    if b.ph.0 < st.ph_min || b.ph.0 > st.ph_max { reasons.push(format!("pH {} outside {}-{}", b.ph.0.normalize(), st.ph_min.normalize(), st.ph_max.normalize())); }
    if b.tds_ppm.0 < st.tds_min || b.tds_ppm.0 > st.tds_max { reasons.push(format!("TDS {} ppm outside {}-{}", b.tds_ppm.0.normalize(), st.tds_min.normalize(), st.tds_max.normalize())); }
    let oor = !reasons.is_empty();
    let mut warnings = reasons.clone();
    if !b.uv_ok { warnings.push("UV lamp reported not OK".to_string()); }
    let mut tx = s.db.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO water_treatment_logs (tds_ppm, ph, ozone_ppm, uv_ok, minerals, out_of_range, notes, logged_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id")
        .bind(b.tds_ppm.0).bind(b.ph.0).bind(b.ozone_ppm.map(|o| o.0)).bind(b.uv_ok).bind(Value::Object(minerals)).bind(oor).bind(&notes).bind(user.id)
        .fetch_one(&mut *tx).await?;
    if oor {
        alerts::notify_once(&mut *tx, alerts::NewAlert {
            event_type: "WATER_OUT_OF_RANGE", dedupe_key: &id.to_string(), severity: "CRITICAL", title: "Water treatment reading out of range",
            body: &reasons.join("; "), url: "/production.html", payload: json!({ "water_log_id": id }),
        }).await?;
    }
    let mut v = water_one(&mut tx, id).await?;
    tx.commit().await?;
    v["warnings"] = json!(warnings);
    created(v)
}

async fn water_one(conn: &mut PgConnection, id: Uuid) -> AppResult<Value> {
    bh::jval(&mut *conn, concat!("SELECT ", "jsonb_build_object('id', w.id, 'logged_at', w.logged_at, 'tds_ppm', w.tds_ppm::text, 'ph', w.ph::text, 'ozone_ppm', w.ozone_ppm::text,
    'uv_ok', w.uv_ok, 'minerals', w.minerals, 'out_of_range', w.out_of_range, 'notes', w.notes, 'logged_by_name', u.full_name)",
        " FROM water_treatment_logs w LEFT JOIN users u ON u.id = w.logged_by WHERE w.id = $1"), &[Arg::U(Some(id))]).await
}

async fn water_list(State(s): State<AppState>, Q(q): Q<RangeQ>) -> AppResult<Json<Value>> {
    let pq = validate::PageQuery { limit: q.limit, offset: q.offset };
    let off = s.settings.get().utc_offset_minutes;
    let v = bh::paged(&s.db,
        concat!("SELECT jsonb_build_object('id', w.id, 'logged_at', w.logged_at, 'tds_ppm', w.tds_ppm::text, 'ph', w.ph::text, 'ozone_ppm', w.ozone_ppm::text,
    'uv_ok', w.uv_ok, 'minerals', w.minerals, 'out_of_range', w.out_of_range, 'notes', w.notes, 'logged_by_name', u.full_name) AS j, w.logged_at AS la, w.id AS wid
         FROM water_treatment_logs w LEFT JOIN users u ON u.id = w.logged_by
         WHERE ($1::date IS NULL OR ", local_date!("w.logged_at", "$3"), " >= $1) AND ($2::date IS NULL OR ", local_date!("w.logged_at", "$3"), " <= $2)"),
        "la DESC, wid", &[Arg::D(q.from), Arg::D(q.to), Arg::I(off)], pq.limit(), pq.offset()).await?;
    Ok(Json(v))
}

async fn water_status(State(s): State<AppState>) -> AppResult<Json<Value>> {
    let v = bh::jval(&s.db,
        "SELECT jsonb_build_object('last_logged_at', t.at, 'minutes_since', floor(extract(epoch FROM (now() - t.at)) / 60)::bigint,
            'overdue', (t.at IS NULL OR t.at < now() - interval '60 minutes'), 'last_out_of_range', COALESCE(t.oor, false))
         FROM (SELECT (SELECT logged_at FROM water_treatment_logs ORDER BY logged_at DESC LIMIT 1) AS at,
                      (SELECT out_of_range FROM water_treatment_logs ORDER BY logged_at DESC LIMIT 1) AS oor) t", &[]).await?;
    Ok(Json(v))
}
