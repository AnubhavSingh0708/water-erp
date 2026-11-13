//! Shared integration-test helpers. Use with `#[sqlx::test(migrations = "./migrations")]` (needs DATABASE_URL pointing at a
//! Postgres where the role can CREATE DATABASE, e.g. postgres://erp:erp@127.0.0.1/water_erp_test).
#![allow(dead_code)]
use axum::{body::Body, http::{Request, StatusCode}, Router};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;
use water_erp::{auth, config::{Config, Mode}, routes, server};

pub async fn state(pool: PgPool) -> water_erp::state::AppState {
    let cfg = Config {
        mode: Mode::Local, domain: None, bind: "127.0.0.1:8443".parse().unwrap(), database_url: String::new(),
        tls_cert: None, tls_key: None, static_dir: "static".into(), data_dir: "data".into(),
        vapid_private_key: None, vapid_public_key: None, vapid_subject: "https://localhost".into(), setup_admin: None, insecure_http: true,
    };
    server::build_state(cfg, pool).await.unwrap()
}

pub async fn app(pool: PgPool) -> (Router, water_erp::state::AppState) {
    let st = state(pool).await;
    (routes::build_router(st.clone()), st)
}

/// Insert a user (password hash is a placeholder – log in via `session_for`) and return (user_id).
pub async fn make_user(pool: &PgPool, username: &str, is_admin: bool, pages: &[&str], subs: &[&str]) -> Uuid {
    sqlx::query_scalar("INSERT INTO users (username, full_name, password_hash, is_admin, allowed_pages, subscribed_notifications)
                        VALUES ($1,$1,'x',$2,$3,$4) RETURNING id")
        .bind(username).bind(is_admin).bind(serde_json::json!(pages)).bind(serde_json::json!(subs))
        .fetch_one(pool).await.unwrap()
}

/// Create a session row directly; returns the raw key to send in `X-Auth-Key`.
pub async fn session_for(pool: &PgPool, user_id: Uuid) -> String {
    let key = auth::generate_key();
    sqlx::query("INSERT INTO user_sessions (user_id, key_hash, expires_at) VALUES ($1,$2, now() + interval '12 hours')")
        .bind(user_id).bind(auth::hash_key(&key)).execute(pool).await.unwrap();
    key
}

pub async fn call(app: &Router, method: &str, uri: &str, key: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(k) = key { b = b.header("x-auth-key", k); }
    let req = if let Some(j) = body {
        b.header("content-type", "application/json").body(Body::from(j.to_string())).unwrap()
    } else { b.body(Body::empty()).unwrap() };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, v)
}
