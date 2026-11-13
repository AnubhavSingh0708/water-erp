mod common;
use axum::http::StatusCode;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use common::{app, call, make_user, session_for};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::alerts::{self, NewAlert};

fn keys() -> Value {
    let sk = p256::SecretKey::random(&mut rand::rngs::OsRng);
    let mut auth = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut auth);
    json!({ "p256dh": URL_SAFE_NO_PAD.encode(sk.public_key().to_encoded_point(false).as_bytes()), "auth": URL_SAFE_NO_PAD.encode(auth) })
}

async fn raise(pool: &PgPool, ev: &str, key: &str, title: &str) -> Uuid {
    alerts::raise(pool, NewAlert { event_type: ev, dedupe_key: key, severity: "INFO", title, body: "b", url: "/x.html", payload: json!({}) }).await.unwrap();
    sqlx::query_scalar("SELECT id FROM alerts WHERE event_type=$1 AND dedupe_key=$2").bind(ev).bind(key).fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn auth_required_but_no_page_needed(pool: PgPool) {
    let (app, _) = app(pool.clone()).await;
    let (s, _) = call(&app, "GET", "/api/notifications/types", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let u = make_user(&pool, "nopages", false, &[], &[]).await;
    let k = session_for(&pool, u).await;
    let (s, v) = call(&app, "GET", "/api/notifications/types", Some(&k), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v.as_array().unwrap().iter().any(|t| t["code"] == "JAR_FLEET_ALERT"));
}

#[sqlx::test(migrations = "./migrations")]
async fn subscriptions_are_validated_and_own_only(pool: PgPool) {
    let (app, _) = app(pool.clone()).await;
    let a = make_user(&pool, "a", false, &[], &[]).await;
    let b = make_user(&pool, "b", false, &[], &["LOW_STOCK"]).await;
    let (ka, kb) = (session_for(&pool, a).await, session_for(&pool, b).await);
    let (s, v) = call(&app, "PUT", "/api/notifications/subscriptions", Some(&ka), Some(json!({"subscribed":["QC_PENDING","QC_PENDING","LOW_STOCK"]}))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["subscribed"], json!(["LOW_STOCK", "QC_PENDING"]));
    let (s, v) = call(&app, "PUT", "/api/notifications/subscriptions", Some(&ka), Some(json!({"subscribed":["NOPE"]}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    let (s, _) = call(&app, "PUT", "/api/notifications/subscriptions", Some(&ka), Some(json!({"subscribed":[], "user_id": b}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown field (cannot target another user)");
    let (s, v) = call(&app, "GET", "/api/notifications/subscriptions", Some(&kb), None).await;
    assert_eq!((s, v["subscribed"].clone()), (StatusCode::OK, json!(["LOW_STOCK"])), "bob untouched");
}

#[sqlx::test(migrations = "./migrations")]
async fn inbox_only_shows_subscribed_types_and_read_probe_is_404(pool: PgPool) {
    let (app, _) = app(pool.clone()).await;
    let a = make_user(&pool, "a", false, &[], &["LOW_STOCK"]).await;
    let b = make_user(&pool, "b", false, &[], &["JAR_OVERDUE"]).await;
    let (ka, kb) = (session_for(&pool, a).await, session_for(&pool, b).await);
    let low = raise(&pool, "LOW_STOCK", "m", "Low cap").await;
    let jar = raise(&pool, "JAR_OVERDUE", "c:p", "Customer X owes jars").await;

    let (_, v) = call(&app, "GET", "/api/notifications/inbox", Some(&ka), None).await;
    assert_eq!(v["total"], 1);
    assert_eq!(v["items"][0]["title"], "Low cap");
    assert_eq!(v["items"][0]["read"], false);
    let (_, v) = call(&app, "GET", "/api/notifications/inbox", Some(&kb), None).await;
    assert_eq!(v["items"][0]["title"], "Customer X owes jars");

    // alice cannot mark / probe bob's alert
    let (s, _) = call(&app, "POST", &format!("/api/notifications/inbox/{jar}/read"), Some(&ka), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&app, "POST", &format!("/api/notifications/inbox/{}/read", Uuid::new_v4()), Some(&ka), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM alert_reads").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);

    let (_, v) = call(&app, "GET", "/api/notifications/inbox/unread-count", Some(&ka), None).await;
    assert_eq!(v["count"], 1);
    let (s, _) = call(&app, "POST", &format!("/api/notifications/inbox/{low}/read"), Some(&ka), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, v) = call(&app, "GET", "/api/notifications/inbox/unread-count", Some(&ka), None).await;
    assert_eq!(v["count"], 0);
    let (_, v) = call(&app, "GET", "/api/notifications/inbox?unread=1", Some(&ka), None).await;
    assert_eq!(v["total"], 0);
    let (_, v) = call(&app, "GET", "/api/notifications/inbox", Some(&ka), None).await;
    assert_eq!(v["items"][0]["read"], true);

    // after unsubscribing, the alert disappears from alice's inbox; read-all only touches visible alerts
    call(&app, "PUT", "/api/notifications/subscriptions", Some(&ka), Some(json!({"subscribed":["JAR_OVERDUE"]}))).await;
    let (_, v) = call(&app, "GET", "/api/notifications/inbox", Some(&ka), None).await;
    assert_eq!(v["items"][0]["title"], "Customer X owes jars");
    let (s, v) = call(&app, "POST", "/api/notifications/inbox/read-all", Some(&ka), None).await;
    assert_eq!((s, v["marked"].clone()), (StatusCode::OK, json!(1)));
    let (_, v) = call(&app, "GET", "/api/notifications/inbox/unread-count", Some(&kb), None).await;
    assert_eq!(v["count"], 1, "bob's unread state is independent");
}

#[sqlx::test(migrations = "./migrations")]
async fn sql_injection_strings_in_filters_are_inert(pool: PgPool) {
    let (app, _) = app(pool.clone()).await;
    let a = make_user(&pool, "a", false, &[], &["LOW_STOCK"]).await;
    let ka = session_for(&pool, a).await;
    raise(&pool, "LOW_STOCK", "m", "t").await;
    for q in ["unread=1'%20OR%20'1'='1", "unread=1;DROP%20TABLE%20alerts;--", "limit=1%3BSELECT%201", "offset=-5"] {
        let (s, _) = call(&app, "GET", &format!("/api/notifications/inbox?{q}"), Some(&ka), None).await;
        assert!(s == StatusCode::OK || s == StatusCode::BAD_REQUEST, "{q}: {s}");
    }
    let (_, v) = call(&app, "GET", "/api/notifications/inbox", Some(&ka), None).await;
    assert_eq!(v["total"], 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn push_registration_rules_and_ssrf(pool: PgPool) {
    let (app, _) = app(pool.clone()).await;
    let a = make_user(&pool, "a", false, &[], &[]).await;
    let b = make_user(&pool, "b", false, &[], &[]).await;
    let (ka, kb) = (session_for(&pool, a).await, session_for(&pool, b).await);
    let (_, v) = call(&app, "GET", "/api/notifications/push/key", Some(&ka), None).await;
    assert_eq!((v["enabled"].clone(), v["public_key"].clone()), (json!(false), Value::Null));

    for bad in ["http://push.example.com/x", "https://localhost/x", "https://127.0.0.1/x", "https://169.254.169.254/latest", "https://[::1]/x",
                "https://foo.internal/x", "https://user:pw@push.example.com/x", "https://2130706433/x", "https://push.example.com:8443/x"] {
        let (s, v) = call(&app, "POST", "/api/notifications/push/subscribe", Some(&ka), Some(json!({"endpoint": bad, "keys": keys()}))).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {v}");
    }
    let (s, _) = call(&app, "POST", "/api/notifications/push/subscribe", Some(&ka), Some(json!({"endpoint": "https://push.example.com/a", "keys": {"p256dh": "AAAA", "auth": "AAAA"}}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "garbage keys");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_subscriptions").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);

    let ep = "https://fcm.googleapis.com/fcm/send/abc";
    let (s, _) = call(&app, "POST", "/api/notifications/push/subscribe", Some(&ka), Some(json!({"endpoint": ep, "keys": keys(), "expirationTime": null}))).await;
    assert_eq!(s, StatusCode::CREATED);
    // another user cannot remove it ...
    let (s, _) = call(&app, "POST", "/api/notifications/push/unsubscribe", Some(&kb), Some(json!({"endpoint": ep}))).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_subscriptions WHERE user_id=$1").bind(a).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    // ... but the same device re-registered by bob moves to bob (shared browser), and alice can no longer remove it
    let (s, _) = call(&app, "POST", "/api/notifications/push/subscribe", Some(&kb), Some(json!({"endpoint": ep, "keys": keys()}))).await;
    assert_eq!(s, StatusCode::CREATED);
    let owner: Uuid = sqlx::query_scalar("SELECT user_id FROM push_subscriptions WHERE endpoint=$1").bind(ep).fetch_one(&pool).await.unwrap();
    assert_eq!(owner, b);
    call(&app, "POST", "/api/notifications/push/unsubscribe", Some(&ka), Some(json!({"endpoint": ep}))).await;
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_subscriptions").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    call(&app, "POST", "/api/notifications/push/unsubscribe", Some(&kb), Some(json!({"endpoint": ep}))).await;
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_subscriptions").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);

    // per-user cap
    for i in 0..13 {
        let (s, _) = call(&app, "POST", "/api/notifications/push/subscribe", Some(&ka), Some(json!({"endpoint": format!("https://fcm.googleapis.com/fcm/send/{i}"), "keys": keys()}))).await;
        assert_eq!(s, StatusCode::CREATED);
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_subscriptions WHERE user_id=$1").bind(a).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 10);

    // no VAPID keys -> test push is disabled
    let (s, v) = call(&app, "POST", "/api/notifications/push/test", Some(&ka), None).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("FEATURE_DISABLED")));
}

#[sqlx::test(migrations = "./migrations")]
async fn push_test_goes_only_to_own_subscriptions(pool: PgPool) {
    use async_trait::async_trait;
    use std::sync::Mutex;
    use water_erp::push::{PushOutcome, PushSender, PushTarget};
    struct M(Mutex<Vec<Uuid>>);
    #[async_trait]
    impl PushSender for M { async fn send(&self, t: &PushTarget, _p: &[u8]) -> PushOutcome { self.0.lock().unwrap().push(t.user_id); PushOutcome::Delivered } }
    let st = common::state(pool.clone()).await;
    let a = make_user(&pool, "a", false, &[], &[]).await;
    let b = make_user(&pool, "b", false, &[], &[]).await;
    for (u, n) in [(a, "1"), (b, "2")] {
        sqlx::query("INSERT INTO push_subscriptions (user_id,endpoint,p256dh,auth) VALUES ($1,$2,'k','a')").bind(u).bind(format!("https://push.example.com/{n}")).execute(&pool).await.unwrap();
    }
    let m = M(Mutex::new(vec![]));
    let r = water_erp::routes::notifications::send_test_with(&st, &m, a).await.unwrap();
    assert_eq!(r["sent"], 1);
    assert_eq!(*m.0.lock().unwrap(), vec![a]);
}

#[sqlx::test(migrations = "./migrations")]
async fn malformed_ids_use_the_error_envelope(pool: PgPool) {
    let (app, _) = app(pool.clone()).await;
    let a = make_user(&pool, "a", true, &[], &[]).await;
    let k = session_for(&pool, a).await;
    for (m, p) in [("POST", "/api/notifications/inbox/not-a-uuid/read"), ("GET", "/api/jars/customers/xyz/ledger"), ("GET", "/api/maintenance/schedules/1/logs")] {
        let (s, v) = call(&app, m, p, Some(&k), None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{p}");
        assert_eq!(v["error"]["code"], "BAD_REQUEST", "{p}: {v}");
    }
}
