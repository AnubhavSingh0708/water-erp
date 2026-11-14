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
pub mod godowns;    // B  /api/godowns/*  (inter-godown transfers)
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

/// Liveness + readiness. 503 while shutting down or when the database is unreachable (load balancers/monitors drain on this).
async fn health(State(s): State<AppState>) -> (axum::http::StatusCode, Json<Value>) {
    let t = std::time::Instant::now();
    let db_ok = tokio::time::timeout(std::time::Duration::from_secs(3), sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&s.db))
        .await.map(|r| r.is_ok()).unwrap_or(false);
    let stopping = s.shutdown.is_triggered();
    let status = if stopping { "shutting_down" } else if db_ok { "ok" } else { "degraded" };
    let code = if db_ok && !stopping { axum::http::StatusCode::OK } else { axum::http::StatusCode::SERVICE_UNAVAILABLE };
    (code, Json(json!({ "status": status, "db": db_ok, "db_ms": t.elapsed().as_millis() as u64, "version": env!("CARGO_PKG_VERSION") })))
}

/// Adds `X-Request-Id` (UUID) to every response and tracing span so users can quote it and admins can find the log line.
async fn request_id(req: Request, next: Next) -> Response {
    let id = uuid::Uuid::new_v4().to_string();
    let span = tracing::info_span!("req", id = %id, method = %req.method(), path = %req.uri().path());
    let mut resp = tracing::Instrument::instrument(next.run(req), span).await;
    if let Ok(v) = HeaderValue::from_str(&id) { resp.headers_mut().insert(HeaderName::from_static("x-request-id"), v); }
    resp
}

/// Optional feature gate (`features.qr_labels`): QR crate-set endpoints answer 404 FEATURE_DISABLED while the feature is off.
async fn qr_feature_gate(State(s): State<AppState>, req: Request, next: Next) -> Result<Response, AppError> {
    if !s.settings.get().qr_labels_enabled {
        return Err(AppError::Conflict { code: "FEATURE_DISABLED", message: "QR labels are switched off. Load stock by product and batch instead.".into(), details: Value::Null });
    }
    Ok(next.run(req).await)
}

/// Idempotency for retried writes (weak Wi-Fi / double tap). Clients send `Idempotency-Key: <uuid>` on POST/PUT/PATCH/DELETE.
/// Same user+key => the first response is replayed (`Idempotent-Replay: true`) and the handler is NOT run again.
/// While the first request is still running a duplicate gets 409 IN_PROGRESS. 5xx responses are not stored (safe to retry).
/// Minutes a key without a stored response (request cut off by the timeout, or the process died mid-request) stays reserved. See `idempotency`.
pub const STALE_KEY_MINUTES: i64 = 10;

