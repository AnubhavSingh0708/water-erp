mod common;
use axum::{body::Body, http::{Request, StatusCode}, Router};
use http_body_util::BodyExt;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use std::time::Duration;
use tower::ServiceExt;
use uuid::Uuid;
use water_erp::{events, routes, stock::{self, ItemType, Move}};

struct Sse { body: Body, buf: String }
impl Sse {
    async fn open(app: &Router, key: &str, query: &str) -> (StatusCode, Option<Sse>) {
        let req = Request::builder().uri(format!("/api/events{query}")).header("x-auth-key", key).body(Body::empty()).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let st = resp.status();
        if st != StatusCode::OK { return (st, None); }
        (st, Some(Sse { body: resp.into_body(), buf: String::new() }))
    }
    /// Next non-comment SSE frame as (event, data); None on timeout or end of stream.
    async fn next(&mut self, secs: u64) -> Option<(String, String)> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        loop {
            while let Some(i) = self.buf.find("\n\n") {
                let frame: String = self.buf.drain(..i + 2).collect();
                let (mut ev, mut data) = (String::new(), String::new());
                for l in frame.lines() {
                    if let Some(v) = l.strip_prefix("event:") { ev = v.trim().into(); }
                    if let Some(v) = l.strip_prefix("data:") { data = v.trim().into(); }
                }
                if !ev.is_empty() { return Some((ev, data)); }
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, self.body.frame()).await {
                Ok(Some(Ok(f))) => { if let Ok(d) = f.into_data() { self.buf.push_str(&String::from_utf8_lossy(&d)); } }
                _ => return None,
            }
        }
    }
    async fn next_change_topics(&mut self, secs: u64) -> Option<Vec<String>> {
        loop {
            let (e, d) = self.next(secs).await?;
            if e == "change" {
                let v: serde_json::Value = serde_json::from_str(&d).unwrap();
                return Some(v["topics"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect());
            }
        }
    }
    /// Swallow events caused by test fixtures (e.g. the `users` insert) so assertions see only what the test triggers.
    async fn settle(&mut self) {
        tokio::time::sleep(Duration::from_millis(500)).await;
        while self.next_change_topics(1).await.is_some() {}
    }
    async fn drain_topics(&mut self) -> Vec<String> {
        let mut seen = vec![];
        while let Some(t) = self.next_change_topics(1).await { seen.extend(t); }
        seen.sort(); seen.dedup(); seen
    }
}

async fn setup(pool: PgPool, check_secs: u64) -> Router {
    let mut st = common::state(pool).await;
    st.events.session_check_secs = check_secs;
    events::spawn_listener(st.clone());
    tokio::time::sleep(Duration::from_millis(500)).await; // let LISTEN attach
    routes::build_router(st)
}
async fn notify(pool: &PgPool, topics: &[&str]) {
    for t in topics { sqlx::query("SELECT pg_notify('erp_events',$1)").bind(*t).execute(pool).await.unwrap(); }
}

#[sqlx::test(migrations = "./migrations")]
async fn requires_auth(pool: PgPool) {
    let app = setup(pool, 30).await;
    assert_eq!(Sse::open(&app, "nope", "").await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn committed_stock_change_emits_topic_and_rollback_does_not(pool: PgPool) {
    let app = setup(pool.clone(), 30).await;
    let admin = common::make_user(&pool, "root", true, &[], &[]).await;
    let key = common::session_for(&pool, admin).await;
    let mut s = Sse::open(&app, &key, "").await.1.unwrap();
    assert_eq!(s.next(3).await.unwrap().0, "hello"); s.settle().await;
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Raw, Uuid::new_v4(), "RM_STORE", Uuid::nil(), dec!(5), "GRN")]).await.unwrap();
    tx.rollback().await.unwrap();
    notify(&pool, &["maintenance"]).await; // sentinel
    assert_eq!(s.next_change_topics(3).await.unwrap(), vec!["maintenance"]);
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Raw, Uuid::new_v4(), "RM_STORE", Uuid::nil(), dec!(5), "GRN")]).await.unwrap();
    tx.commit().await.unwrap();
    assert!(s.next_change_topics(3).await.unwrap().contains(&"stock".to_string()));
}

