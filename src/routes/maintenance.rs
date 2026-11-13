//! `/api/maintenance/*` – equipment/vehicle service schedules and vehicle document expiry, page `maintenance`.
//! OWNER: Subagent C2. See API_REFERENCE.md §12.
//!
//! Status rule (plant-local date from `util::plant_today`): `OVERDUE` when the due/expiry date is before today, `DUE_SOON` when it is
//! within `alerts.expiry_lead_days` (inclusive, today counts as due), else `OK`.
use super::notifications::extract::{opt_uuid, ApiJson, ApiPath, ApiQuery};
use crate::{
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    state::AppState,
    util, validate,
};
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

pub const DOC_KINDS: &[&str] = &["INSURANCE", "FITNESS", "PUC", "PERMIT"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/schedules", get(schedules))
        .route("/schedules/{id}/complete", post(complete))
        .route("/schedules/{id}/logs", get(logs))
        .route("/vehicle-docs", get(vehicle_docs))
}

/// `OK` / `DUE_SOON` / `OVERDUE` for a date relative to today.
pub fn status_for(days_left: i64, lead_days: i64) -> &'static str {
    if days_left < 0 { "OVERDUE" } else if days_left <= lead_days { "DUE_SOON" } else { "OK" }
}

#[derive(Clone, Debug)]
pub struct Schedule {
    pub id: Uuid, pub subject_type: String, pub subject_name: String, pub task: String, pub interval_days: Option<i32>,
    pub last_done_on: Option<NaiveDate>, pub next_due_on: NaiveDate, pub days_left: i64, pub status: &'static str,
}
impl Schedule {
    fn json(&self) -> Value {
        json!({ "id": self.id, "subject_type": self.subject_type, "subject_name": self.subject_name, "task": self.task, "interval_days": self.interval_days,
                "last_done_on": self.last_done_on, "next_due_on": self.next_due_on, "days_left": self.days_left, "status": self.status })
    }
}

const SCHEDULE_SELECT: &str =
    "SELECT s.id, s.subject_type, COALESCE(e.name, v.reg_no, '') AS subject_name, s.task, s.interval_days, s.last_done_on, s.next_due_on
     FROM maintenance_schedules s LEFT JOIN equipment e ON e.id = s.equipment_id LEFT JOIN vehicles v ON v.id = s.vehicle_id";

fn schedule_from(r: &sqlx::postgres::PgRow, today: NaiveDate, lead: i64) -> Schedule {
    let next: NaiveDate = r.get("next_due_on");
    let days_left = (next - today).num_days();
    Schedule {
        id: r.get("id"), subject_type: r.get("subject_type"), subject_name: r.get("subject_name"), task: r.get("task"), interval_days: r.get("interval_days"),
        last_done_on: r.get("last_done_on"), next_due_on: next, days_left, status: status_for(days_left, lead),
    }
}

