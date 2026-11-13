//! Availability, trips (create/list/detail/cancel), trip items (bulk by product/batch, or by QR set), loading summary.
use super::common::*;
use crate::{
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    state::AppState,
    stock, util, validate,
};
use axum::{extract::State, http::StatusCode, response::{IntoResponse, Response}, Json};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use std::collections::HashMap;
use uuid::Uuid;

pub fn id1(ids: &Ids) -> AppResult<Uuid> { ids.0.first().copied().ok_or_else(|| AppError::bad("Missing id in path")) }

// ------------------------------------------------------------------------------------------------ availability

#[derive(Deserialize, Default)]
pub struct AvailQ { product_id: Option<Uuid>, trip_id: Option<Uuid>, location_id: Option<Uuid> }

#[derive(FromRow)]
struct AvailRow {
    product_id: Uuid, product_name: String, kind: String, units_per_crate: i32, crates_per_pallet: Option<i32>,
    batch_id: Uuid, batch_no: String, expiry_date: NaiveDate, qc_status: String, zone: String,
    on_hand: Decimal, reserved: Decimal, in_trip: Decimal,
}

/// `GET /availability?product_id=&trip_id=` - FEFO ordered, reservation-aware stock that can be loaded right now.
pub async fn availability(State(st): State<AppState>, user: AuthUser, Q(q): Q<AvailQ>) -> AppResult<Json<Value>> {
    let strict = st.settings.get().strict_qc_lock;
    let mut conn = st.db.acquire().await?;
    // godown: explicit, else the godown of the trip being loaded, else the user's default
    let requested = match (q.location_id, q.trip_id) {
        (Some(l), _) => Some(l),
        (None, Some(t)) => sqlx::query_scalar("SELECT location_id FROM loading_trips WHERE id = $1").bind(t).fetch_optional(&mut *conn).await?,
        _ => None,
    };
    let loc = util::resolve_location(&mut conn, &user, requested).await?;
    let loc_name = location_name(&mut conn, loc).await?;
    let rows = sqlx::query_as::<_, AvailRow>(
        "SELECT b.product_id, p.name AS product_name, p.kind, p.units_per_crate, p.crates_per_pallet, b.id AS batch_id, b.batch_no, b.expiry_date, b.qc_status,
                sb.zone, sb.qty AS on_hand,
                COALESCE((SELECT SUM(ti.crates) FROM trip_items ti JOIN loading_trips t ON t.id = ti.trip_id
                          WHERE ti.batch_id = b.id AND t.status = 'LOADING' AND t.location_id = $4), 0)::numeric AS reserved,
                COALESCE((SELECT SUM(ti.crates) FROM trip_items ti WHERE ti.batch_id = b.id AND ti.trip_id = $3), 0)::numeric AS in_trip
         FROM production_batches b
         JOIN products p ON p.id = b.product_id
         JOIN stock_balances sb ON sb.item_type = 'PRODUCT' AND sb.item_id = b.product_id AND sb.lot_id = b.id
              AND sb.location_id = $4 AND sb.zone = CASE b.qc_status WHEN 'APPROVED' THEN 'FG_READY' ELSE 'QC_QUARANTINE' END
         WHERE sb.qty > 0 AND (b.qc_status = 'APPROVED' OR ($2 = false AND b.qc_status = 'QUARANTINE'))
           AND ($1::uuid IS NULL OR b.product_id = $1)
         ORDER BY b.expiry_date ASC, b.created_at ASC, b.id ASC LIMIT 500")
        .bind(q.product_id).bind(strict).bind(q.trip_id).bind(loc)
        .fetch_all(&mut *conn).await?;
    let out: Vec<Value> = rows.into_iter().map(|r| {
        let avail = floor_i64((r.on_hand - r.reserved).max(Decimal::ZERO));
        let cpp = r.crates_per_pallet.map(|c| c as i64);
        json!({
            "product_id": r.product_id, "product_name": r.product_name, "kind": r.kind, "units_per_crate": r.units_per_crate,
            "crates_per_pallet": r.crates_per_pallet,
            "batch_id": r.batch_id, "batch_no": r.batch_no, "expiry_date": r.expiry_date, "qc_status": r.qc_status, "zone": r.zone,
            "fg_ready": floor_i64(r.on_hand), "reserved": floor_i64(r.reserved), "available": avail,
            "in_trip": floor_i64(r.in_trip), "location_id": loc, "location_name": loc_name,
            "available_pallets": cpp.map(|c| avail / c), "available_loose_crates": cpp.map(|c| avail % c),
        })
    }).collect();
    Ok(Json(Value::Array(out)))
}

// ------------------------------------------------------------------------------------------------ summary

#[derive(Deserialize, Default)]
pub struct SummaryQ { location_id: Option<Uuid> }

pub async fn summary(State(st): State<AppState>, user: AuthUser, Q(q): Q<SummaryQ>) -> AppResult<Json<Value>> {
    let scope: Option<Vec<Uuid>> = match q.location_id {
        Some(l) => { user.require_location(l)?; Some(vec![l]) }
        None => location_scope(&user),
    };
    let mut conn = st.db.acquire().await?;
    let today = util::plant_today(&mut conn).await?;
    let off = st.settings.get().utc_offset_minutes as i32;
    let r: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT COUNT(*) FROM loading_trips WHERE status = 'LOADING' AND ($3::uuid[] IS NULL OR location_id = ANY($3))),
           (SELECT COUNT(*) FROM loading_trips WHERE status = 'DISPATCHED' AND ($3::uuid[] IS NULL OR location_id = ANY($3))),
           (SELECT COUNT(*) FROM loading_trips WHERE status = 'SETTLED' AND ($3::uuid[] IS NULL OR location_id = ANY($3)) AND ((settled_at AT TIME ZONE 'UTC') + make_interval(mins => $2))::date = $1),
           (SELECT COALESCE(SUM(ti.crates), 0)::bigint FROM trip_items ti JOIN loading_trips t ON t.id = ti.trip_id
             WHERE t.status IN ('DISPATCHED','SETTLED') AND ($3::uuid[] IS NULL OR t.location_id = ANY($3)) AND ((t.gate_pass_issued_at AT TIME ZONE 'UTC') + make_interval(mins => $2))::date = $1),
           (SELECT COUNT(*) FROM dispatch_notes WHERE status = 'ISSUED' AND ($3::uuid[] IS NULL OR location_id = ANY($3)) AND ((created_at AT TIME ZONE 'UTC') + make_interval(mins => $2))::date = $1),
           (SELECT COALESCE(SUM(l.crates), 0)::bigint FROM dispatch_lines l JOIN dispatch_notes d ON d.id = l.dn_id
             WHERE d.status = 'ISSUED' AND ($3::uuid[] IS NULL OR d.location_id = ANY($3)) AND ((d.created_at AT TIME ZONE 'UTC') + make_interval(mins => $2))::date = $1)")
        .bind(today).bind(off).bind(&scope).fetch_one(&mut *conn).await?;
    Ok(Json(json!({
        "open_trips": r.0, "on_road": r.1, "settled_today": r.2, "crates_dispatched_today": r.3,
        "dispatch_notes_today": r.4, "crates_direct_today": r.5,
    })))
}

