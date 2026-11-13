use crate::error::{AppError, AppResult};
use rust_decimal::Decimal;
use sqlx::PgConnection;

/// Next human-readable document number such as `PO-261113-0007`. `seq` must be one of the sequences created by the migrations.
pub async fn next_no(conn: &mut PgConnection, seq: &'static str, prefix: &str) -> AppResult<String> {
    const OK: &[&str] = &["po_seq", "grn_seq", "blow_seq", "batch_seq", "trip_seq", "gatepass_seq", "qr_seq"];
    debug_assert!(OK.contains(&seq));
    if !OK.contains(&seq) { return Err(AppError::bad("bad sequence")); }
    let n: i64 = sqlx::query_scalar(&format!("SELECT nextval('{seq}')")).fetch_one(&mut *conn).await?;
    let d = chrono::Utc::now().format("%y%m%d");
    Ok(format!("{prefix}-{d}-{n:04}"))
}

/// Round a Decimal up to a whole number (used for purchase-unit recommendations).
pub fn ceil(d: Decimal) -> Decimal { d.ceil() }

pub fn require_positive(field: &str, v: Decimal) -> AppResult<()> {
    if v <= Decimal::ZERO { Err(AppError::field(field, "must be greater than 0")) } else { Ok(()) }
}
