use crate::error::{AppError, AppResult};
use rust_decimal::Decimal;
use sqlx::PgConnection;

/// Next human-readable document number such as `PO-261113-0007`. `seq` must be one of the sequences created by the migrations.
pub async fn next_no(conn: &mut PgConnection, seq: &'static str, prefix: &str) -> AppResult<String> {
    const OK: &[&str] = &["po_seq", "grn_seq", "blow_seq", "batch_seq", "trip_seq", "gatepass_seq", "qr_seq", "dn_seq", "wo_seq", "cnt_seq", "rtv_seq", "tf_seq"];
    debug_assert!(OK.contains(&seq));
    if !OK.contains(&seq) { return Err(AppError::bad("bad sequence")); }
    let n: i64 = sqlx::query_scalar(&format!("SELECT nextval('{seq}')")).fetch_one(&mut *conn).await?;
    let d = plant_today(&mut *conn).await?.format("%y%m%d");
    Ok(format!("{prefix}-{d}-{n:04}"))
}

/// THE definition of "dry run": the `dry_run` query value is present and not one of `0`, `false`, `no`, `off` (any case).
/// An unrecognised value (`yes`, `True`, `2` ...) therefore means DRY RUN - fail safe: a review call must never commit.
/// Every handler family and the idempotency middleware use this one predicate (they used to disagree: `?dry_run=yes` committed).
pub fn dry_value(v: Option<&str>) -> bool {
    match v {
        None => false,
        Some(s) => !matches!(s.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"),
    }
}

/// Percent-decode a query component (`+` is a space). Invalid escapes are kept literally.
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'+' { out.push(b' '); i += 1; continue; }
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|x| u8::from_str_radix(x, 16).ok()) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]); i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True when ANY `dry_run` pair of the (percent-decoded) query string is a dry run per `dry_value`.
pub fn dry_run_in_query(q: Option<&str>) -> bool {
    q.map_or(false, |q| q.split('&').any(|p| {
        let (k, v) = p.split_once('=').map_or((p, None), |(k, v)| (k, Some(v)));
        pct_decode(k) == "dry_run" && dry_value(Some(&v.map(pct_decode).unwrap_or_default()))
    }))
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

/// Resolve the godown a request acts on. `requested` = explicit `location_id` from the client.
/// Omitted => the user's only allowed godown if they are restricted to exactly one, else MAIN.
/// Always validates: exists, active, not the system TRANSIT location, and the user may use it.
/// Takes `FOR SHARE` on the location row so that, inside the caller's transaction, a concurrent admin deactivation
/// (which takes `FOR UPDATE` and then checks for stock) cannot slip in between the check and the stock post.
pub async fn resolve_location(conn: &mut PgConnection, user: &crate::auth::AuthUser, requested: Option<uuid::Uuid>) -> AppResult<uuid::Uuid> {
    let id = match requested {
        Some(id) => id,
        // restricted user: their only godown; or MAIN if they may use it; otherwise their first allowed godown
        None if !user.is_admin && !user.allowed_location_ids.is_empty() => {
            if user.allowed_location_ids.contains(&crate::stock::MAIN_LOCATION) { crate::stock::MAIN_LOCATION } else { user.allowed_location_ids[0] }
        }
        None => crate::stock::MAIN_LOCATION,
    };
    let ok: Option<(String, bool)> = sqlx::query_as("SELECT kind, is_active FROM locations WHERE id = $1 FOR SHARE").bind(id).fetch_optional(&mut *conn).await?;
    match ok {
        Some((kind, true)) if kind != "TRANSIT" => {}
        Some(_) => return Err(AppError::field("location_id", "this godown is inactive or not selectable")),
        None => return Err(AppError::field("location_id", "unknown godown")),
    }
    user.require_location(id)?;
    Ok(id)
}
