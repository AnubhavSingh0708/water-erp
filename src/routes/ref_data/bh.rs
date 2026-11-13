//! Shared helpers for the "Owner B" route modules (materials, production, qc, warehouse, ref, dashboard):
//! envelope-preserving extractors, parameterised JSON/paging queries, CSV export with injection defence,
//! decimal parsing, dry-run support and ledger "effects" for review screens.
use crate::{
    error::{AppError, AppResult},
    stock,
    validate,
};
use axum::{
    extract::{FromRequest, FromRequestParts, Request},
    http::{header, request::Parts, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Deserialize, Deserializer};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgExecutor};
use std::str::FromStr;
use uuid::Uuid;

// ------------------------------------------------------------------------------------------------------------------
// Extractors that answer with the standard error envelope instead of axum's plain-text rejections.
// ------------------------------------------------------------------------------------------------------------------

/// JSON body extractor; malformed JSON / unknown fields => `400 BAD_REQUEST` envelope.
pub struct J<T>(pub T);
impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for J<T> {
    type Rejection = AppError;
    async fn from_request(req: Request, state: &S) -> Result<Self, AppError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(J(v)),
            Err(e) => {
                let mut m = e.body_text();
                m.truncate(300);
                Err(AppError::bad(format!("Invalid request body: {m}")))
            }
        }
    }
}

/// Query-string extractor with envelope errors.
pub struct Q<T>(pub T);
impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for Q<T> {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, AppError> {
        axum::extract::Query::<T>::try_from_uri(&parts.uri)
            .map(|q| Q(q.0))
            .map_err(|_| AppError::bad("Invalid query string (check dates, numbers and ids)"))
    }
}

/// Single `{id}` path parameter parsed as UUID (anything else => 404, never a plain-text rejection).
pub struct Id(pub Uuid);
impl<S: Send + Sync> FromRequestParts<S> for Id {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, AppError> {
        let axum::extract::Path(s) = axum::extract::Path::<String>::from_request_parts(parts, state).await
            .map_err(|_| AppError::not_found("record"))?;
        Uuid::parse_str(&s).map(Id).map_err(|_| AppError::not_found("record"))
    }
}

/// `?dry_run=1` flag.
#[derive(Debug, Default, Deserialize)]
pub struct DryQ { #[serde(default)] pub dry_run: Option<String> }
impl DryQ {
    pub fn on(&self) -> bool { matches!(self.dry_run.as_deref(), Some("1") | Some("true") | Some("")) }
}

// ------------------------------------------------------------------------------------------------------------------
// Decimal input: JSON string or number (never a float: numbers are re-read from their decimal text).
// ------------------------------------------------------------------------------------------------------------------
#[derive(Debug, Clone, Copy)]
pub struct D(pub Decimal);
impl<'de> Deserialize<'de> for D {
    fn deserialize<De: Deserializer<'de>>(d: De) -> Result<Self, De::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw { S(String), N(serde_json::Number) }
        let s = match Raw::deserialize(d)? { Raw::S(s) => s, Raw::N(n) => n.to_string() };
        let t = s.trim();
        Decimal::from_str(t).or_else(|_| Decimal::from_scientific(t)).map(D)
            .map_err(|_| serde::de::Error::custom("invalid number"))
    }
}

/// Requires an integral quantity (pcs, crates) > 0 within qty bounds.
pub fn whole_qty(field: &str, d: Decimal) -> AppResult<Decimal> {
    validate::qty_positive(field, d)?;
    if d.fract() != Decimal::ZERO { return Err(AppError::field(field, "must be a whole number")); }
    Ok(d)
}
pub fn whole_i64(field: &str, d: Decimal) -> AppResult<i64> {
    whole_qty(field, d)?;
    d.trunc().to_string().parse::<i64>().map_err(|_| AppError::field(field, "is too large"))
}
pub fn opt_text(field: &str, v: &Option<String>, max: usize) -> AppResult<String> {
    match v { Some(s) => validate::text(field, s, max), None => Ok(String::new()) }
}
pub fn shift(s: &str) -> AppResult<&'static str> {
    match s { "DAY" => Ok("DAY"), "NIGHT" => Ok("NIGHT"), "GENERAL" => Ok("GENERAL"), _ => Err(AppError::field("shift", "must be DAY, NIGHT or GENERAL")) }
}
pub fn zone(field: &str, z: &str) -> AppResult<&'static str> {
    stock::ZONES.iter().copied().find(|x| *x == z).ok_or_else(|| AppError::field(field, "unknown zone"))
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ItemKind { Raw, Product }
impl ItemKind {
    pub fn it(self) -> stock::ItemType { match self { ItemKind::Raw => stock::ItemType::Raw, ItemKind::Product => stock::ItemType::Product } }
}