/// Active schedules of active equipment/vehicles, soonest first.
pub async fn load_schedules(conn: &mut PgConnection, today: NaiveDate, lead: i64, subject_type: Option<&str>) -> AppResult<Vec<Schedule>> {
    let rows = sqlx::query(&format!(
        "{SCHEDULE_SELECT} WHERE s.is_active AND COALESCE(e.is_active, v.is_active, true) AND ($1::text IS NULL OR s.subject_type = $1)
         ORDER BY s.next_due_on, s.task, s.id LIMIT 5000"))
        .bind(subject_type).fetch_all(&mut *conn).await?;
    Ok(rows.iter().map(|r| schedule_from(r, today, lead)).collect())
}

#[derive(Clone, Debug)]
pub struct VehicleDoc { pub vehicle_id: Uuid, pub reg_no: String, pub doc: &'static str, pub expiry_date: NaiveDate, pub days_left: i64, pub status: &'static str }
impl VehicleDoc {
    fn json(&self) -> Value {
        json!({ "vehicle_id": self.vehicle_id, "reg_no": self.reg_no, "doc": self.doc, "expiry_date": self.expiry_date, "days_left": self.days_left,
                "status": self.status, "expired": self.days_left < 0 })
    }
}

/// Dated documents of active vehicles (documents without a date are not tracked and not listed).
pub async fn load_vehicle_docs(conn: &mut PgConnection, today: NaiveDate, lead: i64, vehicle: Option<Uuid>) -> AppResult<Vec<VehicleDoc>> {
    let rows = sqlx::query(
        "SELECT id, reg_no, insurance_expiry, fitness_expiry, puc_expiry, permit_expiry FROM vehicles
         WHERE is_active AND ($1::uuid IS NULL OR id = $1) ORDER BY reg_no, id LIMIT 2000").bind(vehicle).fetch_all(&mut *conn).await?;
    let mut out = vec![];
    for r in rows {
        for (doc, col) in [("INSURANCE", "insurance_expiry"), ("FITNESS", "fitness_expiry"), ("PUC", "puc_expiry"), ("PERMIT", "permit_expiry")] {
            if let Some(d) = r.get::<Option<NaiveDate>, _>(col) {
                let days_left = (d - today).num_days();
                out.push(VehicleDoc { vehicle_id: r.get("id"), reg_no: r.get("reg_no"), doc, expiry_date: d, days_left, status: status_for(days_left, lead) });
            }
        }
    }
    out.sort_by(|a, b| a.expiry_date.cmp(&b.expiry_date).then(a.reg_no.cmp(&b.reg_no)));
    Ok(out)
}

fn check_status(v: &Option<String>) -> AppResult<Option<&'static str>> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some("OK") => Ok(Some("OK")),
        Some("DUE_SOON") => Ok(Some("DUE_SOON")),
        Some("OVERDUE") => Ok(Some("OVERDUE")),
        Some(_) => Err(AppError::field("status", "must be OK, DUE_SOON or OVERDUE")),
    }
}

#[derive(Deserialize, Default)]
struct SchedulesQuery { subject_type: Option<String>, status: Option<String> }

async fn schedules(State(s): State<AppState>, _u: AuthUser, ApiQuery(q): ApiQuery<SchedulesQuery>) -> AppResult<Json<Value>> {
    let st = check_status(&q.status)?;
    let subject = match q.subject_type.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some("EQUIPMENT") => Some("EQUIPMENT"),
        Some("VEHICLE") => Some("VEHICLE"),
        Some(_) => return Err(AppError::field("subject_type", "must be EQUIPMENT or VEHICLE")),
    };
    let mut c = s.db.acquire().await?;
    let today = util::plant_today(&mut c).await?;
    let mut rows = load_schedules(&mut c, today, s.settings.get().expiry_lead_days, subject).await?;
    if let Some(st) = st { rows.retain(|r| r.status == st); }
    rows.truncate(500);
    Ok(Json(Value::Array(rows.iter().map(Schedule::json).collect())))
}

#[derive(Deserialize, Default)]
struct DocsQuery { status: Option<String>, vehicle_id: Option<String> }

