//! Request extractors that answer rejections with the standard error envelope instead of axum's plain-text bodies.
//! Shared by the C2 route modules (jars, maintenance, notifications).
use crate::error::AppError;
use axum::{
    extract::{rejection::JsonRejection, FromRequest, FromRequestParts, Path, Query, Request},
    http::request::Parts,
    Json,
};
use serde::{de::DeserializeOwned, Deserialize};
use std::collections::BTreeMap;

/// `Json<T>` whose rejections are `AppError`s: malformed JSON / wrong content type -> 400, schema errors (unknown or
/// missing field, wrong type) -> 422 `VALIDATION`.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = AppError;
    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(ApiJson(v)),
            Err(JsonRejection::JsonDataError(e)) => {
                let mut m = BTreeMap::new();
                m.insert("body".to_string(), e.body_text());
                Err(AppError::Validation(m))
            }
            Err(JsonRejection::MissingJsonContentType(_)) => Err(AppError::bad("Content-Type must be application/json")),
            Err(JsonRejection::JsonSyntaxError(_)) => Err(AppError::bad("Request body is not valid JSON")),
            Err(_) => Err(AppError::bad("Request body could not be read")),
        }
    }
}

/// `Query<T>` with an envelope error.
pub struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(v)) => Ok(ApiQuery(v)),
            Err(_) => Err(AppError::bad("Invalid query string")),
        }
    }
}

/// `Path<T>` with an envelope error (malformed ids -> 400 instead of axum's plain text).
pub struct ApiPath<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(v)) => Ok(ApiPath(v)),
            Err(_) => Err(AppError::bad("Invalid id in the URL")),
        }
    }
}

/// `?dry_run=1` / `?dry_run=true` / bare `?dry_run`.
pub fn is_dry_run(uri: &axum::http::Uri) -> bool {
    uri.query().map_or(false, |q| {
        q.split('&').any(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            k == "dry_run" && !matches!(v, "0" | "false" | "no")
        })
    })
}

/// Optional UUID query parameter: empty -> None, malformed -> 422.
pub fn opt_uuid(field: &str, v: &Option<String>) -> Result<Option<uuid::Uuid>, AppError> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => uuid::Uuid::parse_str(s).map(Some).map_err(|_| AppError::field(field, "must be a valid id")),
    }
}

/// Optional `YYYY-MM-DD` query parameter: empty -> None, malformed -> 422.
pub fn opt_date(field: &str, v: &Option<String>) -> Result<Option<chrono::NaiveDate>, AppError> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map(Some).map_err(|_| AppError::field(field, "must be a date (YYYY-MM-DD)")),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum IntOrStr { I(i64), S(String) }

/// Whole number that may arrive as a JSON integer or a numeric string (never a float).
pub fn de_int<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    use serde::de::Error;
    match IntOrStr::deserialize(d)? {
        IntOrStr::I(i) => Ok(i),
        IntOrStr::S(s) => s.trim().parse::<i64>().map_err(|_| D::Error::custom("must be a whole number")),
    }
}

/// Optional variant of [`de_int`] (`null` / missing / empty string -> None).
pub fn de_opt_int<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    use serde::de::Error;
    match Option::<IntOrStr>::deserialize(d)? {
        None => Ok(None),
        Some(IntOrStr::I(i)) => Ok(Some(i)),
        Some(IntOrStr::S(s)) if s.trim().is_empty() => Ok(None),
        Some(IntOrStr::S(s)) => s.trim().parse::<i64>().map(Some).map_err(|_| D::Error::custom("must be a whole number")),
    }
}