/// Plant-local date window validation for operational entries (no future dates, bounded back-dating).
pub fn check_entry_date(field: &str, d: NaiveDate, today: NaiveDate, max_back_days: i64) -> AppResult<()> {
    if d > today { return Err(AppError::field(field, "must not be in the future")); }
    if (today - d).num_days() > max_back_days { return Err(AppError::field(field, &format!("must not be more than {max_back_days} days in the past"))); }
    Ok(())
}

// ------------------------------------------------------------------------------------------------------------------
// Parameterised JSON queries. SQL text is `&'static str` so user input can never be formatted into it.
// ------------------------------------------------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub enum Arg { T(Option<String>), U(Option<Uuid>), D(Option<NaiveDate>), I(i64), B(bool), N(Option<Decimal>) }
impl Arg {
    pub fn t(v: &Option<String>) -> Arg { Arg::T(v.clone()) }
    pub fn like(v: &Option<String>) -> Arg {
        Arg::T(v.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()).map(|s| validate::like_pattern(&s.chars().take(100).collect::<String>())))
    }
}

pub async fn jval<'e>(db: impl PgExecutor<'e>, sql: &str, args: &[Arg]) -> AppResult<Value> {
    let mut q = sqlx::query_scalar::<_, Value>(sql);
    for a in args {
        q = match a {
            Arg::T(v) => q.bind(v.clone()), Arg::U(v) => q.bind(*v), Arg::D(v) => q.bind(*v),
            Arg::I(v) => q.bind(*v), Arg::B(v) => q.bind(*v), Arg::N(v) => q.bind(*v),
        };
    }
    Ok(q.fetch_one(db).await?)
}

/// `{items,total}` from a base query that yields a jsonb column `j`. `order` is an SQL ORDER BY list over the base columns.
pub async fn paged<'e>(db: impl PgExecutor<'e>, base_sql: &'static str, order: &'static str, args: &[Arg], limit: i64, offset: i64) -> AppResult<Value> {
    let n = args.len();
    let sql = format!(
        "WITH base AS ({base_sql}), pg AS (SELECT j, row_number() OVER (ORDER BY {order}) AS rn FROM base ORDER BY {order} LIMIT ${} OFFSET ${})
         SELECT jsonb_build_object('items', COALESCE((SELECT jsonb_agg(j ORDER BY rn) FROM pg), '[]'::jsonb), 'total', (SELECT count(*) FROM base))",
        n + 1, n + 2);
    let mut all = args.to_vec();
    all.push(Arg::I(limit));
    all.push(Arg::I(offset));
    jval(db, &sql, &all).await
}

pub fn items_of(v: &Value) -> &[Value] { v.get("items").and_then(Value::as_array).map(|a| a.as_slice()).unwrap_or(&[]) }

// ------------------------------------------------------------------------------------------------------------------
// CSV export. Cells starting with = + - @ TAB CR are prefixed with a single quote (CSV/formula injection defence).
// ------------------------------------------------------------------------------------------------------------------
pub const CSV_MAX_ROWS: i64 = 50_000;