// ------------------------------------------------------------------------------------------------ trips

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewTrip {
    vehicle_id: Uuid,
    route_id: Option<Uuid>,
    driver_name: Option<String>,
    notes: Option<String>,
    /// source godown (default: the caller's only godown, else MAIN)
    location_id: Option<Uuid>,
    #[serde(default)]
    confirm: bool,
}

pub async fn create_trip(State(st): State<AppState>, user: AuthUser, J(b): J<NewTrip>) -> AppResult<Response> {
    let driver = match &b.driver_name { Some(d) => Some(validate::text("driver_name", d, 120)?), None => None };
    let notes = validate::text("notes", b.notes.as_deref().unwrap_or(""), 500)?;
    let mut tx = st.db.begin().await?;
    let loc = util::resolve_location(&mut tx, &user, b.location_id).await?;
    // the vehicle row serialises concurrent trip creation for the same vehicle
    let v: Option<(String, i32, String, bool)> = sqlx::query_as("SELECT reg_no, capacity_crates, driver_name, is_active FROM vehicles WHERE id = $1 FOR UPDATE")
        .bind(b.vehicle_id).fetch_optional(&mut *tx).await?;
    let (reg, cap, vdriver, active) = v.ok_or_else(|| AppError::field("vehicle_id", "unknown vehicle"))?;
    if !active { return Err(AppError::field("vehicle_id", "this vehicle is inactive")); }
    if let Some(r) = b.route_id {
        let ok: Option<bool> = sqlx::query_scalar("SELECT is_active FROM routes WHERE id = $1").bind(r).fetch_optional(&mut *tx).await?;
        if ok != Some(true) { return Err(AppError::field("route_id", "unknown or inactive route")); }
    }
    let open: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, trip_no, status FROM loading_trips WHERE vehicle_id = $1 AND status IN ('LOADING','DISPATCHED') ORDER BY (status = 'LOADING') DESC, created_at DESC LIMIT 1")
        .bind(b.vehicle_id).fetch_optional(&mut *tx).await?;
    if let Some((tid, tno, status)) = open {
        if status == "LOADING" {
            return Err(AppError::conflict_with("DUPLICATE", format!("Vehicle {reg} already has an open loading trip ({tno}). Finish or cancel it first."),
                json!({ "trip_id": tid, "trip_no": tno })));
        }
        if !b.confirm {
            return Err(AppError::confirm(format!("Vehicle {reg} is still on trip {tno} (not settled yet)"),
                vec![format!("Vehicle {reg} has not been settled for trip {tno}. Settle it first unless this is a different truck.")]));
        }
    }
    let trip_no = util::next_no(&mut tx, "trip_seq", "TR").await?;
    let driver = driver.filter(|d| !d.is_empty()).unwrap_or(vdriver);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO loading_trips (trip_no, vehicle_id, route_id, driver_name, capacity_crates, notes, created_by, location_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id")
        .bind(&trip_no).bind(b.vehicle_id).bind(b.route_id).bind(driver).bind(cap).bind(notes).bind(user.id).bind(loc)
        .fetch_one(&mut *tx).await?;
    let out = trip_json(&mut tx, id, true).await?;
    audit::log(&mut *tx, Some(&user), "TRIP_CREATE", "loading_trip", Some(id.to_string()), json!({ "trip_no": trip_no, "vehicle": reg, "location_id": loc })).await;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

#[derive(Deserialize, Default)]
pub struct TripListQ { status: Option<String>, q: Option<String>, vehicle_id: Option<Uuid>, location_id: Option<Uuid>, limit: Option<i64>, offset: Option<i64> }

pub async fn list_trips(State(st): State<AppState>, user: AuthUser, Q(q): Q<TripListQ>) -> AppResult<Json<Value>> {
    let scope: Option<Vec<Uuid>> = match q.location_id {
        Some(l) => { user.require_location(l)?; Some(vec![l]) }
        None => location_scope(&user),
    };
    let status = match q.status.as_deref().map(str::trim) {
        None | Some("") | Some("ALL") => None,
        Some(s) if ["LOADING", "DISPATCHED", "SETTLED", "CANCELLED"].contains(&s) => Some(s.to_string()),
        Some(_) => return Err(AppError::field("status", "must be LOADING, DISPATCHED, SETTLED, CANCELLED or ALL")),
    };
    let pat = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => { validate::text("q", s, 100)?; Some(validate::like_pattern(s)) }
        None => None,
    };
    let page = validate::PageQuery { limit: q.limit, offset: q.offset };
    let filter = "WHERE ($1::text IS NULL OR t.status = $1)
        AND ($2::text IS NULL OR t.trip_no ILIKE $2 OR v.reg_no ILIKE $2 OR t.driver_name ILIKE $2 OR COALESCE(t.gate_pass_no,'') ILIKE $2)
        AND ($3::uuid IS NULL OR t.vehicle_id = $3)
        AND ($6::uuid[] IS NULL OR t.location_id = ANY($6))";
    let rows = sqlx::query_as::<_, TripRow>(&format!("{TRIP_SELECT} {filter} ORDER BY t.created_at DESC, t.id ASC LIMIT $4 OFFSET $5"))
        .bind(&status).bind(&pat).bind(q.vehicle_id).bind(page.limit()).bind(page.offset()).bind(&scope).fetch_all(&st.db).await?;
    let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM loading_trips t JOIN vehicles v ON v.id = t.vehicle_id {filter}"))
        .bind(&status).bind(&pat).bind(q.vehicle_id).bind(0i64).bind(0i64).bind(&scope).fetch_one(&st.db).await?;
    Ok(Json(json!({ "items": rows.iter().map(trip_value).collect::<Vec<_>>(), "total": total })))
}

pub async fn get_trip(State(st): State<AppState>, user: AuthUser, ids: Ids) -> AppResult<Json<Value>> {
    let mut conn = st.db.acquire().await?;
    let id = id1(&ids)?;
    let loc: Uuid = sqlx::query_scalar("SELECT location_id FROM loading_trips WHERE id = $1").bind(id).fetch_optional(&mut *conn).await?.ok_or_else(|| AppError::not_found("Trip"))?;
    user.require_location(loc)?;
    Ok(Json(trip_json(&mut conn, id, true).await?))
}

// ------------------------------------------------------------------------------------------------ items

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddItem {
    product_id: Option<Uuid>,
    batch_id: Option<Uuid>,
    qr: Option<String>,
    #[serde(default, deserialize_with = "de_opt_i64")]
    crates: Option<i64>,
}

pub async fn add_item(State(st): State<AppState>, user: AuthUser, headers: axum::http::HeaderMap, ids: Ids, Q(dq): Q<DryQ>, J(b): J<AddItem>) -> AppResult<Response> {
    let mut tx = st.db.begin().await?;
    let mut fx = Fx::default();
    let out = add_item_core(&mut tx, &st, &headers, &user, id1(&ids)?, b, &mut fx).await?;
    conclude(tx, dq.on(), fx, StatusCode::CREATED, out).await
}

async fn add_item_core(tx: &mut sqlx::PgConnection, st: &AppState, headers: &axum::http::HeaderMap, user: &AuthUser, trip_id: Uuid, b: AddItem, fx: &mut Fx) -> AppResult<Value> {
    // 1. the trip row is locked first (global lock order: trip -> batches -> stock rows)
    let trip = lock_trip_for(tx, user, trip_id).await?;
    if trip.status != "LOADING" {
        return Err(invalid_state(format!("Trip {} is {} - items can only be changed while it is LOADING", trip.trip_no, trip.status), &trip.status));
    }
    let modes = [b.product_id.is_some(), b.batch_id.is_some(), b.qr.is_some()].iter().filter(|x| **x).count();
    if modes != 1 { return Err(AppError::field("product_id", "give exactly one of product_id, batch_id or qr")); }
    if let Some(code) = &b.qr {
        if !st.settings.get().qr_labels_enabled {
            return Err(AppError::conflict("FEATURE_DISABLED", "QR labels are switched off. Load stock by product and batch instead."));
        }
        return crate::routes::qr::add_set_to_trip(tx, st, headers, user, &trip, code, b.crates, fx).await;
    }
    let n = b.crates.ok_or_else(|| AppError::field("crates", "is required"))?;
    validate::count("crates", n)?;
    let strict = st.settings.get().strict_qc_lock;

    // 2. decide which batch(es) the crates come from
    let mut allocs: Vec<(Uuid, i64)> = vec![];
    if let Some(pid) = b.product_id {
        let known: Option<bool> = sqlx::query_scalar("SELECT true FROM products WHERE id = $1").bind(pid).fetch_optional(&mut *tx).await?;
        if known.is_none() { return Err(AppError::field("product_id", "unknown product")); }
        allocs = stock::allocate_free_fefo_at(tx, trip.location_id, pid, n).await.map_err(whole_crates_error)?;
    } else if let Some(bid) = b.batch_id {
        allocs.push((bid, n));
    }
    let want: HashMap<Uuid, i64> = allocs.iter().cloned().collect();
    let batch_ids: Vec<Uuid> = allocs.iter().map(|a| a.0).collect();
    let batches = lock_batches(tx, &batch_ids).await?;
    if batches.len() != batch_ids.len() { return Err(AppError::not_found("Batch")); }

    // 3. re-check everything with the batch rows locked (QC rule, free stock incl. our own reservations)
    let mut plan = vec![];
    let mut labels = Labels::new();
    for bt in &batches {
        let take = want[&bt.id];
        let zone = load_zone(bt, strict)?;
        let av = lot_avail(tx, trip.location_id, bt.product_id, bt.id, zone).await?;
        if Decimal::from(take) > av.free { return Err(insufficient(bt, av.free, take)); }
        if bt.qc_status != "APPROVED" {
            fx.warn(format!("Batch {} is still in QC quarantine - these crates are not yet approved for sale", bt.batch_no));
        }
        labels.insert((bt.product_id, bt.id), format!("{} · {}", bt.product_name, bt.batch_no));
        plan.push((bt.clone(), zone, av, take));
    }

    // 4. capacity
    let loaded: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(crates),0)::bigint FROM trip_items WHERE trip_id = $1").bind(trip.id).fetch_one(&mut *tx).await?;
    let adding: i64 = plan.iter().map(|p| p.3).sum();
    let cap = trip.capacity_crates as i64;
    if loaded + adding > cap {
        return Err(AppError::conflict_with("CAPACITY_EXCEEDED",
            format!("Vehicle capacity is {cap} crates; {loaded} are loaded and you are adding {adding}"),
            json!({ "capacity": cap, "loaded": loaded, "adding": adding, "over_by": loaded + adding - cap })));
    }
    if let Some(w) = capacity_warning(loaded + adding, trip.capacity_crates) { fx.warn(w); }

    // 5. write (merge into an existing (trip, batch) item)
    let mut item_ids = vec![];
    for (bt, zone, av, take) in &plan {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO trip_items (trip_id, batch_id, product_id, crates) VALUES ($1,$2,$3,$4)
             ON CONFLICT (trip_id, batch_id) WHERE qr_set_id IS NULL DO UPDATE SET crates = trip_items.crates + EXCLUDED.crates
             RETURNING id")
            .bind(trip.id).bind(bt.id).bind(bt.product_id).bind(*take as i32).fetch_one(&mut *tx).await?;
        item_ids.push(id);
        fx.effects.push(json!({
            "item": labels[&(bt.product_id, bt.id)], "zone": zone, "delta": "0", "after": dec_str(av.on_hand),
            "note": format!("reserves {take} crates for trip {} (free before: {}, after: {})", trip.trip_no, floor_i64(av.free), floor_i64(av.free) - take),
        }));
    }
    let items = items_by_ids(tx, &item_ids).await?;
    let trip_v = trip_json(tx, trip.id, true).await?;
    Ok(json!({ "trip": trip_v, "item": items.first(), "items": items, "residual_set": Value::Null }))
}

