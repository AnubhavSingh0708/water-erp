//! Alert outbox -> Web Push dispatcher. OWNER: Subagent C2.
//!
//! Every 5 s: claim undispatched alerts (`FOR UPDATE SKIP LOCKED`, so two dispatchers can never take the same alert), choose
//! the recipients = ACTIVE users whose `subscribed_notifications` contains the alert's event type AND who own at least one
//! active push subscription, record one `push_deliveries` row per (alert, user) (primary key => never twice), stamp
//! `alerts.dispatched_at` – all in ONE transaction – and only after commit send the encrypted pushes (at-most-once delivery;
//! the in-app inbox is the durable copy). 404/410 deactivate the subscription. Without VAPID keys alerts are just marked
//! dispatched (logged once). Alerts older than 6 h are never pushed (a restart after a long outage must not spam phones).
use crate::{
    push::{self, PushOutcome, PushSender, PushTarget},
    state::AppState,
};
use sqlx::Row;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use uuid::Uuid;

const INTERVAL: Duration = Duration::from_secs(5);
const BATCH: i64 = 25;
/// Alerts older than this are marked dispatched without a push.
const STALE_HOURS: i64 = 6;

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = dispatch_once(&state).await { tracing::warn!(error = ?e, "push dispatcher run failed"); }
            tokio::select! {
                _ = tokio::time::sleep(INTERVAL) => {}
                _ = state.shutdown.wait() => break,
            }
        }
    });
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DispatchStats {
    pub alerts: usize,
    /// users that were selected as recipients (deliveries claimed)
    pub recipients: usize,
    pub sent: usize,
    pub gone: usize,
    pub failed: usize,
}

static NO_KEYS_LOGGED: AtomicBool = AtomicBool::new(false);

/// One dispatcher pass with the real sender (or "mark only" when no VAPID keys are configured).
pub async fn dispatch_once(state: &AppState) -> anyhow::Result<DispatchStats> {
    match push::WebPushSender::from_state(state) {
        Some(sender) => dispatch_with(state, Some(&sender)).await,
        None => {
            if !NO_KEYS_LOGGED.swap(true, Ordering::Relaxed) {
                tracing::info!("Web Push disabled (no VAPID keys): alerts are only marked as dispatched. Run `water-erp gen-vapid` to enable.");
            }
            dispatch_with(state, None).await
        }
    }
}

struct Claimed { alert_id: Uuid, payload: Vec<u8>, targets: Vec<PushTarget> }

/// One pass with an explicit sender (`None` = push disabled). Tests inject a mock here.
pub async fn dispatch_with(state: &AppState, sender: Option<&dyn PushSender>) -> anyhow::Result<DispatchStats> {
    let mut stats = DispatchStats::default();
    let mut tx = state.db.begin().await?;
    let rows = sqlx::query(
        "SELECT id, event_type, dedupe_key, title, body, url, (created_at < now() - make_interval(hours => $2)) AS stale
         FROM alerts WHERE dispatched_at IS NULL ORDER BY created_at, id LIMIT $1 FOR UPDATE SKIP LOCKED")
        .bind(BATCH).bind(STALE_HOURS as i32).fetch_all(&mut *tx).await?;
    let mut claimed: Vec<Claimed> = vec![];
    for r in &rows {
        let id: Uuid = r.get("id");
        stats.alerts += 1;
        let stale: bool = r.get("stale");
        if sender.is_some() && !stale {
            let event_type: String = r.get("event_type");
            let users: Vec<Uuid> = sqlx::query_scalar(
                "INSERT INTO push_deliveries (alert_id, user_id)
                 SELECT $1, u.id FROM users u
                 WHERE u.is_active AND u.subscribed_notifications @> jsonb_build_array($2::text)
                   AND EXISTS (SELECT 1 FROM push_subscriptions s WHERE s.user_id = u.id AND s.active)
                 ON CONFLICT (alert_id, user_id) DO NOTHING RETURNING user_id")
                .bind(id).bind(&event_type).fetch_all(&mut *tx).await?;
            stats.recipients += users.len();
            if !users.is_empty() {
                let subs: Vec<(Uuid, Uuid, String, String, String)> = sqlx::query_as(
                    "SELECT id, user_id, endpoint, p256dh, auth FROM push_subscriptions WHERE active AND user_id = ANY($1) ORDER BY created_at")
                    .bind(&users).fetch_all(&mut *tx).await?;
                let dedupe_key: String = r.get("dedupe_key");
                let payload = push::build_payload(&r.get::<String, _>("title"), &r.get::<String, _>("body"), &r.get::<String, _>("url"),
                                                  &format!("{event_type}:{dedupe_key}"), Some(id));
                let targets = subs.into_iter().map(|(id, user_id, endpoint, p256dh, auth)| PushTarget { id, user_id, endpoint, p256dh, auth }).collect();
                claimed.push(Claimed { alert_id: id, payload, targets });
            }
        }
        sqlx::query("UPDATE alerts SET dispatched_at = now() WHERE id = $1 AND dispatched_at IS NULL").bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;

    // network I/O strictly after the commit: no DB locks are held while talking to push services
    if let Some(sender) = sender {
        for c in claimed {
            let res = push::deliver(state, sender, c.targets, &c.payload).await;
            for (_, o) in res {
                match o {
                    PushOutcome::Delivered => stats.sent += 1,
                    PushOutcome::Gone => stats.gone += 1,
                    PushOutcome::Failed(m) => { stats.failed += 1; tracing::debug!(alert = %c.alert_id, error = %m, "push failed"); }
                }
            }
        }
    }
    Ok(stats)
}
