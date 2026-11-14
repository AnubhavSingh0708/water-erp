//! Ledger-based zone warehouse. ALL stock mutation in the system goes through this module.
//!
//! * `stock_ledger` is append-only (DB trigger forbids UPDATE/DELETE).
//! * `stock_balances` is the transactional current state, one row per (item, zone, lot).
//!   Every move takes `SELECT ... FOR UPDATE` on the balance row and refuses to go below zero.
//! * Multi-move operations must use `post_all` (or `transfer`) so row locks are taken in a deterministic order
//!   (prevents deadlocks between concurrent transactions).
use crate::error::{AppError, AppResult};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

pub const ZONES: &[&str] = &["RM_STORE", "BLOW_FLOOR", "QC_QUARANTINE", "FG_READY", "SCRAP_YARD", "JAR_EMPTY", "JAR_CLEAN"];
pub const NIL_LOT: Uuid = Uuid::nil();
/// The main plant godown. Code that does not name a location operates here (backwards compatible with single-site data).
pub const MAIN_LOCATION: Uuid = Uuid::from_u128(1);
/// System location holding goods between godowns (inter-godown transfers).
pub const TRANSIT_LOCATION: Uuid = Uuid::from_u128(2);

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ItemType { Raw, Product }
impl ItemType {
    pub fn as_str(self) -> &'static str { match self { ItemType::Raw => "RAW", ItemType::Product => "PRODUCT" } }
}

#[derive(Clone, Debug)]
pub struct Move {
    /// Godown. Defaults to MAIN_LOCATION; use `.at(location)` for other godowns.
    pub location: Uuid,
    pub item_type: ItemType,
    pub item_id: Uuid,
    pub zone: &'static str,
    pub lot_id: Uuid,
    pub delta: Decimal,
    pub txn_type: &'static str,
    pub ref_type: Option<&'static str>,
    pub ref_id: Option<Uuid>,
    pub note: String,
    pub user_id: Option<Uuid>,
    /// Stored on the balance row when it is first created (FEFO key). Ignored for existing rows.
    pub expiry_date: Option<NaiveDate>,
}

