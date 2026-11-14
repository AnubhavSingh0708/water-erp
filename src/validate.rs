//! Shared input validation + pagination helpers. Use these everywhere so limits are uniform.
use crate::error::{AppError, AppResult};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

pub const MAX_QTY: i64 = 1_000_000_000;
pub const MAX_LINES: usize = 200;

#[derive(Debug, Default, Deserialize)]
pub struct PageQuery { pub limit: Option<i64>, pub offset: Option<i64> }
impl PageQuery {
    pub fn limit(&self) -> i64 { self.limit.unwrap_or(50).clamp(1, 200) }
    pub fn offset(&self) -> i64 { self.offset.unwrap_or(0).max(0) }
}

#[derive(Debug, Serialize)]
pub struct Paged<T> { pub items: Vec<T>, pub total: i64 }

/// 0 < qty <= 1e9 and at most 4 decimal places.
pub fn qty_positive(field: &str, d: Decimal) -> AppResult<()> {
    if d <= Decimal::ZERO { return Err(AppError::field(field, "must be greater than 0")); }
    qty_bounds(field, d)
}
/// Non-zero signed quantity (adjustments).
pub fn qty_nonzero(field: &str, d: Decimal) -> AppResult<()> {
    if d.is_zero() { return Err(AppError::field(field, "must not be 0")); }
    qty_bounds(field, d.abs())
}
pub fn qty_non_negative(field: &str, d: Decimal) -> AppResult<()> {
    if d < Decimal::ZERO { return Err(AppError::field(field, "must not be negative")); }
    qty_bounds(field, d)
}
fn qty_bounds(field: &str, d: Decimal) -> AppResult<()> {
    if d > Decimal::from(MAX_QTY) { return Err(AppError::field(field, "is too large")); }
    if d.scale() > 4 { return Err(AppError::field(field, "has too many decimal places (max 4)")); }
    Ok(())
}
/// Whole-number count 1..=1e6 (crates, jars).
pub fn count(field: &str, n: i64) -> AppResult<()> {
    if n < 1 || n > 1_000_000 { Err(AppError::field(field, "must be between 1 and 1,000,000")) } else { Ok(()) }
}
/// Trim and bound free text; rejects control characters other than newline/tab.
pub fn text(field: &str, s: &str, max: usize) -> AppResult<String> {
    let t = s.trim();
    if t.chars().count() > max { return Err(AppError::field(field, &format!("must be at most {max} characters"))); }
    // control characters, plus invisible bidi overrides / BOM ("trojan source" display spoofing of names in lists and printouts).
    // ZWJ / ZWNJ (U+200C/D) stay allowed: Indic scripts need them.
    let spoof = |c: char| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}');
    if t.chars().any(|c| (c.is_control() && c != '\n' && c != '\t') || spoof(c)) { return Err(AppError::field(field, "contains invalid characters")); }
    Ok(t.to_string())
}
pub fn required_text(field: &str, s: &str, max: usize) -> AppResult<String> {
    let t = text(field, s, max)?;
    if t.is_empty() { Err(AppError::field(field, "is required")) } else { Ok(t) }
}
pub fn max_items(field: &str, len: usize, max: usize) -> AppResult<()> {
    if len == 0 { return Err(AppError::field(field, "at least one entry is required")); }
    if len > max { return Err(AppError::field(field, &format!("at most {max} entries allowed"))); }
    Ok(())
}
/// Escape `%`, `_` and `\` for use inside an ILIKE pattern (always bind the pattern, never format it into SQL).
pub fn like_pattern(q: &str) -> String {
    let esc: String = q.trim().chars().flat_map(|c| match c { '%' | '_' | '\\' => vec!['\\', c], _ => vec![c] }).collect();
    format!("%{esc}%")
}
