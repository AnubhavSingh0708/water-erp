mod common;
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::{collections::HashSet, sync::Mutex};
use uuid::Uuid;
use water_erp::{
    alerts::{self, NewAlert},
    push::{PushOutcome, PushSender, PushTarget},
    workers::push::{dispatch_once, dispatch_with},
};

/// Records every send; endpoints listed in `gone` answer 410, in `fail` answer a transient error.
#[derive(Default)]
struct Mock {
    calls: Mutex<Vec<(Uuid, String, Vec<u8>)>>,
    gone: HashSet<String>,
    fail: HashSet<String>,
}
#[async_trait]
impl PushSender for Mock {
    async fn send(&self, t: &PushTarget, payload: &[u8]) -> PushOutcome {
        self.calls.lock().unwrap().push((t.user_id, t.endpoint.clone(), payload.to_vec()));
        if self.gone.contains(&t.endpoint) { PushOutcome::Gone } else if self.fail.contains(&t.endpoint) { PushOutcome::Failed("HTTP 500".into()) } else { PushOutcome::Delivered }
    }
}
impl Mock {
    fn users(&self) -> Vec<Uuid> { self.calls.lock().unwrap().iter().map(|c| c.0).collect() }
    fn count(&self) -> usize { self.calls.lock().unwrap().len() }
}

async fn sub(pool: &PgPool, user: Uuid, name: &str, active: bool) -> String {
    let endpoint = format!("https://push.example.com/{name}");
    sqlx::query("INSERT INTO push_subscriptions (user_id, endpoint, p256dh, auth, active) VALUES ($1,$2,'k','a',$3)")
        .bind(user).bind(&endpoint).bind(active).execute(pool).await.unwrap();
    endpoint
}

