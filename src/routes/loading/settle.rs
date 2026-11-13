//! Trip settlement / return reconciliation: per-item delivered + returned + damaged == loaded, restock, scrap,
//! returnable-container customer ledger, JAR_EMPTY stock and fleet events.
use super::common::*;
use super::trips::id1;
use crate::{
    alerts::{self, NewAlert},
    audit,
    auth::AuthUser,
    error::{AppError, AppResult},
    state::AppState,
    stock::{self, ItemType, Move},
    util, validate,
};
use axum::{extract::State, http::StatusCode, response::Response};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettleItem {
    item_id: Uuid,
    #[serde(deserialize_with = "de_i64")] delivered: i64,
    #[serde(deserialize_with = "de_i64")] returned: i64,
    #[serde(deserialize_with = "de_i64")] damaged: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JarLine {
    customer_id: Uuid,
    product_id: Uuid,
    #[serde(deserialize_with = "de_i64")] delivered: i64,
    #[serde(default, deserialize_with = "de_i64")] empties_collected: i64,
    #[serde(default, deserialize_with = "de_i64")] damaged: i64,
    #[serde(default, deserialize_with = "de_i64")] lost: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettleBody {
    items: Vec<SettleItem>,
    #[serde(default)]
    customer_jars: Vec<JarLine>,
    #[serde(default)]
    confirm: bool,
}

const MAX: i64 = 1_000_000;
fn in_range(v: i64) -> bool { (0..=MAX).contains(&v) }

pub async fn settle(State(st): State<AppState>, user: AuthUser, ids: Ids, Q(dq): Q<DryQ>, J(b): J<SettleBody>) -> AppResult<Response> {
    let dry = dq.on();
    let mut tx = st.db.begin().await?;
    let mut fx = Fx::default();
    let out = settle_core(&mut tx, &st, &user, id1(&ids)?, b, &mut fx).await?;
    conclude(tx, dry, fx, StatusCode::OK, out).await
}

