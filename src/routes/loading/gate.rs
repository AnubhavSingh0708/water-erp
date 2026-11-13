//! Gate pass (atomic dispatch of a trip), printable gate pass, gate-pass void.
use super::common::*;
use super::trips::id1;
use crate::{
    alerts::{self, NewAlert},
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    state::AppState,
    util, validate,
};
use axum::{extract::State, http::StatusCode, response::Response, Json};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoBody {}

/// `POST /trips/{id}/gate-pass`
pub async fn issue(State(st): State<AppState>, user: AuthUser, ids: Ids, Q(dq): Q<DryQ>, J(_b): J<NoBody>) -> AppResult<Response> {
    let dry = dq.on();
    let mut tx = st.db.begin().await?;
    let mut fx = Fx::default();
    let out = issue_core(&mut tx, &st, &user, id1(&ids)?, dry, &mut fx).await?;
    conclude(tx, dry, fx, StatusCode::OK, out).await
}

async fn issue_core(tx: &mut sqlx::PgConnection, st: &AppState, user: &AuthUser, trip_id: Uuid, dry: bool, fx: &mut Fx) -> AppResult<Value> {
    // trip first; the state is read AFTER the lock so a concurrent double submit sees DISPATCHED and fails with INVALID_STATE
    let trip = lock_trip(tx, trip_id).await?;
    if trip.status != "LOADING" {
        return Err(invalid_state(format!("Trip {} is {} - a gate pass can only be issued for a trip that is LOADING", trip.trip_no, trip.status), &trip.status));
    }
    let items = trip_items(tx, trip_id).await?;
    if items.is_empty() { return Err(AppError::bad("Add at least one item to the trip before issuing the gate pass")); }

    // vehicle documents
    let today = util::plant_today(tx).await?;
    let v: (String, bool, Option<chrono::NaiveDate>, Option<chrono::NaiveDate>, Option<chrono::NaiveDate>, Option<chrono::NaiveDate>) = sqlx::query_as(
        "SELECT reg_no, is_active, insurance_expiry, fitness_expiry, puc_expiry, permit_expiry FROM vehicles WHERE id = $1 FOR SHARE")
        .bind(trip.vehicle_id).fetch_one(&mut *tx).await?;
    if !v.1 { return Err(AppError::bad(format!("Vehicle {} is inactive", v.0))); }
    let mut expired = vec![];
    for (doc, d) in [("INSURANCE", v.2), ("FITNESS", v.3), ("PUC", v.4), ("PERMIT", v.5)] {
        if let Some(d) = d { if d < today { expired.push(json!({ "doc": doc, "expiry_date": d })); } }
    }
    if !expired.is_empty() {
        return Err(AppError::conflict_with("VEHICLE_DOCS_EXPIRED",
            format!("Vehicle {} has expired documents: {}", v.0, expired.iter().filter_map(|e| e["doc"].as_str()).collect::<Vec<_>>().join(", ")),
            json!({ "vehicle": v.0, "expired": expired })));
    }

    // lock the batches (canonical order), re-check the QC rule inside the transaction
    let mut batch_ids: Vec<Uuid> = items.iter().map(|i| i.batch_id).collect();
    batch_ids.sort(); batch_ids.dedup();
    let batches = lock_batches(tx, &batch_ids).await?;
    let strict = st.settings.get().strict_qc_lock;
    let mut per_batch: BTreeMap<Uuid, (usize, &'static str, i64)> = BTreeMap::new(); // batch -> (index in `batches`, zone, crates)
    for (idx, b) in batches.iter().enumerate() {
        let zone = load_zone(b, strict)?;
        per_batch.insert(b.id, (idx, zone, 0));
    }
    for it in &items {
        let e = per_batch.get_mut(&it.batch_id).ok_or_else(|| AppError::not_found("Batch"))?;
        e.2 += it.crates as i64;
    }
    // QR sets (optional feature): must still be RESERVED by this trip
    for it in &items {
        if let Some(sid) = it.qr_set_id { crate::routes::qr::on_gate_pass(tx, sid, trip_id).await?; }
    }

    // stock: every item deducts ITS OWN lot (no FEFO re-allocation)
    let mut moves = vec![];
    let mut labels = Labels::new();
    for (bid, (idx, zone, crates)) in &per_batch {
        let b = &batches[*idx];
        let on_hand: rust_decimal::Decimal = sqlx::query_scalar(
            "SELECT COALESCE(SUM(qty),0) FROM stock_balances WHERE item_type='PRODUCT' AND item_id=$1 AND zone=$2 AND lot_id=$3")
            .bind(b.product_id).bind(*zone).bind(bid).fetch_one(&mut *tx).await?;
        if on_hand < rust_decimal::Decimal::from(*crates) {
            return Err(AppError::conflict_with("INSUFFICIENT_STOCK",
                format!("Batch {} has only {} crates on hand in {zone} but this trip carries {crates}", b.batch_no, dec_str(on_hand)),
                json!({ "available": dec_str(on_hand), "required": crates, "batch_id": bid, "batch_no": b.batch_no, "zone": zone })));
        }
        labels.insert((b.product_id, b.id), format!("{} · {}", b.product_name, b.batch_no));
        moves.push(product_move(b.product_id, zone, b.id, -crates, "GATE_PASS").reference("TRIP", trip_id).by(Some(user.id)).note(format!("gate pass for trip {}", trip.trip_no)));
    }
    post_moves(tx, moves, &labels, fx).await?;
    // the number is taken only after every check and stock move succeeded, so failed attempts never leave gaps in the GP series
    let gp_no = doc_no(tx, dry, "gatepass_seq", "GP").await?;

    sqlx::query("UPDATE loading_trips SET status = 'DISPATCHED', gate_pass_no = $2, gate_pass_issued_at = now(), issued_by = $3 WHERE id = $1")
        .bind(trip_id).bind(&gp_no).bind(user.id).execute(&mut *tx).await?;
    let loaded: i64 = items.iter().map(|i| i.crates as i64).sum();
    if let Some(w) = capacity_warning(loaded, trip.capacity_crates) { fx.warn(w); }
    for (bid, (idx, _, _)) in &per_batch {
        let b = &batches[*idx];
        if b.qc_status != "APPROVED" { fx.warn(format!("Batch {} leaves the plant before QC approval", b.batch_no)); }
        let _ = bid;
    }
    let out = trip_json(tx, trip_id, true).await?;

    alerts::notify_once(&mut *tx, NewAlert {
        event_type: "GATE_PASS_ISSUED", dedupe_key: &trip_id.to_string(), severity: "INFO",
        title: &format!("Gate pass {gp_no} issued"),
        body: &format!("Trip {} on {} left with {loaded} crates", trip.trip_no, v.0),
        url: "/loading.html", payload: json!({ "trip_id": trip_id, "gate_pass_no": gp_no, "crates": loaded }),
    }).await?;
    audit::log(&mut *tx, Some(user), "GATE_PASS_ISSUED", "loading_trip", Some(trip_id.to_string()),
        json!({ "trip_no": trip.trip_no, "gate_pass_no": gp_no, "vehicle": v.0, "crates": loaded })).await;
    Ok(out)
}

/// `GET /trips/{id}/gate-pass` - printable document (DISPATCHED / SETTLED only).
pub async fn print(State(st): State<AppState>, ids: Ids) -> AppResult<Json<Value>> {
    let id = id1(&ids)?;
    let mut conn = st.db.acquire().await?;
    let row: Option<(String, Option<String>, Option<chrono::DateTime<Utc>>, Option<String>, String, String, String, String, String, Option<String>, String, Option<String>, String)> =
        sqlx::query_as(
            "SELECT t.trip_no, t.gate_pass_no, t.gate_pass_issued_at, u.full_name, t.status, v.reg_no, v.name, v.driver_phone, t.driver_name,
                    r.name, t.notes, NULL::text, v.driver_name
             FROM loading_trips t JOIN vehicles v ON v.id = t.vehicle_id LEFT JOIN routes r ON r.id = t.route_id LEFT JOIN users u ON u.id = t.issued_by
             WHERE t.id = $1")
            .bind(id).fetch_optional(&mut *conn).await?;
    let (trip_no, gp_no, issued_at, issued_by, status, reg, vname, vphone, driver, route, notes, _, vdriver) = row.ok_or_else(|| AppError::not_found("Trip"))?;
    if status != "DISPATCHED" && status != "SETTLED" {
        return Err(invalid_state(format!("Trip {trip_no} is {status}: there is no valid gate pass"), &status));
    }
    let items = trip_items(&mut conn, id).await?;
    let crates: i64 = items.iter().map(|i| i.crates as i64).sum();
    let bottles: i64 = items.iter().map(|i| i.crates as i64 * i.units_per_crate as i64).sum();
    let pallets: i64 = items.iter().map(|i| i.pallets.unwrap_or(0) as i64).sum();
    let loose: i64 = items.iter().map(|i| i.loose_crates.unwrap_or(i.crates) as i64).sum();
    Ok(Json(json!({
        "gate_pass_no": gp_no, "plant_name": st.settings.get().plant_name, "trip_no": trip_no, "status": status,
        "issued_at": issued_at, "issued_by_name": issued_by,
        "vehicle": { "reg_no": reg, "name": vname, "driver_name": if driver.is_empty() { vdriver } else { driver.clone() }, "driver_phone": vphone },
        "route": route, "driver_name": driver, "items": items,
        "totals": { "crates": crates, "bottles": bottles, "pallets": pallets, "loose_crates": loose },
        "notes": notes,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoidBody {
    reason: String,
    #[serde(default)]
    confirm: bool,
}

/// `POST /trips/{id}/void {reason, confirm:true}` - wrong load / vehicle never left. Only DISPATCHED (not settled), within the
/// void window (Admin unlimited). Restocks every item's lot, trip -> CANCELLED.
pub async fn void(State(st): State<AppState>, user: AuthUser, ids: Ids, Q(dq): Q<DryQ>, J(b): J<VoidBody>) -> AppResult<Response> {
    let reason = validate::required_text("reason", &b.reason, 300)?;
    if reason.chars().count() < 5 { return Err(AppError::field("reason", "must be at least 5 characters")); }
    let dry = dq.on();
    let trip_id = id1(&ids)?;
    let mut tx = st.db.begin().await?;
    let mut fx = Fx::default();
    let trip = lock_trip(&mut tx, trip_id).await?;
    if trip.status != "DISPATCHED" {
        let hint = if trip.status == "SETTLED" { " Settled trips cannot be voided - correct the stock with an adjustment." } else { "" };
        return Err(invalid_state(format!("Trip {} is {} - only a DISPATCHED (not yet settled) trip can be voided.{hint}", trip.trip_no, trip.status), &trip.status));
    }
    if !user.is_admin {
        let window = st.settings.get().void_window_hours;
        let issued = trip.gate_pass_issued_at.unwrap_or_else(Utc::now);
        if Utc::now() - issued > Duration::hours(window) {
            return Err(AppError::forbidden(format!("The {window}-hour void window for this gate pass has passed. Ask an Administrator.")));
        }
    }
    let items = trip_items(&mut tx, trip_id).await?;
    let loaded: i64 = items.iter().map(|i| i.crates as i64).sum();
    if !b.confirm {
        return Err(AppError::confirm(format!("Voiding gate pass {} cancels trip {} and returns {loaded} crates to stock", trip.gate_pass_no.clone().unwrap_or_default(), trip.trip_no),
            vec![format!("{loaded} crates will be put back into stock and the trip will be CANCELLED."), "This cannot be undone.".into()]));
    }
    let mut batch_ids: Vec<Uuid> = items.iter().map(|i| i.batch_id).collect();
    batch_ids.sort(); batch_ids.dedup();
    let batches = lock_batches(&mut tx, &batch_ids).await?;
    let mut sums: BTreeMap<Uuid, i64> = BTreeMap::new();
    for it in &items { *sums.entry(it.batch_id).or_default() += it.crates as i64; }
    let mut moves = vec![];
    let mut labels = Labels::new();
    for bt in &batches {
        let n = sums[&bt.id];
        labels.insert((bt.product_id, bt.id), format!("{} · {}", bt.product_name, bt.batch_no));
        moves.push(product_move(bt.product_id, restock_zone(&bt.qc_status), bt.id, n, "GATE_PASS_VOID")
            .reference("TRIP", trip_id).by(Some(user.id)).expiry(Some(bt.expiry_date)).note(format!("void: {reason}")));
    }
    post_moves(&mut tx, moves, &labels, &mut fx).await?;
    for it in &items {
        if let Some(sid) = it.qr_set_id { crate::routes::qr::on_void(&mut tx, sid, trip_id).await?; }
    }
    sqlx::query("UPDATE loading_trips SET status = 'CANCELLED', void_reason = $2, voided_by = $3, voided_at = now() WHERE id = $1")
        .bind(trip_id).bind(&reason).bind(user.id).execute(&mut *tx).await?;
    let out = trip_json(&mut tx, trip_id, true).await?;
    audit::log(&mut *tx, Some(&user), "GATE_PASS_VOID", "loading_trip", Some(trip_id.to_string()),
        json!({ "trip_no": trip.trip_no, "gate_pass_no": trip.gate_pass_no, "reason": reason, "crates": loaded })).await;
    conclude(tx, dry, fx, StatusCode::OK, out).await
}
