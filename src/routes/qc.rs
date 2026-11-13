//! `/api/qc/*` – Lab QC (OWNER B, page `qc`). The decision is final: batch row `FOR UPDATE`, only from QUARANTINE, approve needs the
//! latest test to PASS, the REMAINING lot balance in QC_QUARANTINE moves (never `batch.crates`).
use crate::{
    alerts, audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    routes::ref_data::bh::{self, batch_j, ok, Arg, Id, J, Q, D},
    routes::production::batch_detail,
    state::AppState,
    stock::{self, ItemType},
    validate,
};
use axum::{extract::State, http::StatusCode, routing::{get, post}, Json, Router};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/batches", get(list))
        .route("/batches/{id}", get(get_one))
        .route("/batches/{id}/tests", post(add_test))
        .route("/batches/{id}/approve", post(approve))
        .route("/batches/{id}/reject", post(reject))
}

#[derive(Deserialize, Default)]
struct ListQ { location_id: Option<String>, status: Option<String>, limit: Option<i64>, offset: Option<i64> }

async fn list(State(s): State<AppState>, user: AuthUser, Q(q): Q<ListQ>) -> AppResult<Json<Value>> {
    let locs = { let mut c = s.db.acquire().await?; bh::list_scope(&mut c, &user, &q.location_id).await? };
    let status = match q.status.as_deref() {
        None => Some("QUARANTINE".to_string()),
        Some("ALL") => None,
        Some(x) if ["QUARANTINE", "APPROVED", "REJECTED"].contains(&x) => Some(x.to_string()),
        _ => return Err(AppError::field("status", "must be QUARANTINE, APPROVED, REJECTED or ALL")),
    };
    let pq = validate::PageQuery { limit: q.limit, offset: q.offset };
    let base = concat!("SELECT ", batch_j!(), " || jsonb_build_object('age_minutes', floor(extract(epoch FROM (now() - b.created_at)) / 60)::bigint,
            'last_test_result', (SELECT t.result FROM qc_tests t WHERE t.batch_id = b.id ORDER BY t.tested_at DESC, t.id DESC LIMIT 1),
            'tests_count', (SELECT count(*) FROM qc_tests t WHERE t.batch_id = b.id)) AS j,
            b.created_at AS ca, b.qc_decided_at AS dec, b.id AS bid
         FROM production_batches b JOIN products p ON p.id = b.product_id WHERE b.location_id = ANY($2) AND ($1::text IS NULL OR b.qc_status = $1)");
    // oldest waiting first; decided lists newest first
    let order = if status.as_deref() == Some("QUARANTINE") { "ca ASC, bid" } else { "dec DESC NULLS LAST, ca DESC, bid" };
    Ok(Json(bh::paged(&s.db, base, order, &[Arg::T(status), Arg::UA(locs)], pq.limit(), pq.offset()).await?))
}

async fn get_one(State(s): State<AppState>, user: AuthUser, Id(id): Id) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let v = batch_detail(&mut c, id).await?;
    user.require_location(crate::routes::production::batches::loc_of(&v))?;
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestReq {
    tds_ppm: Option<D>, ph: Option<D>, ozone_ppm: Option<D>, turbidity_ntu: Option<D>,
    microbial_ok: Option<bool>, seal_ok: Option<bool>, label_ok: Option<bool>, result: String, remarks: Option<String>,
}

fn bounded(field: &str, d: &Option<D>, hi: i64) -> AppResult<Option<Decimal>> {
    match d {
        None => Ok(None),
        Some(D(v)) => {
            if *v < Decimal::ZERO || *v > Decimal::from(hi) { return Err(AppError::field(field, &format!("must be between 0 and {hi}"))); }
            if v.scale() > 3 { return Err(AppError::field(field, "has too many decimal places")); }
            Ok(Some(*v))
        }
    }
}

struct Locked { returnable: bool, loc: Uuid, status: String, product_id: Uuid, batch_no: String, crates: i32, product_name: String }

async fn lock_batch(tx: &mut sqlx::PgConnection, user: &AuthUser, id: Uuid) -> AppResult<Locked> {
    let r = sqlx::query("SELECT b.location_id, b.qc_status, b.product_id, b.batch_no, b.crates, p.name, p.kind FROM production_batches b JOIN products p ON p.id = b.product_id WHERE b.id=$1 FOR UPDATE OF b")
        .bind(id).fetch_optional(&mut *tx).await?.ok_or_else(|| AppError::not_found("batch"))?;
    user.require_location(r.get::<Uuid, _>("location_id"))?;
    Ok(Locked { returnable: r.get::<String, _>("kind") == "RETURNABLE", loc: r.get("location_id"), status: r.get("qc_status"), product_id: r.get("product_id"), batch_no: r.get("batch_no"), crates: r.get("crates"), product_name: r.get("name") })
}