pub async fn delete_item(State(st): State<AppState>, user: AuthUser, ids: Ids) -> AppResult<Response> {
    let (trip_id, item_id) = match ids.0.as_slice() { [a, b] => (*a, *b), _ => return Err(AppError::bad("Invalid path")) };
    let mut tx = st.db.begin().await?;
    let trip = lock_trip_for(&mut tx, &user, trip_id).await?;
    if trip.status != "LOADING" {
        return Err(invalid_state(format!("Trip {} is {} - items can only be removed while it is LOADING", trip.trip_no, trip.status), &trip.status));
    }
    let gone: Option<(Option<Uuid>, i32)> = sqlx::query_as("DELETE FROM trip_items WHERE id = $1 AND trip_id = $2 RETURNING qr_set_id, crates")
        .bind(item_id).bind(trip_id).fetch_optional(&mut *tx).await?;
    let (set, crates) = gone.ok_or_else(|| AppError::not_found("Trip item"))?;
    if let Some(s) = set { crate::routes::qr::release_set(&mut tx, s).await?; }
    let out = trip_json(&mut tx, trip_id, true).await?;
    audit::log(&mut *tx, Some(&user), "TRIP_ITEM_REMOVE", "loading_trip", Some(trip_id.to_string()), json!({ "trip_no": trip.trip_no, "item_id": item_id, "crates": crates })).await;
    tx.commit().await?;
    Ok((StatusCode::OK, Json(out)).into_response())
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CancelBody { reason: Option<String> }

pub async fn cancel_trip(State(st): State<AppState>, user: AuthUser, ids: Ids, J(b): J<CancelBody>) -> AppResult<Response> {
    let id = id1(&ids)?;
    let reason = validate::text("reason", b.reason.as_deref().unwrap_or(""), 300)?;
    let mut tx = st.db.begin().await?;
    let trip = lock_trip_for(&mut tx, &user, id).await?;
    if trip.status != "LOADING" {
        return Err(invalid_state(format!("Trip {} is {} - only a LOADING trip can be cancelled (use 'void gate pass' after dispatch)", trip.trip_no, trip.status), &trip.status));
    }
    let sets: Vec<Uuid> = sqlx::query_scalar("SELECT qr_set_id FROM trip_items WHERE trip_id = $1 AND qr_set_id IS NOT NULL ORDER BY qr_set_id")
        .bind(id).fetch_all(&mut *tx).await?;
    for s in sets { crate::routes::qr::release_set(&mut tx, s).await?; }
    sqlx::query("UPDATE loading_trips SET status = 'CANCELLED' WHERE id = $1").bind(id).execute(&mut *tx).await?;
    let out = trip_json(&mut tx, id, true).await?;
    audit::log(&mut *tx, Some(&user), "TRIP_CANCEL", "loading_trip", Some(id.to_string()), json!({ "trip_no": trip.trip_no, "reason": reason })).await;
    tx.commit().await?;
    Ok((StatusCode::OK, Json(out)).into_response())
}

