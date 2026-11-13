//! Hourly alert engine for maintenance, vehicle documents, overdue containers and container-fleet health. OWNER: Subagent C2.
//!
//! Dedupe keys (one OPEN alert per key, API_REFERENCE.md §12):
//! * `MAINTENANCE_DUE`    `schedule:<schedule_id>:<next_due_on>`   – resolved when the schedule is completed/moved/deactivated
//! * `VEHICLE_DOC_EXPIRY` `vehicle:<vehicle_id>:<DOC>:<expiry>`     – resolved when the document is renewed (new date) or the vehicle deactivated
//! * `JAR_OVERDUE`        `<customer_id>:<product_id>`              – resolved when the customer is no longer overdue
//! * `JAR_FLEET_ALERT`    `<product_id>:HIGH_DESTROY_RATE` / `<product_id>:NEAR_RETIREMENT` (contract gives no key; chosen here)
//!   – raised when destroyed ÷ fills over the last 30 plant-local days exceeds `jars.destroy_warn_pct` or retirement >= 90 %; resolved when clear.
//! Each pass raises what is currently true and resolves, per event type, every open alert whose key is no longer current.
use crate::{
    alerts::{self, NewAlert},
    routes::{jars, maintenance as mt},
    state::AppState,
    util,
};
use serde_json::json;
use sqlx::PgConnection;
use std::{collections::HashSet, time::Duration};

const INTERVAL: Duration = Duration::from_secs(3600);

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        loop {
            match run_once(&state).await {
                Ok(s) => tracing::debug!(raised = s.raised, resolved = s.resolved, "maintenance/jar alert pass done"),
                Err(e) => tracing::warn!(error = ?e, "maintenance/jar alert pass failed"),
            }
            tokio::select! {
                _ = tokio::time::sleep(INTERVAL) => {}
                _ = state.shutdown.wait() => break,
            }
        }
    });
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RunStats { pub raised: usize, pub resolved: usize }

/// One full pass (also used by tests). A failing section is logged and does not stop the others.
pub async fn run_once(state: &AppState) -> anyhow::Result<RunStats> {
    let mut conn = state.db.acquire().await?;
    let today = util::plant_today(&mut conn).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut total = RunStats::default();
    let sections: [(&str, anyhow::Result<RunStats>); 4] = [
        ("maintenance", maintenance_due(&mut conn, state, today).await),
        ("vehicle documents", vehicle_docs(&mut conn, state, today).await),
        ("jar overdue", jar_overdue(&mut conn, state, today).await),
        ("jar fleet", jar_fleet(&mut conn, state, today).await),
    ];
    for (name, r) in sections {
        match r {
            Ok(s) => { total.raised += s.raised; total.resolved += s.resolved; }
            Err(e) => tracing::warn!(section = name, error = ?e, "alert section failed"),
        }
    }
    Ok(total)
}

struct Alert { key: String, severity: &'static str, title: String, body: String, url: &'static str, payload: serde_json::Value }