async fn settle_core(tx: &mut sqlx::PgConnection, st: &AppState, user: &AuthUser, trip_id: Uuid, b: SettleBody, fx: &mut Fx) -> AppResult<Value> {
    let trip = lock_trip_for(tx, user, trip_id).await?; // authorisation (godown) before body validation
    validate::max_items("items", b.items.len(), 1000)?;
    if b.customer_jars.len() > 500 { return Err(AppError::field("customer_jars", "at most 500 entries allowed")); }
    if trip.status != "DISPATCHED" {
        return Err(invalid_state(format!("Trip {} is {} - only a DISPATCHED trip can be settled", trip.trip_no, trip.status), &trip.status));
    }
    let items = trip_items(tx, trip_id).await?;
    let by_id: HashMap<Uuid, &ItemRow> = items.iter().map(|i| (i.id, i)).collect();

    // ---------- validation of the request against the trip (nothing is written yet)
    let mut errs: Vec<(String, String)> = vec![];
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut req: HashMap<Uuid, (i64, i64, i64)> = HashMap::new();
    for (i, si) in b.items.iter().enumerate() {
        let Some(it) = by_id.get(&si.item_id) else { errs.push((format!("items[{i}].item_id"), "is not an item of this trip".into())); continue };
        if !seen.insert(si.item_id) { errs.push((format!("items[{i}].item_id"), "is listed more than once".into())); continue; }
        let mut ok = true;
        for (f, v) in [("delivered", si.delivered), ("returned", si.returned), ("damaged", si.damaged)] {
            if !in_range(v) { errs.push((format!("items[{i}].{f}"), "must be a whole number between 0 and 1,000,000".into())); ok = false; }
        }
        if ok && si.delivered + si.returned + si.damaged != it.crates as i64 {
            errs.push((format!("items[{i}]"), format!("delivered + returned + damaged ({}) must equal the {} crates loaded of {} ({})",
                si.delivered + si.returned + si.damaged, it.crates, it.product_name, it.batch_no)));
        }
        req.insert(si.item_id, (si.delivered, si.returned, si.damaged));
    }
    for it in &items {
        if !seen.contains(&it.id) { errs.push((format!("items.{}", it.id), format!("{} ({}) is missing - every loaded item must be settled", it.product_name, it.batch_no))); }
    }
    if !errs.is_empty() { return Err(validation(errs)); }

    // returnable containers: per product the customer lines must add up to what the items delivered
    let mut ret_delivered: BTreeMap<Uuid, i64> = BTreeMap::new();
    let mut prod_name: HashMap<Uuid, String> = HashMap::new();
    for it in &items {
        prod_name.insert(it.product_id, it.product_name.clone());
        if it.kind == "RETURNABLE" { *ret_delivered.entry(it.product_id).or_default() += req[&it.id].0; }
    }
    let mut line_sum: HashMap<Uuid, i64> = HashMap::new();
    let mut seen_lines: HashSet<(Uuid, Uuid)> = HashSet::new();
    for (i, l) in b.customer_jars.iter().enumerate() {
        if !ret_delivered.contains_key(&l.product_id) {
            errs.push((format!("customer_jars[{i}].product_id"), "is not a returnable product on this trip".into()));
            continue;
        }
        if !seen_lines.insert((l.customer_id, l.product_id)) {
            errs.push((format!("customer_jars[{i}]"), "this customer/product pair is listed more than once - combine the quantities".into()));
        }
        for (f, v) in [("delivered", l.delivered), ("empties_collected", l.empties_collected), ("damaged", l.damaged), ("lost", l.lost)] {
            if !in_range(v) { errs.push((format!("customer_jars[{i}].{f}"), "must be a whole number between 0 and 1,000,000".into())); }
        }
        *line_sum.entry(l.product_id).or_default() += l.delivered.clamp(0, MAX);
    }
    for (pid, delivered) in &ret_delivered {
        let got = line_sum.get(pid).copied().unwrap_or(0);
        if got != *delivered {
            errs.push((format!("customer_jars.{pid}"), format!("customer lines deliver {got} of {} but the trip items delivered {delivered}",
                prod_name.get(pid).cloned().unwrap_or_default())));
        }
    }
    if !errs.is_empty() { return Err(validation(errs)); }
    let cust_ids: Vec<Uuid> = { let mut v: Vec<Uuid> = b.customer_jars.iter().map(|l| l.customer_id).collect(); v.sort(); v.dedup(); v };
    let mut cust_name: HashMap<Uuid, String> = HashMap::new();
    if !cust_ids.is_empty() {
        let rows: Vec<(Uuid, String, bool)> = sqlx::query_as("SELECT id, name, is_active FROM customers WHERE id = ANY($1)")
            .bind(&cust_ids).fetch_all(&mut *tx).await?;
        for (id, name, active) in rows { if active { cust_name.insert(id, name); } }
        for (i, l) in b.customer_jars.iter().enumerate() {
            if !cust_name.contains_key(&l.customer_id) { errs.push((format!("customer_jars[{i}].customer_id"), "unknown or inactive customer".into())); }
        }
        if !errs.is_empty() { return Err(validation(errs)); }
    }

    // damage threshold -> confirm
    let tot_loaded: i64 = items.iter().map(|i| i.crates as i64).sum();
    let tot_damaged: i64 = req.values().map(|r| r.2).sum();
    let tot_returned: i64 = req.values().map(|r| r.1).sum();
    let tot_delivered: i64 = req.values().map(|r| r.0).sum();
    let warn_pct = st.settings.get().damage_warn_pct;
    if tot_damaged > 0 && tot_damaged * 100 > warn_pct * tot_loaded && !b.confirm {
        return Err(AppError::confirm(
            format!("{tot_damaged} of {tot_loaded} crates ({}%) are reported damaged", tot_damaged * 100 / tot_loaded.max(1)),
            vec![format!("Damaged crates are above {warn_pct}% of the load. Check the count, then confirm.")]));
    }
    if tot_damaged > 0 { fx.warn(format!("{tot_damaged} damaged crates will be moved to the scrap yard")); }

    // ---------- writes (trip -> batches -> stock rows -> customers' ledger)
    let mut batch_ids: Vec<Uuid> = items.iter().map(|i| i.batch_id).collect();
    batch_ids.sort(); batch_ids.dedup();
    let batches = lock_batches(tx, &batch_ids).await?;
    let bmap: HashMap<Uuid, &BatchRow> = batches.iter().map(|b| (b.id, b)).collect();
    let mut labels = Labels::new();
    let mut moves: Vec<Move> = vec![];
    for bt in &batches { labels.insert((bt.product_id, bt.id), format!("{} · {}", bt.product_name, bt.batch_no)); }
    for it in &items {
        let (_, ret, dmg) = req[&it.id];
        let bt = bmap[&it.batch_id];
        if ret > 0 {
            moves.push(product_move(trip.location_id, it.product_id, restock_zone(&bt.qc_status), it.batch_id, ret, "TRIP_RETURN")
                .reference("TRIP", trip_id).by(Some(user.id)).expiry(Some(bt.expiry_date)).note(format!("return from trip {}", trip.trip_no)));
        }
        if dmg > 0 {
            moves.push(product_move(trip.location_id, it.product_id, "SCRAP_YARD", it.batch_id, dmg, "TRIP_DAMAGED")
                .reference("TRIP", trip_id).by(Some(user.id)).expiry(Some(bt.expiry_date)).note(format!("damaged on trip {}", trip.trip_no)));
        }
    }
    let today = util::plant_today(tx).await?;
    // containers
    let mut jar_empty: BTreeMap<Uuid, i64> = BTreeMap::new();
    let mut jar_scrap: BTreeMap<Uuid, i64> = BTreeMap::new();
    for (pid, name) in &prod_name { labels.insert((*pid, stock::NIL_LOT), format!("{name} (containers)")); }
    for l in &b.customer_jars {
        *jar_empty.entry(l.product_id).or_default() += l.empties_collected;
        *jar_scrap.entry(l.product_id).or_default() += l.damaged;
    }
    for (pid, n) in &jar_empty {
        if *n > 0 { moves.push(Move::new(ItemType::Product, *pid, "JAR_EMPTY", stock::NIL_LOT, Decimal::from(*n), "JAR_RECEIVE").at(trip.location_id)
            .reference("TRIP", trip_id).by(Some(user.id)).note(format!("empties collected on trip {}", trip.trip_no))); }
    }
    for (pid, n) in &jar_scrap {
        if *n > 0 { moves.push(Move::new(ItemType::Product, *pid, "SCRAP_YARD", stock::NIL_LOT, Decimal::from(*n), "TRIP_DAMAGED").at(trip.location_id)
            .reference("TRIP", trip_id).by(Some(user.id)).note(format!("damaged empties from customers, trip {}", trip.trip_no))); }
    }
    post_moves(tx, moves, &labels, fx).await?;

    // customers last in the lock order (trip -> batches -> stock -> customers): lock their rows (sorted), then check the holdings
    if !cust_ids.is_empty() {
        sqlx::query("SELECT id FROM customers WHERE id = ANY($1) ORDER BY id FOR UPDATE").bind(&cust_ids).fetch_all(&mut *tx).await?;
    }
    let mut ledger_rows: Vec<(Uuid, Uuid, &'static str, i64)> = vec![];
    for l in &b.customer_jars {
        let before: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(delta),0)::bigint FROM jar_ledger WHERE customer_id = $1 AND product_id = $2")
            .bind(l.customer_id).bind(l.product_id).fetch_one(&mut *tx).await?;
        let after = before + l.delivered - l.empties_collected - l.damaged - l.lost;
        let cname = cust_name[&l.customer_id].clone();
        if after < 0 {
            return Err(AppError::bad(format!("Customer {cname} holds {before} {} and receives {} - they cannot return/lose {} (would be {after})",
                prod_name[&l.product_id], l.delivered, l.empties_collected + l.damaged + l.lost)));
        }
        for (t, d) in [("ISSUE_FILLED", l.delivered), ("RETURN_EMPTY", -l.empties_collected), ("DAMAGED", -l.damaged), ("LOST", -l.lost)] {
            if d != 0 { ledger_rows.push((l.customer_id, l.product_id, t, d)); }
        }
        fx.effects.push(json!({ "item": format!("{cname} · {} (jars at customer)", prod_name[&l.product_id]), "zone": "CUSTOMER",
            "delta": (l.delivered - l.empties_collected - l.damaged - l.lost).to_string(), "after": after.to_string(), "txn_type": "JAR_LEDGER" }));
    }
    for (cust, prod, t, d) in &ledger_rows {
        sqlx::query("INSERT INTO jar_ledger (customer_id, product_id, txn_type, delta, txn_date, trip_id, note, created_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(cust).bind(prod).bind(*t).bind(*d as i32).bind(today).bind(trip_id).bind(format!("trip {}", trip.trip_no)).bind(user.id)
            .execute(&mut *tx).await?;
    }
    // fleet lifecycle events (aggregate): damaged in the vehicle, damaged/lost at the customer
    for it in &items {
        let dmg = req[&it.id].2;
        if it.kind == "RETURNABLE" && dmg > 0 { fleet_event(tx, user, it.product_id, dmg, "DAMAGED_RETURN", "VEHICLE", None, trip_id, &trip.trip_no, trip.location_id).await?; }
    }
    for l in &b.customer_jars {
        if l.damaged > 0 { fleet_event(tx, user, l.product_id, l.damaged, "DAMAGED_RETURN", "CUSTOMER", Some(l.customer_id), trip_id, &trip.trip_no, trip.location_id).await?; }
        if l.lost > 0 { fleet_event(tx, user, l.product_id, l.lost, "LOST", "CUSTOMER", Some(l.customer_id), trip_id, &trip.trip_no, trip.location_id).await?; }
    }

    for it in &items {
        let (d, r, x) = req[&it.id];
        sqlx::query("UPDATE trip_items SET crates_delivered = $2, crates_returned = $3, crates_damaged = $4 WHERE id = $1")
            .bind(it.id).bind(d as i32).bind(r as i32).bind(x as i32).execute(&mut *tx).await?;
        if let Some(sid) = it.qr_set_id { crate::routes::qr::on_settle(tx, sid, trip_id, r).await?; }
    }
    sqlx::query("UPDATE loading_trips SET status = 'SETTLED', settled_at = now(), returned_at = now(), settled_by = $2 WHERE id = $1")
        .bind(trip_id).bind(user.id).execute(&mut *tx).await?;
    let mut out = trip_json(tx, trip_id, true).await?;
    out["settlement"] = json!({ "loaded": tot_loaded, "delivered": tot_delivered, "returned": tot_returned, "damaged": tot_damaged });

    alerts::notify_once(&mut *tx, NewAlert {
        event_type: "TRIP_SETTLED", dedupe_key: &trip_id.to_string(), severity: if tot_damaged > 0 { "WARNING" } else { "INFO" },
        title: &format!("Trip {} settled", trip.trip_no),
        body: &format!("Delivered {tot_delivered}, returned {tot_returned}, damaged {tot_damaged} of {tot_loaded} crates"),
        url: "/loading.html", payload: json!({ "trip_id": trip_id, "delivered": tot_delivered, "returned": tot_returned, "damaged": tot_damaged }),
    }).await?;
    audit::log(&mut *tx, Some(user), "TRIP_SETTLED", "loading_trip", Some(trip_id.to_string()),
        json!({ "trip_no": trip.trip_no, "loaded": tot_loaded, "delivered": tot_delivered, "returned": tot_returned, "damaged": tot_damaged,
                "jar_lines": b.customer_jars.len(), "confirmed": b.confirm })).await;
    Ok(out)
}

async fn fleet_event(tx: &mut sqlx::PgConnection, user: &AuthUser, product: Uuid, qty: i64, reason: &str, location: &str, customer: Option<Uuid>, trip_id: Uuid, trip_no: &str, loc_id: Uuid) -> AppResult<()> {
    sqlx::query("INSERT INTO jar_fleet_events (product_id, event_type, qty, reason_code, location, customer_id, trip_id, note, created_by, location_id)
                 VALUES ($1,'DESTROYED',$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(product).bind(qty as i32).bind(reason).bind(location).bind(customer).bind(trip_id).bind(format!("trip {trip_no}")).bind(user.id).bind(loc_id)
        .execute(&mut *tx).await?;
    Ok(())
}