pub async fn idempotency(State(s): State<AppState>, req: Request, next: Next) -> Result<Response, AppError> {
    if !matches!(*req.method(), Method::POST | Method::PUT | Method::PATCH | Method::DELETE) { return Ok(next.run(req).await); }
    let Some(key) = req.headers().get("idempotency-key").and_then(|v| v.to_str().ok()).map(str::to_owned) else { return Ok(next.run(req).await) };
    // dry runs never touch the key (a dry run followed by the real submit must run the handler)
    if crate::util::dry_run_in_query(req.uri().query()) { return Ok(next.run(req).await); }
    if key.len() < 8 || key.len() > 64 || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(AppError::field("Idempotency-Key", "must be 8-64 characters (letters, digits, - _)"));
    }
    let user = req.extensions().get::<auth::AuthUser>().ok_or(AppError::Unauthorized)?.id;
    // fingerprint the full request so a reused key with a different body (e.g. confirm:true added) is detected
    let (parts, body) = req.into_parts();
    let body_bytes = axum::body::to_bytes(body, 2 * 1024 * 1024).await.map_err(|_| AppError::bad("request body too large"))?;
    let method = parts.method.to_string();
    let path = parts.uri.path().to_string();
    let fp = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(method.as_bytes()); h.update(b"|"); h.update(path.as_bytes()); h.update(b"|");
        h.update(parts.uri.query().unwrap_or("").as_bytes()); h.update(b"|"); h.update(&body_bytes);
        hex::encode(h.finalize())
    };
    let req = Request::from_parts(parts, axum::body::Body::from(body_bytes));
    // A key whose request never finished (status NULL) is NOT freed after the 30 s request timeout: the timeout drops the handler's
    // future at an arbitrary await point, which may be just after its transaction COMMITTED but before the key was updated. Re-running such a
    // request would write twice, so a pending key is only taken over after `STALE_KEY_MINUTES` - far longer than any retry loop of a client,
    // short enough that a really abandoned key (process killed mid-request) does not block that action for ever.
    sqlx::query("DELETE FROM idempotency_keys WHERE user_id=$1 AND key=$2 AND status IS NULL AND created_at < now() - make_interval(mins => $3)")
        .bind(user).bind(&key).bind(STALE_KEY_MINUTES as i32).execute(&s.db).await?;
    let inserted = sqlx::query("INSERT INTO idempotency_keys (user_id, key, method, path, fingerprint) VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
        .bind(user).bind(&key).bind(&method).bind(&path).bind(&fp).execute(&s.db).await?.rows_affected();
    if inserted == 0 {
        let row: (String, Option<i32>, Option<Vec<u8>>, f64) =
            sqlx::query_as("SELECT fingerprint, status, body, EXTRACT(EPOCH FROM (now() - created_at))::float8 FROM idempotency_keys WHERE user_id=$1 AND key=$2")
                .bind(user).bind(&key).fetch_one(&s.db).await?;
        if row.0 != fp { return Err(AppError::conflict("IDEMPOTENCY_CONFLICT", "Idempotency key was already used for a different request")); }
        return match (row.1, row.2) {
            (Some(st), Some(body)) => {
                let mut r = Response::new(axum::body::Body::from(body));
                *r.status_mut() = axum::http::StatusCode::from_u16(st as u16).unwrap_or(axum::http::StatusCode::OK);
                r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                r.headers_mut().insert(HeaderName::from_static("idempotent-replay"), HeaderValue::from_static("true"));
                Ok(r)
            }
            // older than the request timeout and still unfinished: the first attempt timed out / was interrupted and its outcome is UNKNOWN
            _ if row.3 > 35.0 => Err(AppError::conflict("IN_PROGRESS",
                format!("An earlier attempt with this key timed out or was interrupted, so it is not known whether it was saved. Check the records before doing it again; this key is released after {STALE_KEY_MINUTES} minutes"))),
            _ => Err(AppError::conflict("IN_PROGRESS", "The same request is still being processed")),
        };
    }
    if rand::random::<u8>() < 3 { // ~1 % of writes: prune old keys
        let _ = sqlx::query("DELETE FROM idempotency_keys WHERE created_at < now() - interval '2 days'").execute(&s.db).await;
    }
    let resp = next.run(req).await;
    let (parts, body) = resp.into_parts();
    // The handler has already COMMITTED when we get here: a response that is merely large must never turn into a 500 (the client would
    // retry and, once the stale-key window passes, the write would run a second time).
    let bytes = axum::body::to_bytes(body, 64 * 1024 * 1024).await.map_err(|e| AppError::Internal(anyhow::anyhow!("idempotency body: {e}")))?;
    // Finalise the key in a detached task: if the request timeout drops this future right after the handler committed, the key is still
    // completed (replayable) instead of staying pending. A failure to remember the response is logged, never surfaced: the write is committed.
    let ok = parts.status.is_success();
    let status = parts.status.as_u16() as i32;
    const MAX_STORED: usize = 512 * 1024;
    let stored: Vec<u8> = if !ok { vec![] } else if bytes.len() <= MAX_STORED { bytes.to_vec() } else {
        // Responses over 512 KB are returned to the caller but stored as a small marker: a retry gets "already processed" instead of a re-run.
        br#"{"idempotent_replay_truncated":true,"message":"This request was already processed. Its response was too large to store - reload the page to see the result."}"#.to_vec()
    };
    let db = s.db.clone();
    let finalise = tokio::spawn(async move {
        if ok {
            sqlx::query("UPDATE idempotency_keys SET status=$3, body=$4 WHERE user_id=$1 AND key=$2")
                .bind(user).bind(&key).bind(status).bind(&stored).execute(&db).await
                .map_err(|e| tracing::warn!(error = %e, "could not store idempotent response")).ok();
        } else {
            // only successes are replayed: validation errors, CONFIRM_REQUIRED, 5xx ... must be re-runnable with the same key
            let _ = sqlx::query("DELETE FROM idempotency_keys WHERE user_id=$1 AND key=$2").bind(user).bind(&key).execute(&db).await;
        }
    });
    let _ = finalise.await;
    Ok(Response::from_parts(parts, axum::body::Body::from(bytes)))
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
        .nest("/api/godowns", guarded(&["warehouse"], godowns::router()))
        .nest("/api/qr", guarded(&["qr-labels", "loading"], qr::router().layer(middleware::from_fn_with_state(state.clone(), qr_feature_gate))))
        .nest("/api/loading", guarded(&["loading"], loading::router()))
        .nest("/api/jars", guarded(&["jars"], jars::router()))
        .nest("/api/maintenance", guarded(&["maintenance"], maintenance::router()))
        .nest("/api/admin", admin::router().layer(middleware::from_fn(auth::admin_guard)))
        .layer(middleware::from_fn_with_state(state.clone(), idempotency))
        .layer(middleware::from_fn_with_state(state.clone(), auth::auth_middleware));

    let static_dir = state.cfg.static_dir.clone();
    let sec = |n: &'static str, v: &'static str| SetResponseHeaderLayer::overriding(HeaderName::from_static(n), HeaderValue::from_static(v));

    // HSTS only when we terminate TLS ourselves
    let hsts = SetResponseHeaderLayer::overriding(HeaderName::from_static("strict-transport-security"),
        HeaderValue::from_static(if state.cfg.insecure_http { "max-age=0" } else { "max-age=31536000" }));

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
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; worker-src 'self'; manifest-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'"))
        .layer(sec("cross-origin-opener-policy", "same-origin"))
        .layer(hsts)
        .layer(CompressionLayer::new())
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(axum::http::StatusCode::SERVICE_UNAVAILABLE, std::time::Duration::from_secs(30)))
        .layer(middleware::from_fn(request_id))
        .layer(TraceLayer::new_for_http())
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024))
        .with_state(state)
}
