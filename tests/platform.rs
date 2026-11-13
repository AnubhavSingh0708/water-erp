mod common;
use axum::{extract::State, http::StatusCode, middleware, routing::post, Json, Router};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};
use tower::ServiceExt;
use water_erp::{auth, routes};

#[derive(Clone)]
struct Counter(Arc<AtomicUsize>);
async fn bump(State(c): State<Counter>) -> Json<Value> { Json(json!({ "n": c.0.fetch_add(1, Ordering::SeqCst) + 1 })) }
async fn needs_confirm(body: Option<Json<Value>>) -> Result<Json<Value>, water_erp::error::AppError> {
    let ok = body.map_or(false, |b| b.0["confirm"] == json!(true));
    if ok { Ok(Json(json!({"done": true}))) } else { Err(water_erp::error::AppError::confirm("Large quantity", vec!["10x usual".into()])) }
}
async fn boom() -> Result<Json<Value>, water_erp::error::AppError> { Err(water_erp::error::AppError::Unavailable) }
async fn fail500() -> (StatusCode, Json<Value>) { (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))) }

fn probe_router(st: water_erp::state::AppState, c: Counter) -> Router {
    Router::new()
        .route("/api/probe", post(bump).with_state(c.clone()))
        .route("/api/probe2", post(bump).with_state(c))
        .route("/api/confirm", post(needs_confirm))
        .route("/api/boom", post(boom))
        .route("/api/fail500", post(fail500))
        .layer(middleware::from_fn_with_state(st.clone(), routes::idempotency))
        .layer(middleware::from_fn_with_state(st, auth::auth_middleware))
}
async fn post_with(app: &Router, uri: &str, key: &str, idem: Option<&str>) -> (StatusCode, Value, Option<String>) {
    post_body(app, uri, key, idem, None).await
}
async fn post_body(app: &Router, uri: &str, key: &str, idem: Option<&str>, body: Option<Value>) -> (StatusCode, Value, Option<String>) {
    let mut b = axum::http::Request::builder().method("POST").uri(uri).header("x-auth-key", key);
    if let Some(i) = idem { b = b.header("idempotency-key", i); }
    let req = match body { Some(j) => b.header("content-type", "application/json").body(axum::body::Body::from(j.to_string())).unwrap(), None => b.body(axum::body::Body::empty()).unwrap() };
    let resp = app.clone().oneshot(req).await.unwrap();
    let replay = resp.headers().get("idempotent-replay").map(|v| v.to_str().unwrap().to_string());
    let st = resp.status();
    let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null), replay)
}

