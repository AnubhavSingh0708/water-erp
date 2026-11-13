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
    ("STOCK_EXPIRY", "Raw material lot or finished-goods batch nearing expiry"),
];

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
