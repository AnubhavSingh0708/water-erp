//! Shared error type. Every API error is serialised as
//! `{ "error": { "code": "...", "message": "...", "fields": { "field": "msg" }? , "details": {..}? } }`.
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("validation failed")]
    Validation(BTreeMap<String, String>),
    #[error("authentication required")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(String),
    #[error("{0} not found")]
    NotFound(String),
    /// Business-rule conflict. `code` is a stable machine code, e.g. QC_LOCKED, INSUFFICIENT_STOCK, CAPACITY_EXCEEDED.
    #[error("{message}")]
    Conflict { code: &'static str, message: String, details: Value },
    #[error("too many attempts")]
    Locked(String),
    /// Database/pool temporarily unavailable – nothing was saved; safe to retry.
    #[error("service unavailable")]
    Unavailable,
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn bad(msg: impl Into<String>) -> Self { AppError::BadRequest(msg.into()) }
    pub fn forbidden(msg: impl Into<String>) -> Self { AppError::Forbidden(msg.into()) }
    pub fn not_found(what: impl Into<String>) -> Self { AppError::NotFound(what.into()) }
    pub fn conflict(code: &'static str, msg: impl Into<String>) -> Self {
        AppError::Conflict { code, message: msg.into(), details: Value::Null }
    }
    pub fn conflict_with(code: &'static str, msg: impl Into<String>, details: Value) -> Self {
        AppError::Conflict { code, message: msg.into(), details }
    }
    /// Suspicious-but-possible entry: client must resend the same request with `"confirm": true`.
    pub fn confirm(msg: impl Into<String>, warnings: Vec<String>) -> Self {
        AppError::Conflict { code: "CONFIRM_REQUIRED", message: msg.into(), details: json!({ "warnings": warnings }) }
    }
    pub fn field(field: &str, msg: &str) -> Self {
        let mut m = BTreeMap::new();
        m.insert(field.to_string(), msg.to_string());
        AppError::Validation(m)
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        if let sqlx::Error::RowNotFound = e { return AppError::NotFound("record".into()); }
        if matches!(e, sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) | sqlx::Error::Tls(_)) { return AppError::Unavailable; }
        if let Some(db) = e.as_database_error() {
            match db.code().as_deref() {
                Some("23505") => return AppError::conflict("DUPLICATE", format!("Already exists ({})", db.constraint().unwrap_or("unique"))),
                Some("23503") => return AppError::conflict("IN_USE", "Record is referenced by other data or the referenced record does not exist"),
                Some("23514") => return AppError::BadRequest(format!("Value rejected by rule ({})", db.constraint().unwrap_or("check"))),
                Some("23502") => return AppError::BadRequest("A required value is missing".into()),
                Some("40P01") | Some("40001") => return AppError::conflict("RETRY", "Concurrent update, please retry"),
                _ => {}
            }
        }
        AppError::Internal(anyhow::Error::new(e))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message, extra): (StatusCode, String, String, Value) = match &self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, "BAD_REQUEST".into(), m.clone(), Value::Null),
            AppError::Validation(f) => (StatusCode::UNPROCESSABLE_ENTITY, "VALIDATION".into(), "Please correct the highlighted fields".into(), json!({ "fields": f })),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED".into(), "Authentication required".into(), Value::Null),
            AppError::Forbidden(m) => (StatusCode::FORBIDDEN, "FORBIDDEN".into(), m.clone(), Value::Null),
            AppError::NotFound(w) => (StatusCode::NOT_FOUND, "NOT_FOUND".into(), format!("{w} not found"), Value::Null),
            AppError::Conflict { code, message, details } => (StatusCode::CONFLICT, (*code).into(), message.clone(), if details.is_null() { Value::Null } else { json!({ "details": details }) }),
            AppError::Locked(m) => (StatusCode::TOO_MANY_REQUESTS, "LOCKED".into(), m.clone(), Value::Null),
            AppError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "UNAVAILABLE".into(), "The server is busy or the database is unreachable. Your entry was NOT saved.".into(), Value::Null),
            AppError::Internal(e) => {
                tracing::error!(error = ?e, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL".into(), "Internal server error".into(), Value::Null)
            }
        };
        let mut body = json!({ "code": code, "message": message });
        if let Some(h) = hint_for(&code) { body["hint"] = json!(h); }
        if let Value::Object(m) = extra { for (k, v) in m { body[k] = v; } }
        (status, Json(json!({ "error": body }))).into_response()
    }
}

/// Convenience for `?` on anyhow-able things inside handlers.
pub trait Ctx<T> { fn ctx(self, what: &str) -> AppResult<T>; }
impl<T, E: std::error::Error + Send + Sync + 'static> Ctx<T> for Result<T, E> {
    fn ctx(self, what: &str) -> AppResult<T> { self.map_err(|e| AppError::Internal(anyhow::Error::new(e).context(what.to_string()))) }
}

/// Actionable next step shown under the error message in the UI. Keyed by error code.
pub fn hint_for(code: &str) -> Option<&'static str> {
    Some(match code {
        "UNAUTHORIZED" => "Your session has ended. Sign in again - anything you typed is kept as a draft.",
        "FORBIDDEN" => "Ask an administrator to give your account access to this page.",
        "VALIDATION" => "Check the highlighted fields and try again.",
        "DUPLICATE" => "This looks like it was already entered. Open the existing record instead of creating it again.",
        "IN_USE" => "Other records still use this. Deactivate it instead of deleting.",
        "INSUFFICIENT_STOCK" => "Check the physical count. If stock really exists, receive it (GRN) or post a counted adjustment first.",
        "QC_LOCKED" => "This batch is not approved by Lab QC yet, so it cannot be loaded or dispatched.",
        "CAPACITY_EXCEEDED" => "Reduce the quantity, remove an item, or choose a bigger vehicle.",
        "INVALID_STATE" => "Someone else may have already changed this record. The screen was refreshed - review it and try again.",
        "VEHICLE_DOCS_EXPIRED" => "Renew the vehicle document in Maintenance before issuing a gate pass.",
        "CONFIRM_REQUIRED" => "This entry looks unusual. Check the numbers, then confirm to continue.",
        "IDEMPOTENCY_CONFLICT" => "This request key was already used for a different action. Reload the page and try again.",
        "IN_PROGRESS" => "The same action is still being processed. Wait a few seconds - do not submit again.",
        "RETRY" => "Another user changed the same stock at the same moment. Simply try again.",
        "LOCKED" => "Too many attempts. Wait a few minutes and try again.",
        "UNAVAILABLE" => "Wait a moment and retry. Your entry was not saved. If it keeps happening, tell the administrator.",
        "RESERVED_STOCK" => "Some of these crates are held by an open loading trip. Remove them from that trip first, then try again.",
        "PROTECTED" => "This record is built in and cannot be changed this way.",
        "FEATURE_DISABLED" => "This feature is switched off or not configured. Ask an administrator.",
        "INTERNAL" => "Something went wrong on the server. Your entry was not saved. Quote the reference shown to the administrator.",
        _ => return None,
    })
}