#[sqlx::test(migrations = "./migrations")]
async fn idempotent_retry_runs_handler_once(pool: PgPool) {
    let st = common::state(pool.clone()).await;
    let c = Counter(Arc::new(AtomicUsize::new(0)));
    let app = probe_router(st, c.clone());
    let u = common::make_user(&pool, "u", true, &[], &[]).await;
    let key = common::session_for(&pool, u).await;
    let (s1, b1, r1) = post_with(&app, "/api/probe", &key, Some("abc12345-key")).await;
    let (s2, b2, r2) = post_with(&app, "/api/probe", &key, Some("abc12345-key")).await;
    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK));
    assert_eq!(b1, b2, "replayed body must equal the first response");
    assert!(r1.is_none() && r2.as_deref() == Some("true"));
    assert_eq!(c.0.load(Ordering::SeqCst), 1, "handler must not run twice");
    // different key => runs again; no key => runs again
    post_with(&app, "/api/probe", &key, Some("other-key-0001")).await;
    post_with(&app, "/api/probe", &key, None).await;
    assert_eq!(c.0.load(Ordering::SeqCst), 3);
    // same key on a different path => conflict, handler not run
    let (s, b, _) = post_with(&app, "/api/probe2", &key, Some("abc12345-key")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(b["error"]["code"], "IDEMPOTENCY_CONFLICT");
    assert_eq!(c.0.load(Ordering::SeqCst), 3);
    // keys are per user
    let u2 = common::make_user(&pool, "u2", true, &[], &[]).await;
    let key2 = common::session_for(&pool, u2).await;
    let (_, _, r) = post_with(&app, "/api/probe", &key2, Some("abc12345-key")).await;
    assert!(r.is_none());
    assert_eq!(c.0.load(Ordering::SeqCst), 4);
    // malformed key rejected
    let (s, _, _) = post_with(&app, "/api/probe", &key, Some("short")).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_duplicate_submits_run_once(pool: PgPool) {
    let st = common::state(pool.clone()).await;
    let c = Counter(Arc::new(AtomicUsize::new(0)));
    let app = probe_router(st, c.clone());
    let u = common::make_user(&pool, "u", true, &[], &[]).await;
    let key = common::session_for(&pool, u).await;
    let mut hs = vec![];
    for _ in 0..10 { let (a, k) = (app.clone(), key.clone()); hs.push(tokio::spawn(async move { post_with(&a, "/api/probe", &k, Some("dup-submit-0001")).await.0 })); }
    for h in hs { let s = h.await.unwrap(); assert!(s == StatusCode::OK || s == StatusCode::CONFLICT, "{s}"); }
    assert_eq!(c.0.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn server_errors_are_not_stored_so_retry_can_succeed(pool: PgPool) {
    let st = common::state(pool.clone()).await;
    let app = probe_router(st, Counter(Arc::new(AtomicUsize::new(0))));
    let u = common::make_user(&pool, "u", true, &[], &[]).await;
    let key = common::session_for(&pool, u).await;
    let (s, b, _) = post_with(&app, "/api/boom", &key, Some("retry-after-5xx")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(b["error"]["code"], "UNAVAILABLE");
    assert!(b["error"]["hint"].as_str().unwrap().contains("not saved"));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_keys").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
    let (s, _, _) = post_with(&app, "/api/fail500", &key, Some("retry-after-500")).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_keys").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn request_id_hint_and_security_headers(pool: PgPool) {
    let (app, _) = common::app(pool).await;
    let resp = app.clone().oneshot(axum::http::Request::builder().uri("/api/nope").body(axum::body::Body::empty()).unwrap()).await.unwrap();
    assert!(resp.headers().contains_key("x-request-id"));
    assert_eq!(resp.headers()["x-content-type-options"], "nosniff");
    assert!(resp.headers()["content-security-policy"].to_str().unwrap().contains("script-src 'self'"));
    let resp = app.clone().oneshot(axum::http::Request::builder().uri("/api/events").body(axum::body::Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let bytes = http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["error"]["hint"].as_str().unwrap().contains("Sign in"));
}

#[sqlx::test(migrations = "./migrations")]
async fn cross_origin_writes_rejected(pool: PgPool) {
    let (app, _st) = common::app(pool.clone()).await;
    let u = common::make_user(&pool, "admin", true, &[], &[]).await;
    let key = common::session_for(&pool, u).await;
    let req = axum::http::Request::builder().method("POST").uri("/api/admin/users").header("x-auth-key", &key)
        .header("origin", "https://evil.example").header("host", "plant.local").header("content-type", "application/json")
        .body(axum::body::Body::from("{}")).unwrap();
    assert_eq!(app.clone().oneshot(req).await.unwrap().status(), StatusCode::FORBIDDEN);
    // NOTE: the QR feature-gate (FEATURE_DISABLED while features.qr_labels=false) is verified in the QA phase once QR routes are merged.
}

#[sqlx::test(migrations = "./migrations")]
async fn health_reports_shutdown_and_streams_are_told_to_reconnect(pool: PgPool) {
    let (app, st) = common::app(pool.clone()).await;
    let (s, b) = common::call(&app, "GET", "/api/health", None, None).await;
    assert_eq!((s, b["status"].as_str().unwrap()), (StatusCode::OK, "ok"));
    st.shutdown.trigger();
    let (s, b) = common::call(&app, "GET", "/api/health", None, None).await;
    assert_eq!((s, b["status"].as_str().unwrap()), (StatusCode::SERVICE_UNAVAILABLE, "shutting_down"));
}

#[sqlx::test(migrations = "./migrations")]
async fn confirm_resend_and_dry_run_work_with_the_same_key(pool: PgPool) {
    let st = common::state(pool.clone()).await;
    let c = Counter(Arc::new(AtomicUsize::new(0)));
    let app = probe_router(st, c.clone());
    let u = common::make_user(&pool, "u", true, &[], &[]).await;
    let key = common::session_for(&pool, u).await;
    // 1st attempt -> CONFIRM_REQUIRED (not stored), resend with confirm:true and the SAME key succeeds
    let (s, b, _) = post_body(&app, "/api/confirm", &key, Some("confirm-flow-0001"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(b["error"]["code"], "CONFIRM_REQUIRED");
    assert_eq!(b["error"]["details"]["warnings"][0], "10x usual");
    let (s, b, r) = post_body(&app, "/api/confirm", &key, Some("confirm-flow-0001"), Some(json!({"confirm": true}))).await;
    assert_eq!((s, b["done"].as_bool(), r), (StatusCode::OK, Some(true), None));
    // dry run then real, same key
    let (s, _, _) = post_body(&app, "/api/probe?dry_run=1", &key, Some("dry-then-real-01"), None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, r) = post_with(&app, "/api/probe", &key, Some("dry-then-real-01")).await;
    assert_eq!((s, r), (StatusCode::OK, None));
    assert_eq!(c.0.load(Ordering::SeqCst), 2, "dry run and real call must each run the handler");
    // same key, different body after a success => conflict (never replay a different request's response)
    let (s, b, _) = post_body(&app, "/api/confirm", &key, Some("confirm-flow-0001"), Some(json!({"confirm": true, "x": 1}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(b["error"]["code"], "IDEMPOTENCY_CONFLICT");
}