async fn vehicle_docs(State(s): State<AppState>, _u: AuthUser, ApiQuery(q): ApiQuery<DocsQuery>) -> AppResult<Json<Value>> {
    let st = check_status(&q.status)?;
    let vehicle = opt_uuid("vehicle_id", &q.vehicle_id)?;
    let mut c = s.db.acquire().await?;
    let today = util::plant_today(&mut c).await?;
    let mut rows = load_vehicle_docs(&mut c, today, s.settings.get().expiry_lead_days, vehicle).await?;
    if let Some(st) = st { rows.retain(|r| r.status == st); }
    rows.truncate(500);
    Ok(Json(Value::Array(rows.iter().map(VehicleDoc::json).collect())))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteReq { done_on: Option<NaiveDate>, notes: Option<String>, next_due_on: Option<NaiveDate> }

async fn complete(State(s): State<AppState>, u: AuthUser, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<CompleteReq>) -> AppResult<Json<Value>> {
    let notes = match &b.notes { Some(n) => validate::text("notes", n, 500)?, None => String::new() };
    let mut tx = s.db.begin().await?;
    // the schedule row is the gate: lock it and decide on its CURRENT state
    let row = sqlx::query("SELECT is_active, interval_days, last_done_on, next_due_on FROM maintenance_schedules WHERE id = $1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?.ok_or_else(|| AppError::not_found("schedule"))?;
    if !row.get::<bool, _>("is_active") { return Err(AppError::conflict("INVALID_STATE", "This schedule is no longer active")); }
    let interval: Option<i32> = row.get("interval_days");
    let last_done: Option<NaiveDate> = row.get("last_done_on");
    let old_next: NaiveDate = row.get("next_due_on");
    let today = util::plant_today(&mut tx).await?;
    let done_on = b.done_on.unwrap_or(today);
    if done_on > today { return Err(AppError::field("done_on", "must not be in the future")); }
    if (today - done_on).num_days() > 3660 { return Err(AppError::field("done_on", "is too far in the past")); }
    if last_done.map_or(false, |l| done_on <= l) {
        return Err(AppError::conflict("INVALID_STATE", "This task was already recorded as done on or after that date. Refresh the page."));
    }
    let next = match (b.next_due_on, interval) {
        (Some(n), _) => n,
        (None, Some(i)) => done_on + chrono::Duration::days(i as i64),
        (None, None) => return Err(AppError::field("next_due_on", "is required for a schedule without an interval")),
    };
    if next <= done_on { return Err(AppError::field("next_due_on", "must be after the date the task was done")); }
    if (next - done_on).num_days() > 3660 { return Err(AppError::field("next_due_on", "is too far in the future")); }
    sqlx::query("UPDATE maintenance_schedules SET last_done_on = $2, next_due_on = $3 WHERE id = $1").bind(id).bind(done_on).bind(next).execute(&mut *tx).await?;
    let log_id: Uuid = sqlx::query_scalar("INSERT INTO maintenance_logs (schedule_id, done_on, done_by, notes, next_due_on) VALUES ($1,$2,$3,$4,$5) RETURNING id")
        .bind(id).bind(done_on).bind(u.id).bind(&notes).bind(next).fetch_one(&mut *tx).await?;
    crate::alerts::resolve(&mut *tx, "MAINTENANCE_DUE", &format!("schedule:{id}:{old_next}")).await?;
    audit::log(&mut *tx, Some(&u), "maintenance.complete", "maintenance_schedules", Some(id.to_string()),
               json!({ "log_id": log_id, "done_on": done_on, "next_due_on": next, "previous_due_on": old_next })).await;
    let r = sqlx::query(&format!("{SCHEDULE_SELECT} WHERE s.id = $1")).bind(id).fetch_one(&mut *tx).await?;
    let lead = s.settings.get().expiry_lead_days;
    let sched = schedule_from(&r, today, lead);
    tx.commit().await?;
    let mut out = sched.json();
    out["log_id"] = json!(log_id);
    Ok(Json(out))
}

async fn logs(State(s): State<AppState>, _u: AuthUser, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<Value>> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM maintenance_schedules WHERE id = $1)").bind(id).fetch_one(&s.db).await?;
    if !exists { return Err(AppError::not_found("schedule")); }
    let rows = sqlx::query(
        "SELECT l.id, l.done_on, l.notes, l.next_due_on, l.created_at, u.full_name AS done_by_name
         FROM maintenance_logs l LEFT JOIN users u ON u.id = l.done_by WHERE l.schedule_id = $1 ORDER BY l.done_on DESC, l.created_at DESC, l.id DESC LIMIT 200")
        .bind(id).fetch_all(&s.db).await?;
    Ok(Json(Value::Array(rows.iter().map(|r| json!({
        "id": r.get::<Uuid, _>("id"), "done_on": r.get::<NaiveDate, _>("done_on"), "done_by_name": r.get::<Option<String>, _>("done_by_name"),
        "notes": r.get::<String, _>("notes"), "next_due_on": r.get::<Option<NaiveDate>, _>("next_due_on"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
    })).collect())))
}