async fn alert(pool: &PgPool, ev: &str, key: &str) -> Uuid {
    alerts::raise(pool, NewAlert { event_type: ev, dedupe_key: key, severity: "WARNING", title: "Title", body: "Body", url: "/jars.html", payload: json!({}) }).await.unwrap();
    sqlx::query_scalar("SELECT id FROM alerts WHERE event_type=$1 AND dedupe_key=$2 ORDER BY created_at DESC LIMIT 1").bind(ev).bind(key).fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn delivers_only_to_subscribed_active_users_with_active_subscriptions(pool: PgPool) {
    let st = common::state(pool.clone()).await;
    let a = common::make_user(&pool, "alice", false, &[], &["QC_PENDING"]).await;
    let b = common::make_user(&pool, "bob", false, &[], &["MAINTENANCE_DUE"]).await;           // other type
    let c = common::make_user(&pool, "carol", false, &[], &["QC_PENDING"]).await;              // deactivated user
    let d = common::make_user(&pool, "dave", false, &[], &["QC_PENDING"]).await;               // no subscription at all
    let e = common::make_user(&pool, "erin", false, &[], &["QC_PENDING"]).await;               // only an inactive subscription
    let f = common::make_user(&pool, "frank", false, &[], &[]).await;                          // nothing subscribed
    sqlx::query("UPDATE users SET is_active=false WHERE id=$1").bind(c).execute(&pool).await.unwrap();
    let ea = sub(&pool, a, "a", true).await;
    let _ = sub(&pool, a, "a2", true).await;
    for (u, n) in [(b, "b"), (c, "c"), (e, "e"), (f, "f")] { sub(&pool, u, n, n != "e").await; }
    let _ = d;
    let id = alert(&pool, "QC_PENDING", "batch-1").await;

    let mock = Mock::default();
    let s = dispatch_with(&st, Some(&mock)).await.unwrap();
    assert_eq!(s.alerts, 1);
    assert_eq!(s.recipients, 1);
    assert_eq!(s.sent, 2, "both of alice's devices");
    let users = mock.users();
    assert_eq!(users.len(), 2);
    assert!(users.iter().all(|u| *u == a), "only alice may receive: {users:?}");
    assert!(mock.calls.lock().unwrap().iter().any(|c| c.1 == ea));
    // payload shape
    let p: Value = serde_json::from_slice(&mock.calls.lock().unwrap()[0].2).unwrap();
    assert_eq!(p["title"], "Title");
    assert_eq!(p["url"], "/jars.html");
    assert_eq!(p["alert_id"], id.to_string());
    assert!(p["tag"].as_str().unwrap().starts_with("QC_PENDING:"));

    let disp: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("SELECT dispatched_at FROM alerts WHERE id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert!(disp.is_some());
    let del: Vec<Uuid> = sqlx::query_scalar("SELECT user_id FROM push_deliveries WHERE alert_id=$1").bind(id).fetch_all(&pool).await.unwrap();
    assert_eq!(del, vec![a]);

    // second pass: nothing left, no duplicates
    let s2 = dispatch_with(&st, Some(&mock)).await.unwrap();
    assert_eq!(s2.alerts, 0);
    assert_eq!(mock.count(), 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn existing_delivery_row_prevents_duplicate_and_notify_once_is_dispatched(pool: PgPool) {
    let st = common::state(pool.clone()).await;
    let a = common::make_user(&pool, "alice", false, &[], &["QC_APPROVED", "QC_PENDING"]).await;
    sub(&pool, a, "a", true).await;
    let id = alert(&pool, "QC_PENDING", "k").await;
    sqlx::query("INSERT INTO push_deliveries (alert_id,user_id) VALUES ($1,$2)").bind(id).bind(a).execute(&pool).await.unwrap();
    // one-shot alert is born resolved but must still be pushed
    alerts::notify_once(&pool, NewAlert { event_type: "QC_APPROVED", dedupe_key: "b1", severity: "INFO", title: "ok", body: "", url: "/qc.html", payload: json!({}) }).await.unwrap();
    let mock = Mock::default();
    let s = dispatch_with(&st, Some(&mock)).await.unwrap();
    assert_eq!(s.alerts, 2);
    assert_eq!(mock.count(), 1, "only the QC_APPROVED one-shot; the QC_PENDING delivery already existed");
    let ev: Vec<String> = sqlx::query_scalar("SELECT a.event_type FROM push_deliveries d JOIN alerts a ON a.id=d.alert_id ORDER BY 1").fetch_all(&pool).await.unwrap();
    assert_eq!(ev, vec!["QC_APPROVED", "QC_PENDING"]);
}

#[sqlx::test(migrations = "./migrations")]
async fn gone_endpoint_is_deactivated_failures_are_recorded(pool: PgPool) {
    let st = common::state(pool.clone()).await;
    let a = common::make_user(&pool, "alice", false, &[], &["LOW_STOCK"]).await;
    let ok = sub(&pool, a, "ok", true).await;
    let stale = sub(&pool, a, "stale", true).await;
    let flaky = sub(&pool, a, "flaky", true).await;
    alert(&pool, "LOW_STOCK", "m1").await;
    let mut mock = Mock::default();
    mock.gone.insert(stale.clone());
    mock.fail.insert(flaky.clone());
    let s = dispatch_with(&st, Some(&mock)).await.unwrap();
    assert_eq!((s.sent, s.gone, s.failed), (1, 1, 1));
    let rows: Vec<(String, bool, Option<String>, bool)> = sqlx::query_as("SELECT endpoint, active, last_error, last_success_at IS NOT NULL FROM push_subscriptions ORDER BY endpoint").fetch_all(&pool).await.unwrap();
    for (ep, active, err, success) in rows {
        if ep == ok { assert!(active && success && err.is_none()); }
        else if ep == stale { assert!(!active); assert!(err.is_some()); }
        else if ep == flaky { assert!(active, "transient failures keep the subscription"); assert_eq!(err.as_deref(), Some("HTTP 500")); }
    }
    // a deactivated subscription is not used for the next alert
    alert(&pool, "LOW_STOCK", "m2").await;
    let before = mock.count();
    dispatch_with(&st, Some(&mock)).await.unwrap();
    let new_calls: Vec<String> = mock.calls.lock().unwrap()[before..].iter().map(|c| c.1.clone()).collect();
    assert!(!new_calls.contains(&stale));
}

#[sqlx::test(migrations = "./migrations")]
async fn without_vapid_keys_alerts_are_only_marked_dispatched(pool: PgPool) {
    let st = common::state(pool.clone()).await; // test config has no VAPID keys
    let a = common::make_user(&pool, "alice", false, &[], &["LOW_STOCK"]).await;
    sub(&pool, a, "a", true).await;
    let id = alert(&pool, "LOW_STOCK", "m1").await;
    let s = dispatch_once(&st).await.unwrap();
    assert_eq!((s.alerts, s.recipients, s.sent), (1, 0, 0));
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_deliveries").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
    let d: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("SELECT dispatched_at FROM alerts WHERE id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert!(d.is_some());
}

#[sqlx::test(migrations = "./migrations")]
async fn stale_alerts_are_not_pushed(pool: PgPool) {
    let st = common::state(pool.clone()).await;
    let a = common::make_user(&pool, "alice", false, &[], &["LOW_STOCK"]).await;
    sub(&pool, a, "a", true).await;
    let id = alert(&pool, "LOW_STOCK", "old").await;
    sqlx::query("UPDATE alerts SET created_at = now() - interval '2 days' WHERE id=$1").bind(id).execute(&pool).await.unwrap();
    let mock = Mock::default();
    let s = dispatch_with(&st, Some(&mock)).await.unwrap();
    assert_eq!(s.alerts, 1);
    assert_eq!(mock.count(), 0);
    let d: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("SELECT dispatched_at FROM alerts WHERE id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert!(d.is_some());
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_dispatchers_never_double_send(pool: PgPool) {
    let st = common::state(pool.clone()).await;
    let a = common::make_user(&pool, "alice", false, &[], &["LOW_STOCK"]).await;
    sub(&pool, a, "a", true).await;
    for i in 0..10 { alert(&pool, "LOW_STOCK", &format!("m{i}")).await; }
    let mock = Mock::default();
    let (r1, r2, r3) = tokio::join!(dispatch_with(&st, Some(&mock)), dispatch_with(&st, Some(&mock)), dispatch_with(&st, Some(&mock)));
    r1.unwrap(); r2.unwrap(); r3.unwrap();
    assert_eq!(mock.count(), 10, "each alert reaches alice's single device exactly once");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_deliveries").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 10);
}