fn require_quarantine(l: &Locked) -> AppResult<()> {
    if l.status != "QUARANTINE" {
        return Err(AppError::conflict("INVALID_STATE", format!("This batch was already decided ({}). QC decisions are final.", l.status)));
    }
    Ok(())
}

async fn add_test(State(s): State<AppState>, user: AuthUser, Id(id): Id, J(b): J<TestReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let result = match b.result.as_str() { "PASS" => "PASS", "FAIL" => "FAIL", _ => return Err(AppError::field("result", "must be PASS or FAIL")) };
    let tds = bounded("tds_ppm", &b.tds_ppm, 10000)?;
    let ph = bounded("ph", &b.ph, 14)?;
    let ozone = bounded("ozone_ppm", &b.ozone_ppm, 100)?;
    let turb = bounded("turbidity_ntu", &b.turbidity_ntu, 1000)?;
    let remarks = bh::opt_text("remarks", &b.remarks, 500)?;
    let mut tx = s.db.begin().await?;
    let l = lock_batch(&mut tx, &user, id).await?;
    require_quarantine(&l)?;
    sqlx::query("INSERT INTO qc_tests (batch_id, tested_at, tested_by, tds_ppm, ph, ozone_ppm, turbidity_ntu, microbial_ok, seal_ok, label_ok, result, remarks)
                 VALUES ($1, clock_timestamp(), $2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
        .bind(id).bind(user.id).bind(tds).bind(ph).bind(ozone).bind(turb).bind(b.microbial_ok).bind(b.seal_ok).bind(b.label_ok).bind(result).bind(&remarks)
        .execute(&mut *tx).await?;
    audit::log(&mut *tx, Some(&user), "QC_TEST", "production_batch", Some(id.to_string()), json!({ "batch_no": l.batch_no, "result": result })).await;
    let v = batch_detail(&mut tx, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(v)))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ApproveReq { notes: Option<String> }

async fn approve(State(s): State<AppState>, user: AuthUser, Id(id): Id, body: axum::body::Bytes) -> AppResult<(StatusCode, Json<Value>)> {
    let req: ApproveReq = if body.iter().all(|b| b.is_ascii_whitespace()) { ApproveReq::default() }
        else { serde_json::from_slice(&body).map_err(|_| AppError::bad("Invalid request body"))? };
    let notes = bh::opt_text("notes", &req.notes, 500)?;
    let mut tx = s.db.begin().await?;
    let l = lock_batch(&mut tx, &user, id).await?;
    require_quarantine(&l)?;
    // lock order shared with loading: batch row (above) -> QR sets -> stock
    sqlx::query("SELECT id FROM qr_crate_sets WHERE batch_id=$1 ORDER BY id FOR UPDATE").bind(id).execute(&mut *tx).await?;
    let last: Option<String> = sqlx::query_scalar("SELECT result FROM qc_tests WHERE batch_id=$1 ORDER BY tested_at DESC, id DESC LIMIT 1").bind(id).fetch_optional(&mut *tx).await?;
    match last.as_deref() {
        None => return Err(AppError::conflict("INVALID_STATE", "QC test required: record a passing lab test before approving")),
        Some("PASS") => {}
        Some(_) => return Err(AppError::conflict("INVALID_STATE", "The latest QC test FAILED: record a passing test before approving, or reject the batch")),
    }
    let rem: Decimal = sqlx::query_scalar(
        "SELECT COALESCE((SELECT qty FROM stock_balances WHERE item_type='PRODUCT' AND item_id=$1 AND zone='QC_QUARANTINE' AND lot_id=$2 AND location_id=$3 FOR UPDATE), 0)")
        .bind(l.product_id).bind(id).bind(l.loc).fetch_one(&mut *tx).await?;
    if rem > Decimal::ZERO {
        stock::transfer_between(&mut tx, ItemType::Product, l.product_id, id, l.loc, "QC_QUARANTINE", l.loc, "FG_READY", rem, "QC_RELEASE", Some("QC"), Some(id), Some(user.id), &format!("QC approved {}", l.batch_no)).await?;
    }
    sqlx::query("UPDATE production_batches SET qc_status='APPROVED', qc_decided_by=$2, qc_decided_at=now(), qc_notes=$3 WHERE id=$1").bind(id).bind(user.id).bind(&notes).execute(&mut *tx).await?;
    sqlx::query("UPDATE qr_crate_sets SET status='AVAILABLE' WHERE batch_id=$1 AND status='QUARANTINE'").bind(id).execute(&mut *tx).await?;
    alerts::resolve(&mut *tx, "QC_PENDING", &id.to_string()).await?;
    alerts::notify_once(&mut *tx, alerts::NewAlert {
        event_type: "QC_APPROVED", dedupe_key: &id.to_string(), severity: "INFO", title: &format!("Batch {} approved", l.batch_no),
        body: &format!("{} crates of {} released to finished goods", rem.normalize(), l.product_name), url: "/warehouse.html", payload: json!({ "batch_id": id }),
    }).await?;
    audit::log(&mut *tx, Some(&user), "QC_APPROVE", "production_batch", Some(id.to_string()),
        json!({ "batch_no": l.batch_no, "released_crates": rem.to_string(), "batch_crates": l.crates, "notes": notes })).await;
    let v = batch_detail(&mut tx, id).await?;
    tx.commit().await?;
    ok(v)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RejectReq { reason: String }

async fn reject(State(s): State<AppState>, user: AuthUser, Id(id): Id, J(b): J<RejectReq>) -> AppResult<(StatusCode, Json<Value>)> {
    let reason = validate::required_text("reason", &b.reason, 500)?;
    if reason.chars().count() < 5 { return Err(AppError::field("reason", "must be at least 5 characters")); }
    let mut tx = s.db.begin().await?;
    let l = lock_batch(&mut tx, &user, id).await?;
    require_quarantine(&l)?;
    // lock order shared with loading: batch row (above) -> QR sets -> stock
    sqlx::query("SELECT id FROM qr_crate_sets WHERE batch_id=$1 ORDER BY id FOR UPDATE").bind(id).execute(&mut *tx).await?;
    let rem: Decimal = sqlx::query_scalar(
        "SELECT COALESCE((SELECT qty FROM stock_balances WHERE item_type='PRODUCT' AND item_id=$1 AND zone='QC_QUARANTINE' AND lot_id=$2 AND location_id=$3 FOR UPDATE), 0)")
        .bind(l.product_id).bind(id).bind(l.loc).fetch_one(&mut *tx).await?;
    if rem > Decimal::ZERO {
        stock::transfer_between(&mut tx, ItemType::Product, l.product_id, id, l.loc, "QC_QUARANTINE", l.loc, "SCRAP_YARD", rem, "QC_REJECT", Some("QC"), Some(id), Some(user.id), &format!("QC rejected {}: {reason}", l.batch_no)).await?;
    }
    if l.returnable && rem > Decimal::ZERO {
        // the filled containers of a rejected batch leave the fleet: keep the fleet analytics true
        let q = rem.trunc().to_string().parse::<i32>().map_err(|_| AppError::bad("quantity out of range"))?;
        sqlx::query("INSERT INTO jar_fleet_events (product_id, event_type, qty, reason_code, location, note, created_by, location_id) VALUES ($1,'DESTROYED',$2,'OTHER','PLANT_CLEAN',$3,$4,$5)")
            .bind(l.product_id).bind(q).bind(format!("QC reject {}", l.batch_no)).bind(user.id).bind(l.loc).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE production_batches SET qc_status='REJECTED', qc_decided_by=$2, qc_decided_at=now(), qc_notes=$3 WHERE id=$1").bind(id).bind(user.id).bind(&reason).execute(&mut *tx).await?;
    sqlx::query("UPDATE qr_crate_sets SET status='REJECTED' WHERE batch_id=$1 AND status IN ('QUARANTINE','AVAILABLE')").bind(id).execute(&mut *tx).await?;
    alerts::resolve(&mut *tx, "QC_PENDING", &id.to_string()).await?;
    alerts::notify_once(&mut *tx, alerts::NewAlert {
        event_type: "QC_REJECTED", dedupe_key: &id.to_string(), severity: "CRITICAL", title: &format!("Batch {} rejected", l.batch_no),
        body: &reason, url: "/qc.html", payload: json!({ "batch_id": id }),
    }).await?;
    audit::log(&mut *tx, Some(&user), "QC_REJECT", "production_batch", Some(id.to_string()),
        json!({ "batch_no": l.batch_no, "scrapped_crates": rem.to_string(), "reason": reason })).await;
    let v = batch_detail(&mut tx, id).await?;
    tx.commit().await?;
    ok(v)
}