impl Move {
    pub fn new(item_type: ItemType, item_id: Uuid, zone: &'static str, lot_id: Uuid, delta: Decimal, txn_type: &'static str) -> Move {
        Move { location: MAIN_LOCATION, item_type, item_id, zone, lot_id, delta, txn_type, ref_type: None, ref_id: None, note: String::new(), user_id: None, expiry_date: None }
    }
    pub fn at(mut self, location: Uuid) -> Self { self.location = location; self }
    pub fn reference(mut self, ref_type: &'static str, ref_id: Uuid) -> Self { self.ref_type = Some(ref_type); self.ref_id = Some(ref_id); self }
    pub fn by(mut self, user: Option<Uuid>) -> Self { self.user_id = user; self }
    pub fn note(mut self, n: impl Into<String>) -> Self { self.note = n.into(); self }
    pub fn expiry(mut self, e: Option<NaiveDate>) -> Self { self.expiry_date = e; self }
}

fn check_zone(z: &str) -> AppResult<()> {
    if ZONES.contains(&z) { Ok(()) } else { Err(AppError::bad(format!("unknown zone {z}"))) }
}

/// Post a single move inside the caller's transaction. Returns the new balance.
pub async fn post(tx: &mut PgConnection, m: &Move, group: Uuid) -> AppResult<Decimal> {
    check_zone(m.zone)?;
    if m.delta.is_zero() { return Err(AppError::bad("stock movement quantity must not be zero")); }
    sqlx::query(
        "INSERT INTO stock_balances (item_type, item_id, zone, lot_id, qty, expiry_date, location_id)
         VALUES ($1,$2,$3,$4,0,$5,$6) ON CONFLICT (location_id, item_type, item_id, zone, lot_id) DO NOTHING")
        .bind(m.item_type.as_str()).bind(m.item_id).bind(m.zone).bind(m.lot_id).bind(m.expiry_date).bind(m.location)
        .execute(&mut *tx).await?;
    let cur: Decimal = sqlx::query_scalar(
        "SELECT qty FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone=$3 AND lot_id=$4 AND location_id=$5 FOR UPDATE")
        .bind(m.item_type.as_str()).bind(m.item_id).bind(m.zone).bind(m.lot_id).bind(m.location)
        .fetch_one(&mut *tx).await?;
    let new = cur + m.delta;
    if new < Decimal::ZERO {
        return Err(AppError::conflict_with("INSUFFICIENT_STOCK",
            format!("Insufficient stock in {}: have {}, need {}", m.zone, cur.normalize(), (-m.delta).normalize()),
            json!({ "item_type": m.item_type.as_str(), "item_id": m.item_id, "zone": m.zone, "lot_id": m.lot_id, "location_id": m.location, "available": cur, "required": -m.delta })));
    }
    sqlx::query("UPDATE stock_balances SET qty=$5, updated_at=now() WHERE item_type=$1 AND item_id=$2 AND zone=$3 AND lot_id=$4 AND location_id=$6")
        .bind(m.item_type.as_str()).bind(m.item_id).bind(m.zone).bind(m.lot_id).bind(new).bind(m.location)
        .execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO stock_ledger (txn_group,item_type,item_id,zone,lot_id,qty_delta,balance_after,txn_type,ref_type,ref_id,note,user_id,location_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)")
        .bind(group).bind(m.item_type.as_str()).bind(m.item_id).bind(m.zone).bind(m.lot_id).bind(m.delta).bind(new)
        .bind(m.txn_type).bind(m.ref_type).bind(m.ref_id).bind(&m.note).bind(m.user_id).bind(m.location)
        .execute(&mut *tx).await?;
    Ok(new)
}

/// Post several moves atomically (caller supplies the transaction). Locks are taken in a stable order.
pub async fn post_all(tx: &mut PgConnection, mut moves: Vec<Move>) -> AppResult<Uuid> {
    let group = Uuid::new_v4();
    moves.sort_by(|a, b| (a.location, a.item_type, a.item_id, a.zone, a.lot_id).cmp(&(b.location, b.item_type, b.item_id, b.zone, b.lot_id)));
    for m in &moves { post(&mut *tx, m, group).await?; }
    Ok(group)
}

/// Move `qty` of one lot between zones of the MAIN godown (two ledger rows, same txn_group). Expiry date is carried over.
pub async fn transfer(tx: &mut PgConnection, item_type: ItemType, item_id: Uuid, lot_id: Uuid, from: &'static str, to: &'static str,
                      qty: Decimal, txn_type: &'static str, ref_type: Option<&'static str>, ref_id: Option<Uuid>, user: Option<Uuid>, note: &str) -> AppResult<Uuid> {
    transfer_between(tx, item_type, item_id, lot_id, MAIN_LOCATION, from, MAIN_LOCATION, to, qty, txn_type, ref_type, ref_id, user, note).await
}

/// Move `qty` of one lot between any two (godown, zone) pairs. Both ledger rows share a txn_group; expiry is carried over.
/// Inter-godown moves must go through the TRANSIT location (see `godown_transfers`), never directly.
pub async fn transfer_between(tx: &mut PgConnection, item_type: ItemType, item_id: Uuid, lot_id: Uuid,
                      from_loc: Uuid, from: &'static str, to_loc: Uuid, to: &'static str,
                      qty: Decimal, txn_type: &'static str, ref_type: Option<&'static str>, ref_id: Option<Uuid>, user: Option<Uuid>, note: &str) -> AppResult<Uuid> {
    let exp: Option<NaiveDate> = sqlx::query_scalar(
        "SELECT expiry_date FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone=$3 AND lot_id=$4 AND location_id=$5")
        .bind(item_type.as_str()).bind(item_id).bind(from).bind(lot_id).bind(from_loc).fetch_optional(&mut *tx).await?.flatten();
    let mut out = Move::new(item_type, item_id, from, lot_id, -qty, txn_type).at(from_loc).by(user).note(note);
    let mut inn = Move::new(item_type, item_id, to, lot_id, qty, txn_type).at(to_loc).by(user).note(note).expiry(exp);
    if let (Some(rt), Some(ri)) = (ref_type, ref_id) { out = out.reference(rt, ri); inn = inn.reference(rt, ri); }
    post_all(tx, vec![out, inn]).await
}

/// Consume `qty` from a zone of the MAIN godown using FEFO.
pub async fn consume_fefo(tx: &mut PgConnection, item_type: ItemType, item_id: Uuid, zone: &'static str, qty: Decimal,
                          txn_type: &'static str, ref_type: Option<&'static str>, ref_id: Option<Uuid>, user: Option<Uuid>, note: &str)
                          -> AppResult<Vec<(Uuid, Decimal)>> {
    consume_fefo_at(tx, MAIN_LOCATION, item_type, item_id, zone, qty, txn_type, ref_type, ref_id, user, note).await
}

/// Consume `qty` from a zone of one godown using FEFO (earliest expiry first, then oldest received). Returns `(lot_id, qty)` taken.
pub async fn consume_fefo_at(tx: &mut PgConnection, location: Uuid, item_type: ItemType, item_id: Uuid, zone: &'static str, qty: Decimal,
                          txn_type: &'static str, ref_type: Option<&'static str>, ref_id: Option<Uuid>, user: Option<Uuid>, note: &str)
                          -> AppResult<Vec<(Uuid, Decimal)>> {
    check_zone(zone)?;
    let rows = sqlx::query(
        "SELECT lot_id, qty FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone=$3 AND location_id=$4 AND qty>0
         ORDER BY expiry_date ASC NULLS LAST, received_at ASC, lot_id ASC FOR UPDATE")
        .bind(item_type.as_str()).bind(item_id).bind(zone).bind(location).fetch_all(&mut *tx).await?;
    let total: Decimal = rows.iter().map(|r| r.get::<Decimal, _>("qty")).sum();
    if total < qty {
        return Err(AppError::conflict_with("INSUFFICIENT_STOCK",
            format!("Insufficient stock in {zone}: have {}, need {}", total.normalize(), qty.normalize()),
            json!({ "item_type": item_type.as_str(), "item_id": item_id, "zone": zone, "location_id": location, "available": total, "required": qty })));
    }
    let group = Uuid::new_v4();
    let mut left = qty;
    let mut taken = vec![];
    for r in rows {
        if left.is_zero() { break; }
        let lot: Uuid = r.get("lot_id");
        let have: Decimal = r.get("qty");
        let take = have.min(left);
        let mut m = Move::new(item_type, item_id, zone, lot, -take, txn_type).at(location).by(user).note(note);
        if let (Some(rt), Some(ri)) = (ref_type, ref_id) { m = m.reference(rt, ri); }
        post(&mut *tx, &m, group).await?;
        taken.push((lot, take));
        left -= take;
    }
    Ok(taken)
}

/// Total quantity of an item in a zone of the MAIN godown (all lots). Not locked – informational.
pub async fn available(db: &mut PgConnection, item_type: ItemType, item_id: Uuid, zone: &str) -> AppResult<Decimal> {
    available_at(db, MAIN_LOCATION, item_type, item_id, zone).await
}
pub async fn available_at(db: &mut PgConnection, location: Uuid, item_type: ItemType, item_id: Uuid, zone: &str) -> AppResult<Decimal> {
    let q: Option<Decimal> = sqlx::query_scalar("SELECT SUM(qty) FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone=$3 AND location_id=$4")
        .bind(item_type.as_str()).bind(item_id).bind(zone).bind(location).fetch_one(&mut *db).await?;
    Ok(q.unwrap_or_default())
}

// ---------------------------------------------------------------------------------------------------------------
// Reservation-aware availability of finished goods, PER GODOWN. Crates added to a trip that is still LOADING are
// *reserved* in the trip's source godown: they have not left FG_READY yet (that happens at gate pass) but nobody else
// may take them. EVERY flow that deducts finished goods from FG_READY (dispatch notes, jar counter issue, write-offs,
// scrap transfers, negative adjustments, godown transfers) must use these helpers instead of computing availability itself.
// The un-suffixed functions operate on the MAIN godown.
// ---------------------------------------------------------------------------------------------------------------

/// Crates of `batch_id` held by LOADING trips whose source godown is MAIN (all such trips, including the caller's own).
pub async fn fg_reserved(conn: &mut PgConnection, batch_id: Uuid) -> AppResult<Decimal> { fg_reserved_at(conn, MAIN_LOCATION, batch_id).await }
pub async fn fg_reserved_at(conn: &mut PgConnection, location: Uuid, batch_id: Uuid) -> AppResult<Decimal> {
    let r: Option<Decimal> = sqlx::query_scalar(
        "SELECT SUM(ti.crates)::numeric FROM trip_items ti JOIN loading_trips t ON t.id = ti.trip_id
         WHERE ti.batch_id = $1 AND t.status = 'LOADING' AND t.location_id = $2")
        .bind(batch_id).bind(location).fetch_one(&mut *conn).await?;
    Ok(r.unwrap_or_default())
}

pub async fn fg_free(conn: &mut PgConnection, product_id: Uuid, batch_id: Uuid) -> AppResult<Decimal> { fg_free_at(conn, MAIN_LOCATION, product_id, batch_id).await }
/// `FG_READY` quantity of the batch in that godown minus reservations.
pub async fn fg_free_at(conn: &mut PgConnection, location: Uuid, product_id: Uuid, batch_id: Uuid) -> AppResult<Decimal> {
    let have = sqlx::query_scalar::<_, Decimal>(
        "SELECT COALESCE(SUM(qty),0) FROM stock_balances WHERE item_type='PRODUCT' AND item_id=$1 AND zone='FG_READY' AND lot_id=$2 AND location_id=$3")
        .bind(product_id).bind(batch_id).bind(location).fetch_one(&mut *conn).await?;
    Ok(have - fg_reserved_at(&mut *conn, location, batch_id).await?)
}

pub async fn allocate_free_fefo(conn: &mut PgConnection, product_id: Uuid, crates: i64) -> AppResult<Vec<(Uuid, i64)>> {
    allocate_free_fefo_at(conn, MAIN_LOCATION, product_id, crates).await
}
/// FEFO-allocate `crates` of a product from APPROVED batches' *free* FG_READY stock of one godown. Locks the batch rows
/// (`FOR UPDATE`, in a separate statement from the availability read) so concurrent allocators on the same batch serialise.
/// Returns `(batch_id, crates)`; does NOT move stock – the caller posts the ledger rows inside the same transaction.
pub async fn allocate_free_fefo_at(conn: &mut PgConnection, location: Uuid, product_id: Uuid, crates: i64) -> AppResult<Vec<(Uuid, i64)>> {
    // Step 1: lock the candidate batch rows (stable order). Under READ COMMITTED a statement that waits for a row lock keeps its
    // old snapshot, so reservations that committed while we waited would be invisible if we computed them in this same statement.
    sqlx::query("SELECT b.id FROM production_batches b WHERE b.product_id = $1 AND b.qc_status = 'APPROVED'
                 ORDER BY b.expiry_date ASC, b.created_at ASC, b.id ASC FOR UPDATE OF b")
        .bind(product_id).fetch_all(&mut *conn).await?;
    // Step 2: a NEW statement (fresh snapshot, locks now held) computes stock and reservations.
    let rows = sqlx::query(
        "SELECT b.id, sb.qty,
                COALESCE((SELECT SUM(ti.crates) FROM trip_items ti JOIN loading_trips t ON t.id = ti.trip_id
                          WHERE ti.batch_id = b.id AND t.status = 'LOADING' AND t.location_id = $2), 0)::numeric AS reserved
         FROM production_batches b
         JOIN stock_balances sb ON sb.item_type='PRODUCT' AND sb.item_id=b.product_id AND sb.zone='FG_READY' AND sb.lot_id=b.id AND sb.location_id = $2
         WHERE b.product_id = $1 AND b.qc_status = 'APPROVED' AND sb.qty > 0
         ORDER BY b.expiry_date ASC, b.created_at ASC, b.id ASC")
        .bind(product_id).bind(location).fetch_all(&mut *conn).await?;
    let mut left = crates;
    let mut out = vec![];
    let mut total_free = Decimal::ZERO;
    for r in &rows {
        let free: Decimal = r.get::<Decimal, _>("qty") - r.get::<Decimal, _>("reserved");
        if free > Decimal::ZERO { total_free += free; }
        if left == 0 || free <= Decimal::ZERO { continue; }
        let take = free.floor().to_string().parse::<i64>().unwrap_or(0).min(left);
        if take > 0 { out.push((r.get::<Uuid, _>("id"), take)); left -= take; }
    }
    if left > 0 {
        return Err(AppError::conflict_with("INSUFFICIENT_STOCK",
            format!("Only {} crates are free to load/dispatch here (the rest is reserved by open loading trips, not QC-approved, or in another godown)", total_free.normalize()),
            json!({ "product_id": product_id, "location_id": location, "available": total_free, "required": crates })));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------------
// Over-production guard. Finished goods can only come from a production batch, so no correction (positive adjustment,
// count surplus) may make the quantity of a batch lot that is on record exceed `production_batches.crates`.
// ---------------------------------------------------------------------------------------------------------------

/// Crates of the batch lot currently on record in FG_READY + QC_QUARANTINE across ALL godowns including TRANSIT (crates on
/// DISPATCHED trips / delivered / scrapped are not counted: a correction can only be blocked when the on-hand quantity alone is too much).
/// The batch row must already be locked by the caller (`lock_batch_for_cap` does it) so concurrent writers serialise.
pub async fn lot_on_record(conn: &mut PgConnection, batch_id: Uuid) -> AppResult<Decimal> {
    Ok(sqlx::query_scalar::<_, Decimal>(
        "SELECT COALESCE(SUM(qty),0) FROM stock_balances WHERE item_type='PRODUCT' AND lot_id=$1 AND zone IN ('FG_READY','QC_QUARANTINE')")
        .bind(batch_id).fetch_one(&mut *conn).await?)
}

/// `409 OVER_PRODUCED` (never overridable by `confirm`) when adding `add` crates to the batch lot would exceed what the batch produced.
/// Locks the batch row `FOR UPDATE` first (canonical order: batch row before stock balance rows), then reads the quantity in a NEW statement
/// so a writer that committed while we waited for the lock is visible. `already_added` = crates the same request has staged but not posted yet.
pub async fn guard_over_produced(conn: &mut PgConnection, batch_id: Uuid, add: Decimal, already_added: Decimal) -> AppResult<()> {
    let b: Option<(i32, String)> = sqlx::query_as("SELECT crates, batch_no FROM production_batches WHERE id = $1 FOR UPDATE").bind(batch_id).fetch_optional(&mut *conn).await?;
    let (produced, batch_no) = b.ok_or_else(|| AppError::not_found("Batch"))?;
    let on_record = lot_on_record(&mut *conn, batch_id).await? + already_added;
    if on_record + add > Decimal::from(produced) {
        return Err(AppError::conflict_with("OVER_PRODUCED",
            format!("Batch {batch_no} only produced {produced} crates; {} are already on record, so {} more cannot be added", on_record.normalize(), add.normalize()),
            json!({ "batch_id": batch_id, "batch_no": batch_no, "produced": produced, "on_record": on_record, "adding": add })));
    }
    Ok(())
}
