//! OPTIONAL QR crate-set labels (`features.qr_labels`, default off; the router answers `FEATURE_DISABLED` while off).
//! Also hosts the set state-machine hooks that the loading flows call for trip items with a `qr_set_id`.
use crate::{
    error::{AppError, AppResult},
    routes::loading::common::*,
    state::AppState,
};
use crate::auth::AuthUser;
use axum::Router;
use serde_json::{json, Value};
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

pub fn router() -> Router<AppState> { Router::new() }

/// Status a free set takes, following its batch's QC state.
pub fn free_status(qc_status: &str) -> &'static str {
    match qc_status { "APPROVED" => "AVAILABLE", "QUARANTINE" => "QUARANTINE", _ => "REJECTED" }
}

#[derive(FromRow, Debug)]
struct SetLock { status: String, trip_id: Option<Uuid>, batch_id: Uuid }

/// Lock a set. Lock order: batch (shared) first, then the set, to match QC decisions (batch -> sets).
async fn lock_set(tx: &mut PgConnection, set_id: Uuid) -> AppResult<(SetLock, String)> {
    let batch: Uuid = sqlx::query_scalar("SELECT batch_id FROM qr_crate_sets WHERE id = $1").bind(set_id).fetch_optional(&mut *tx).await?
        .ok_or_else(|| AppError::not_found("QR set"))?;
    let qc: String = sqlx::query_scalar("SELECT qc_status FROM production_batches WHERE id = $1 FOR SHARE").bind(batch).fetch_one(&mut *tx).await?;
    let s = sqlx::query_as::<_, SetLock>("SELECT status, trip_id, batch_id FROM qr_crate_sets WHERE id = $1 FOR UPDATE")
        .bind(set_id).fetch_one(&mut *tx).await?;
    Ok((s, qc))
}

/// Trip item removed / trip cancelled: `RESERVED -> AVAILABLE|QUARANTINE` (whatever the batch's QC state implies).
pub async fn release_set(tx: &mut PgConnection, set_id: Uuid) -> AppResult<()> {
    let (s, qc) = lock_set(tx, set_id).await?;
    if s.status == "RESERVED" {
        sqlx::query("UPDATE qr_crate_sets SET status = $2, trip_id = NULL WHERE id = $1").bind(set_id).bind(free_status(&qc)).execute(&mut *tx).await?;
    }
    Ok(())
}

/// Gate pass: the set must still be RESERVED by this very trip, then `RESERVED -> LOADED`.
pub async fn on_gate_pass(tx: &mut PgConnection, set_id: Uuid, trip_id: Uuid) -> AppResult<()> {
    let (s, _) = lock_set(tx, set_id).await?;
    if s.status != "RESERVED" || s.trip_id != Some(trip_id) {
        return Err(AppError::conflict_with("INVALID_STATE", "A QR set of this trip is no longer reserved for it - remove it and add it again",
            json!({ "set_id": set_id, "status": s.status })));
    }
    sqlx::query("UPDATE qr_crate_sets SET status = 'LOADED', loaded_at = now() WHERE id = $1").bind(set_id).execute(&mut *tx).await?;
    Ok(())
}

/// Gate-pass void: `LOADED -> AVAILABLE|QUARANTINE`.
pub async fn on_void(tx: &mut PgConnection, set_id: Uuid, trip_id: Uuid) -> AppResult<()> {
    let (s, qc) = lock_set(tx, set_id).await?;
    if s.status == "LOADED" && s.trip_id == Some(trip_id) {
        sqlx::query("UPDATE qr_crate_sets SET status = $2, trip_id = NULL, loaded_at = NULL WHERE id = $1").bind(set_id).bind(free_status(&qc)).execute(&mut *tx).await?;
    }
    Ok(())
}

/// Settlement: nothing returned -> `DELIVERED`; otherwise `AVAILABLE` with `crates = returned`.
pub async fn on_settle(tx: &mut PgConnection, set_id: Uuid, trip_id: Uuid, returned: i64) -> AppResult<()> {
    let (s, qc) = lock_set(tx, set_id).await?;
    if s.status != "LOADED" || s.trip_id != Some(trip_id) {
        return Err(AppError::conflict_with("INVALID_STATE", "A QR set of this trip is not in LOADED state", json!({ "set_id": set_id, "status": s.status })));
    }
    if returned == 0 {
        sqlx::query("UPDATE qr_crate_sets SET status = 'DELIVERED' WHERE id = $1").bind(set_id).execute(&mut *tx).await?;
    } else {
        sqlx::query("UPDATE qr_crate_sets SET status = $2, crates = $3, trip_id = NULL WHERE id = $1")
            .bind(set_id).bind(free_status(&qc)).bind(returned as i32).execute(&mut *tx).await?;
    }
    Ok(())
}

pub async fn add_set_to_trip(_tx: &mut PgConnection, _st: &AppState, _user: &AuthUser, _trip: &TripLock, _code: &str, _crates: Option<i64>, _fx: &mut Fx) -> AppResult<Value> {
    Err(AppError::bad("QR loading is not available yet"))
}
