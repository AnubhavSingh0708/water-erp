//! Alert outbox. Producers (workers, handlers) call `raise`; the Web Push dispatcher (push.rs) delivers
//! each undispatched alert only to users whose `subscribed_notifications` contains its `event_type`.
use serde_json::Value;
use sqlx::PgExecutor;

/// (event_type, human label). Users subscribe to these codes. Keep in sync with API_REFERENCE.md.
pub const EVENT_TYPES: &[(&str, &str)] = &[
    ("LOW_STOCK", "Raw material below minimum stock"),
    ("REORDER_RECOMMENDED", "Reorder recommended (burn-rate vs lead time)"),
    ("QC_PENDING", "New batch awaiting Lab QC"),
    ("QC_APPROVED", "Batch approved by QC"),
    ("QC_REJECTED", "Batch rejected by QC"),
    ("WATER_OUT_OF_RANGE", "Water treatment reading out of range"),
    ("GATE_PASS_ISSUED", "Gate pass issued"),
    ("TRIP_SETTLED", "Trip return settled"),
    ("JAR_OVERDUE", "20L jars overdue at customer"),
    ("MAINTENANCE_DUE", "Equipment maintenance due/overdue"),
    ("VEHICLE_DOC_EXPIRY", "Vehicle insurance/fitness/PUC/permit expiring"),
    ("JAR_FLEET_ALERT", "Returnable containers: high destroy rate or fleet near retirement age"),
    ("STOCK_EXPIRY", "Raw material lot or finished-goods batch nearing expiry"),
];

/// Pages whose users may receive/see an event type (Admins: all). Prevents e.g. a user without the `jars` page from learning
/// customer names via a JAR_OVERDUE alert. Used by: subscription validation, inbox, dashboard recent alerts, push dispatcher.
pub fn event_pages(code: &str) -> &'static [&'static str] {
    match code {
        "LOW_STOCK" | "REORDER_RECOMMENDED" | "STOCK_EXPIRY" => &["raw-materials", "warehouse"],
        "QC_PENDING" | "QC_APPROVED" | "QC_REJECTED" => &["qc", "production"],
        "WATER_OUT_OF_RANGE" => &["production", "qc"],
        "GATE_PASS_ISSUED" | "TRIP_SETTLED" => &["loading"],
        "JAR_OVERDUE" | "JAR_FLEET_ALERT" => &["jars"],
        "MAINTENANCE_DUE" => &["maintenance"],
        "VEHICLE_DOC_EXPIRY" => &["maintenance", "loading"],
        _ => &[],
    }
}

/// May a user with these permissions receive/see this event type?
pub fn user_may_receive(is_admin: bool, allowed_pages: &[String], code: &str) -> bool {
    is_admin || event_pages(code).iter().any(|p| allowed_pages.iter().any(|a| a == p))
}

pub fn is_valid_event(code: &str) -> bool { EVENT_TYPES.iter().any(|(c, _)| *c == code) }

pub struct NewAlert<'a> {
    pub event_type: &'a str,
    /// Stable key of the thing being alerted on (e.g. material id). An unresolved alert with the same
    /// (event_type, dedupe_key) is never raised twice.
    pub dedupe_key: &'a str,
    pub severity: &'a str, // INFO | WARNING | CRITICAL
    pub title: &'a str,
    pub body: &'a str,
    pub url: &'a str,
    pub payload: Value,
}

/// Returns true when a new alert was created, false when an identical unresolved one already exists.
pub async fn raise<'e>(db: impl PgExecutor<'e>, a: NewAlert<'_>) -> Result<bool, sqlx::Error> {
    debug_assert!(is_valid_event(a.event_type));
    let r = sqlx::query(
        "INSERT INTO alerts (event_type, dedupe_key, severity, title, body, url, payload)
         VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (event_type, dedupe_key) WHERE resolved_at IS NULL DO NOTHING")
        .bind(a.event_type).bind(a.dedupe_key).bind(a.severity).bind(a.title).bind(a.body).bind(a.url).bind(a.payload)
        .execute(db).await?;
    Ok(r.rows_affected() > 0)
}

/// Mark the open alert resolved (condition cleared) so it can be raised again later.
pub async fn resolve<'e>(db: impl PgExecutor<'e>, event_type: &str, dedupe_key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE alerts SET resolved_at = now() WHERE event_type=$1 AND dedupe_key=$2 AND resolved_at IS NULL")
        .bind(event_type).bind(dedupe_key).execute(db).await?;
    Ok(())
}

/// One-shot event (QC approved, gate pass issued ...): raised and immediately resolved so it never blocks a later identical event.
pub async fn notify_once<'e>(db: impl PgExecutor<'e>, a: NewAlert<'_>) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO alerts (event_type, dedupe_key, severity, title, body, url, payload, resolved_at) VALUES ($1,$2,$3,$4,$5,$6,$7, now())")
        .bind(a.event_type).bind(a.dedupe_key).bind(a.severity).bind(a.title).bind(a.body).bind(a.url).bind(a.payload)
        .execute(db).await?;
    Ok(())
}
