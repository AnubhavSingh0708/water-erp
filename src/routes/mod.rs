//! Router assembly. Each module owns one URL prefix (see API_REFERENCE.md). Page guards are applied HERE so that
//! the access matrix is visible in one place.
use crate::{auth, error::AppError, state::AppState};
use axum::{
    extract::{Request, State},
    http::{header, HeaderName, HeaderValue, Method},
    middleware::{self, Next},
    response::Response,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use tower_http::{compression::CompressionLayer, services::ServeDir, set_header::SetResponseHeaderLayer, trace::TraceLayer};

pub mod admin;      // A  /api/admin/*
pub mod auth_routes; // A  /api/auth/login, /api/setup/*, /api/me/*
pub mod dashboard;  // B  /api/dashboard
pub mod jars;       // C  /api/jars/*
pub mod loading;    // C  /api/loading/*
pub mod maintenance; // C /api/maintenance/*
pub mod materials;  // B  /api/raw-materials/*
pub mod notifications; // C /api/notifications/*
pub mod production; // B  /api/production/*
pub mod qc;         // B  /api/qc/*
pub mod qr;         // C  /api/qr/*
pub mod ref_data;   // B  /api/ref/*
pub mod warehouse;  // B  /api/warehouse/*

async fn health(State(s): State<AppState>) -> Json<Value> {
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&s.db).await.is_ok();
    Json(json!({ "status": if db_ok { "ok" } else { "degraded" }, "db": db_ok, "version": env!("CARGO_PKG_VERSION") }))
}

async fn api_not_found() -> AppError { AppError::NotFound("endpoint".into()) }

/// Reject cross-origin state-changing requests: `Origin` host must equal the `Host` header or `--domain`.
async fn origin_check(State(s): State<AppState>, req: Request, next: Next) -> Result<Response, AppError> {
    if matches!(*req.method(), Method::POST | Method::PUT | Method::PATCH | Method::DELETE) {
        if let Some(origin) = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            let oh = origin.split("://").nth(1).unwrap_or(origin).to_ascii_lowercase();
            let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
            let dom = s.cfg.domain.clone().unwrap_or_default().to_ascii_lowercase();
            let strip = |h: &str| h.split(':').next().unwrap_or("").to_string();
            let ok = oh == host || (!dom.is_empty() && strip(&oh) == strip(&dom));
            if !ok { return Err(AppError::forbidden("Cross-origin request rejected")); }
        }
    }
    Ok(next.run(req).await)
}

pub fn build_router(state: AppState) -> Router {
    use auth::guarded;
    let public = Router::new()
        .route("/api/health", get(health))
        .nest("/api/auth", auth_routes::public_router())
        .nest("/api/setup", auth_routes::setup_router());

    let protected = Router::new()
        .route("/api/events", get(crate::events::stream_handler))
        .nest("/api/me", auth_routes::me_router())
        .nest("/api/ref", ref_data::router())
        .nest("/api/notifications", notifications::router())
        .nest("/api/dashboard", guarded(&["dashboard"], dashboard::router()))
        .nest("/api/raw-materials", guarded(&["raw-materials"], materials::router()))
        .nest("/api/production", guarded(&["production"], production::router()))
        .nest("/api/qc", guarded(&["qc"], qc::router()))
        .nest("/api/warehouse", guarded(&["warehouse"], warehouse::router()))
        .nest("/api/qr", guarded(&["qr-labels", "loading"], qr::router()))
        .nest("/api/loading", guarded(&["loading"], loading::router()))
        .nest("/api/jars", guarded(&["jars"], jars::router()))
        .nest("/api/maintenance", guarded(&["maintenance"], maintenance::router()))
        .nest("/api/admin", admin::router().layer(middleware::from_fn(auth::admin_guard)))
        .layer(middleware::from_fn_with_state(state.clone(), auth::auth_middleware));

    let static_dir = state.cfg.static_dir.clone();
    let sec = |n: &'static str, v: &'static str| SetResponseHeaderLayer::overriding(HeaderName::from_static(n), HeaderValue::from_static(v));

    Router::new()
        .merge(public)
        .merge(protected)
        .route("/api/{*rest}", axum::routing::any(api_not_found))
        .fallback_service(ServeDir::new(static_dir).append_index_html_on_directories(true))
        .layer(middleware::from_fn_with_state(state.clone(), origin_check))
        .layer(sec("x-content-type-options", "nosniff"))
        .layer(sec("x-frame-options", "DENY"))
        .layer(sec("referrer-policy", "same-origin"))
        .layer(sec("permissions-policy", "camera=(self), geolocation=()"))
        .layer(sec("content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; worker-src 'self'; manifest-src 'self'; frame-ancestors 'none'"))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024))
        .with_state(state)
}