/// Raise every alert in `current` (refreshing text/severity of already open ones) and resolve the open alerts of `event_type` that are not current.
async fn sync(conn: &mut PgConnection, event_type: &'static str, current: Vec<Alert>) -> anyhow::Result<RunStats> {
    let mut stats = RunStats::default();
    let mut keep: HashSet<String> = HashSet::new();
    for a in &current {
        keep.insert(a.key.clone());
        let created = alerts::raise(&mut *conn, NewAlert { event_type, dedupe_key: &a.key, severity: a.severity, title: &a.title, body: &a.body, url: a.url, payload: a.payload.clone() }).await?;
        if created {
            stats.raised += 1;
        } else {
            // same open alert: keep its text / severity current (DUE_SOON -> OVERDUE, days counting up) without re-notifying
            sqlx::query("UPDATE alerts SET severity = $3, title = $4, body = $5, payload = $6
                         WHERE event_type = $1 AND dedupe_key = $2 AND resolved_at IS NULL
                           AND (severity, title, body) IS DISTINCT FROM ($3, $4, $5)")
                .bind(event_type).bind(&a.key).bind(a.severity).bind(&a.title).bind(&a.body).bind(&a.payload).execute(&mut *conn).await?;
        }
    }
    let open: Vec<String> = sqlx::query_scalar("SELECT dedupe_key FROM alerts WHERE event_type = $1 AND resolved_at IS NULL").bind(event_type).fetch_all(&mut *conn).await?;
    for k in open.into_iter().filter(|k| !keep.contains(k)) {
        alerts::resolve(&mut *conn, event_type, &k).await?;
        stats.resolved += 1;
    }
    Ok(stats)
}

fn sev(overdue: bool) -> &'static str { if overdue { "CRITICAL" } else { "WARNING" } }

fn when(days_left: i64) -> String {
    match days_left {
        d if d < 0 => format!("{} day(s) overdue", -d),
        0 => "due today".to_string(),
        d => format!("due in {d} day(s)"),
    }
}

async fn maintenance_due(conn: &mut PgConnection, state: &AppState, today: chrono::NaiveDate) -> anyhow::Result<RunStats> {
    let lead = state.settings.get().expiry_lead_days;
    let rows = mt::load_schedules(conn, today, lead, None).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let current = rows.into_iter().filter(|s| s.status != "OK").map(|s| {
        let overdue = s.status == "OVERDUE";
        Alert {
            key: format!("schedule:{}:{}", s.id, s.next_due_on), severity: sev(overdue),
            title: format!("{}: {} {}", if overdue { "Maintenance overdue" } else { "Maintenance due" }, s.task, s.subject_name),
            body: format!("{} ({}) - {}, due {}.", s.task, s.subject_name, when(s.days_left), s.next_due_on),
            url: "/maintenance.html", payload: json!({ "schedule_id": s.id, "next_due_on": s.next_due_on, "days_left": s.days_left }),
        }
    }).collect();
    sync(conn, "MAINTENANCE_DUE", current).await
}

async fn vehicle_docs(conn: &mut PgConnection, state: &AppState, today: chrono::NaiveDate) -> anyhow::Result<RunStats> {
    let lead = state.settings.get().expiry_lead_days;
    let rows = mt::load_vehicle_docs(conn, today, lead, None).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let current = rows.into_iter().filter(|d| d.status != "OK").map(|d| {
        let overdue = d.status == "OVERDUE";
        Alert {
            key: format!("vehicle:{}:{}:{}", d.vehicle_id, d.doc, d.expiry_date), severity: sev(overdue),
            title: format!("Vehicle {}: {} {}", d.reg_no, d.doc, if overdue { "expired" } else { "expiring" }),
            body: format!("{} of {} - {} (expiry {}).", d.doc, d.reg_no, when(d.days_left), d.expiry_date),
            url: "/maintenance.html", payload: json!({ "vehicle_id": d.vehicle_id, "doc": d.doc, "expiry_date": d.expiry_date, "days_left": d.days_left }),
        }
    }).collect();
    sync(conn, "VEHICLE_DOC_EXPIRY", current).await
}

async fn jar_overdue(conn: &mut PgConnection, state: &AppState, today: chrono::NaiveDate) -> anyhow::Result<RunStats> {
    let limit = state.settings.get().jar_overdue_days;
    let rows = jars::balances(conn, None, None, None, false, today, limit).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let current = rows.into_iter().filter(|b| b.overdue).map(|b| Alert {
        key: format!("{}:{}", b.customer_id, b.product_id), severity: if b.days_outstanding > 2 * limit { "CRITICAL" } else { "WARNING" },
        title: format!("Overdue containers: {}", b.customer_name),
        body: format!("{} x {} outstanding, oldest for {} days (limit {} days).", b.holding, b.product_name, b.days_outstanding, limit),
        url: "/jars.html", payload: json!({ "customer_id": b.customer_id, "product_id": b.product_id, "holding": b.holding, "days_outstanding": b.days_outstanding }),
    }).collect();
    sync(conn, "JAR_OVERDUE", current).await
}

async fn jar_fleet(conn: &mut PgConnection, state: &AppState, today: chrono::NaiveDate) -> anyhow::Result<RunStats> {
    let st = state.settings.get();
    let mut current = vec![];
    for p in jars::active_returnable_products(conn).await.map_err(|e| anyhow::anyhow!("{e}"))? {
        let name = p.name.clone();
        let pid = p.id;
        let rep = jars::fleet_report(conn, p, today - chrono::Duration::days(29), today, st.utc_offset_minutes, st.jar_destroy_warn_pct, None).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        if rep.high_destroy_rate() {
            let rate = rep.destroy_rate_pct().map(|r| format!("{:.1}%", r)).unwrap_or_else(|| "no fills".into());
            current.push(Alert {
                key: format!("{pid}:HIGH_DESTROY_RATE"), severity: "WARNING",
                title: format!("High container loss: {name}"),
                body: format!("{} destroyed vs {} filled in the last 30 days ({rate}; limit {}%).", rep.period.destroyed_total, rep.period.fills, st.jar_destroy_warn_pct),
                url: "/jars.html", payload: json!({ "product_id": pid, "kind": "HIGH_DESTROY_RATE", "destroyed": rep.period.destroyed_total, "fills": rep.period.fills }),
            });
        }
        if rep.near_retirement() {
            let pct = rep.retirement_pct().map(|r| format!("{:.0}%", r)).unwrap_or_default();
            current.push(Alert {
                key: format!("{pid}:NEAR_RETIREMENT"), severity: if rep.retirement_pct().map_or(false, |r| r >= rust_decimal::Decimal::from(100)) { "CRITICAL" } else { "WARNING" },
                title: format!("Container fleet near retirement age: {name}"),
                body: format!("Estimated {pct} of the {} fills each container is rated for.", rep.product.max_cycles.unwrap_or(0)),
                url: "/jars.html", payload: json!({ "product_id": pid, "kind": "NEAR_RETIREMENT" }),
            });
        }
    }
    sync(conn, "JAR_FLEET_ALERT", current).await
}
