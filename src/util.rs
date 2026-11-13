use crate::error::{AppError, AppResult};
use rust_decimal::Decimal;
use sqlx::PgConnection;

/// Next human-readable document number such as `PO-261113-0007`. `seq` must be one of the sequences created by the migrations.
pub async fn next_no(conn: &mut PgConnection, seq: &'static str, prefix: &str) -> AppResult<String> {
    const OK: &[&str] = &["po_seq", "grn_seq", "blow_seq", "batch_seq", "trip_seq", "gatepass_seq", "qr_seq", "dn_seq", "wo_seq", "cnt_seq", "rtv_seq"];
    debug_assert!(OK.contains(&seq));
    if !OK.contains(&seq) { return Err(AppError::bad("bad sequence")); }
    let n: i64 = sqlx::query_scalar(&format!("SELECT nextval('{seq}')")).fetch_one(&mut *conn).await?;
    let d = plant_today(&mut *conn).await?.format("%y%m%d");
    Ok(format!("{prefix}-{d}-{n:04}"))
}

/// Round a Decimal up to a whole number (used for purchase-unit recommendations).
pub fn ceil(d: Decimal) -> Decimal { d.ceil() }

pub fn require_positive(field: &str, v: Decimal) -> AppResult<()> {
    if v <= Decimal::ZERO { Err(AppError::field(field, "must be greater than 0")) } else { Ok(()) }
}

/// "Today" in plant local time (`plant.utc_offset_minutes`). Always use this instead of `CURRENT_DATE` / `Utc::now().date_naive()`
/// for business dates (production day, mfg date, due dates, daily summaries).
pub async fn plant_today(conn: &mut PgConnection) -> AppResult<chrono::NaiveDate> {
    let off: Option<serde_json::Value> = sqlx::query_scalar("SELECT value FROM system_settings WHERE key='plant.utc_offset_minutes'")
        .fetch_optional(&mut *conn).await?;
    let mins = off.and_then(|v| v.as_i64()).unwrap_or(330);
    Ok((chrono::Utc::now() + chrono::Duration::minutes(mins)).date_naive())
}