#[sqlx::test(migrations = "./migrations")]
async fn topics_are_filtered_by_allowed_pages(pool: PgPool) {
    let app = setup(pool.clone(), 30).await;
    let u = common::make_user(&pool, "qcuser", false, &["qc"], &[]).await;
    let key = common::session_for(&pool, u).await;
    let mut s = Sse::open(&app, &key, "").await.1.unwrap();
    assert_eq!(s.next(3).await.unwrap().0, "hello"); s.settle().await;
    notify(&pool, &["qr", "stock", "admin", "jars", "qc", "alerts"]).await;
    assert_eq!(s.drain_topics().await, vec!["alerts", "qc"], "qc-only user must never see stock/qr/admin/jars");
}

#[sqlx::test(migrations = "./migrations")]
async fn page_subscription_only_receives_that_pages_topics(pool: PgPool) {
    let app = setup(pool.clone(), 30).await;
    let admin = common::make_user(&pool, "root2", true, &[], &[]).await;
    let key = common::session_for(&pool, admin).await;
    // loading page: qr, trips, qc, stock, master, alerts, users  -- NOT jars/maintenance/materials/admin
    let mut s = Sse::open(&app, &key, "?page=loading").await.1.unwrap();
    assert_eq!(s.next(3).await.unwrap().0, "hello"); s.settle().await;
    notify(&pool, &["jars", "maintenance", "materials", "admin", "production", "qr", "trips"]).await;
    assert_eq!(s.drain_topics().await, vec!["qr", "trips"]);
    // jars page must not be refreshed by loading events
    let mut j = Sse::open(&app, &key, "?page=jars").await.1.unwrap();
    assert_eq!(j.next(3).await.unwrap().0, "hello"); j.settle().await;
    notify(&pool, &["qr", "trips", "qc", "jars"]).await;
    assert_eq!(j.drain_topics().await, vec!["jars"]);
    // explicit narrowing
    let mut n = Sse::open(&app, &key, "?page=dashboard&topics=trips,jars").await.1.unwrap();
    assert_eq!(n.next(3).await.unwrap().0, "hello"); n.settle().await;
    notify(&pool, &["stock", "trips", "jars", "qc"]).await;
    assert_eq!(n.drain_topics().await, vec!["jars", "trips"]);
}

#[sqlx::test(migrations = "./migrations")]
async fn page_param_is_validated_and_authorised(pool: PgPool) {
    let app = setup(pool.clone(), 30).await;
    let u = common::make_user(&pool, "floor", false, &["loading"], &[]).await;
    let key = common::session_for(&pool, u).await;
    assert_eq!(Sse::open(&app, &key, "?page=admin").await.0, StatusCode::FORBIDDEN);
    assert_eq!(Sse::open(&app, &key, "?page=jars").await.0, StatusCode::FORBIDDEN);
    assert_eq!(Sse::open(&app, &key, "?page=nonsense").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(Sse::open(&app, &key, "?page=loading&topics=drop_table").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(Sse::open(&app, &key, "?page=loading").await.0, StatusCode::OK);
    assert_eq!(Sse::open(&app, &key, "?page=profile").await.0, StatusCode::OK);
}

#[sqlx::test(migrations = "./migrations")]
async fn revoked_session_ends_stream(pool: PgPool) {
    let app = setup(pool.clone(), 1).await;
    let u = common::make_user(&pool, "u1", true, &[], &[]).await;
    let key = common::session_for(&pool, u).await;
    let mut s = Sse::open(&app, &key, "").await.1.unwrap();
    assert_eq!(s.next(3).await.unwrap().0, "hello"); s.settle().await;
    sqlx::query("DELETE FROM user_sessions").execute(&pool).await.unwrap();
    loop { let (e, _) = s.next(4).await.expect("stream should end with session_ended"); if e == "session_ended" { break; } }
    assert!(s.next(2).await.is_none());
}

#[sqlx::test(migrations = "./migrations")]
async fn per_user_stream_cap(pool: PgPool) {
    let app = setup(pool.clone(), 30).await;
    let u = common::make_user(&pool, "u2", true, &[], &[]).await;
    let key = common::session_for(&pool, u).await;
    let mut held = vec![];
    for _ in 0..8 { let (c, s) = Sse::open(&app, &key, "").await; assert_eq!(c, StatusCode::OK); held.push(s); }
    assert_eq!(Sse::open(&app, &key, "").await.0, StatusCode::TOO_MANY_REQUESTS);
    drop(held);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(Sse::open(&app, &key, "").await.0, StatusCode::OK);
}