pub fn csv_cell(raw: &str) -> String {
    let mut v = raw.to_string();
    if matches!(v.chars().next(), Some('=') | Some('+') | Some('-') | Some('@') | Some('\t') | Some('\r')) { v.insert(0, '\''); }
    if v.contains(|c| matches!(c, ',' | '"' | '\n' | '\r')) { format!("\"{}\"", v.replace('"', "\"\"")) } else { v }
}
fn cell_of(v: &Value) -> String {
    match v { Value::Null => String::new(), Value::String(s) => s.clone(), Value::Bool(b) => b.to_string(), Value::Number(n) => n.to_string(), o => o.to_string() }
}
/// `cols` = (header, json key). Row values come from the JSON items of the normal list view.
pub fn csv_response(filename: &str, cols: &[(&str, &str)], items: &[Value]) -> Response {
    let mut out = String::with_capacity(items.len() * 64 + 256);
    out.push_str(&cols.iter().map(|(h, _)| csv_cell(h)).collect::<Vec<_>>().join(","));
    out.push_str("\r\n");
    for it in items {
        out.push_str(&cols.iter().map(|(_, k)| csv_cell(&cell_of(it.get(*k).unwrap_or(&Value::Null)))).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    let mut r = Response::new(axum::body::Body::from(out));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) { h.insert(header::CONTENT_DISPOSITION, v); }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

// ------------------------------------------------------------------------------------------------------------------
// Dry-run support: effects are read back from the ledger rows the document just wrote (by ref_type/ref_id).
// ------------------------------------------------------------------------------------------------------------------
pub async fn effects_by_ref(conn: &mut PgConnection, ref_type: &str, ref_id: Uuid) -> AppResult<Value> {
    jval(&mut *conn,
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('item', COALESCE(rm.name, p.name), 'item_type', l.item_type, 'item_id', l.item_id,
                'zone', l.zone, 'lot_id', l.lot_id, 'delta', l.qty_delta::text, 'after', l.balance_after::text, 'txn_type', l.txn_type) ORDER BY l.id), '[]'::jsonb)
         FROM stock_ledger l
         LEFT JOIN raw_materials rm ON l.item_type = 'RAW' AND rm.id = l.item_id
         LEFT JOIN products p ON l.item_type = 'PRODUCT' AND p.id = l.item_id
         WHERE l.ref_type = $1 AND l.ref_id = $2",
        &[Arg::T(Some(ref_type.to_string())), Arg::U(Some(ref_id))]).await
}

pub fn dry_body(effects: Value, warnings: &[String]) -> Value { json!({ "dry_run": true, "effects": effects, "warnings": warnings }) }

/// Roll the transaction back and answer a dry run.
pub async fn finish_dry(tx: sqlx::Transaction<'_, sqlx::Postgres>, ref_type: &str, ref_id: Uuid, warnings: &[String]) -> AppResult<(StatusCode, Json<Value>)> {
    let mut tx = tx;
    let effects = effects_by_ref(&mut tx, ref_type, ref_id).await?;
    tx.rollback().await?;
    Ok((StatusCode::OK, Json(dry_body(effects, warnings))))
}

/// Document number; a dry run never consumes a sequence value (unique preview string instead).
pub async fn doc_no(tx: &mut PgConnection, seq: &'static str, prefix: &str, dry: bool) -> AppResult<String> {
    if dry { Ok(format!("{prefix}-PREVIEW-{}", &Uuid::new_v4().simple().to_string()[..8])) } else { crate::util::next_no(tx, seq, prefix).await }
}

/// `CONFIRM_REQUIRED` unless `confirm` (or a dry run, which reports the same warnings instead of stopping).
pub fn need_confirm(warnings: &[String], confirm: bool, dry: bool, msg: &str) -> AppResult<()> {
    if warnings.is_empty() || confirm || dry { Ok(()) } else { Err(AppError::confirm(msg, warnings.to_vec())) }
}

pub fn pallets(crates: i64, cpp: Option<i64>) -> (Option<i64>, Option<i64>) {
    match cpp { Some(c) if c > 0 => (Some(crates / c), Some(crates % c)), _ => (None, None) }
}

pub fn created(v: Value) -> AppResult<(StatusCode, Json<Value>)> { Ok((StatusCode::CREATED, Json(v))) }
pub fn ok(v: Value) -> AppResult<(StatusCode, Json<Value>)> { Ok((StatusCode::OK, Json(v))) }
pub fn json_resp(v: Value) -> Response { Json(v).into_response() }

/// Decide whether `?format=csv` was requested.
pub fn is_csv(format: &Option<String>) -> bool { matches!(format.as_deref(), Some("csv")) }
