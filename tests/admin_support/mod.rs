#![allow(dead_code)]
use axum::{body::Body, http::{Request, StatusCode}, Router};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

pub async fn req(app: &Router, method: &str, uri: &str, key: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(k) = key { b = b.header("x-auth-key", k); }
    let r = match body {
        Some(j) => b.header("content-type", "application/json").body(Body::from(j.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(r).await.unwrap();
    let st = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into())))
}

/// (admin_user_id, key)
pub async fn admin(pool: &PgPool, name: &str) -> (Uuid, String) {
    let id = crate::common::make_user(pool, name, true, &[], &[]).await;
    let k = crate::common::session_for(pool, id).await;
    (id, k)
}

/// Non-admin with every page except admin.
pub async fn plain_user(pool: &PgPool, name: &str) -> (Uuid, String) {
    let pages: Vec<&str> = water_erp::auth::PAGES.iter().copied().filter(|p| *p != "admin").collect();
    let id = crate::common::make_user(pool, name, false, &pages, &[]).await;
    let k = crate::common::session_for(pool, id).await;
    (id, k)
}
