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

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ItemType { Raw, Product }
impl ItemType {
    pub fn as_str(self) -> &'static str { match self { ItemType::Raw => "RAW", ItemType::Product => "PRODUCT" } }
}

#[derive(Clone, Debug)]
pub struct Move {
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
        Move { item_type, item_id, zone, lot_id, delta, txn_type, ref_type: None, ref_id: None, note: String::new(), user_id: None, expiry_date: None }
    }
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
        "INSERT INTO stock_balances (item_type, item_id, zone, lot_id, qty, expiry_date)
         VALUES ($1,$2,$3,$4,0,$5) ON CONFLICT (item_type, item_id, zone, lot_id) DO NOTHING")
        .bind(m.item_type.as_str()).bind(m.item_id).bind(m.zone).bind(m.lot_id).bind(m.expiry_date)
        .execute(&mut *tx).await?;
    let cur: Decimal = sqlx::query_scalar(
        "SELECT qty FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone=$3 AND lot_id=$4 FOR UPDATE")
        .bind(m.item_type.as_str()).bind(m.item_id).bind(m.zone).bind(m.lot_id)
        .fetch_one(&mut *tx).await?;
    let new = cur + m.delta;
    if new < Decimal::ZERO {
        return Err(AppError::conflict_with("INSUFFICIENT_STOCK",
            format!("Insufficient stock in {}: have {}, need {}", m.zone, cur.normalize(), (-m.delta).normalize()),
            json!({ "item_type": m.item_type.as_str(), "item_id": m.item_id, "zone": m.zone, "lot_id": m.lot_id, "available": cur, "required": -m.delta })));
    }
    sqlx::query("UPDATE stock_balances SET qty=$5, updated_at=now() WHERE item_type=$1 AND item_id=$2 AND zone=$3 AND lot_id=$4")
        .bind(m.item_type.as_str()).bind(m.item_id).bind(m.zone).bind(m.lot_id).bind(new)
        .execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO stock_ledger (txn_group,item_type,item_id,zone,lot_id,qty_delta,balance_after,txn_type,ref_type,ref_id,note,user_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
        .bind(group).bind(m.item_type.as_str()).bind(m.item_id).bind(m.zone).bind(m.lot_id).bind(m.delta).bind(new)
        .bind(m.txn_type).bind(m.ref_type).bind(m.ref_id).bind(&m.note).bind(m.user_id)
        .execute(&mut *tx).await?;
    Ok(new)
}

/// Post several moves atomically (caller supplies the transaction). Locks are taken in a stable order.
pub async fn post_all(tx: &mut PgConnection, mut moves: Vec<Move>) -> AppResult<Uuid> {
    let group = Uuid::new_v4();
    moves.sort_by(|a, b| (a.item_type, a.item_id, a.zone, a.lot_id).cmp(&(b.item_type, b.item_id, b.zone, b.lot_id)));
    for m in &moves { post(&mut *tx, m, group).await?; }
    Ok(group)
}

/// Move `qty` of one lot between zones (two ledger rows, same txn_group). Expiry date is carried over.
pub async fn transfer(tx: &mut PgConnection, item_type: ItemType, item_id: Uuid, lot_id: Uuid, from: &'static str, to: &'static str,
                      qty: Decimal, txn_type: &'static str, ref_type: Option<&'static str>, ref_id: Option<Uuid>, user: Option<Uuid>, note: &str) -> AppResult<Uuid> {
    let exp: Option<NaiveDate> = sqlx::query_scalar(
        "SELECT expiry_date FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone=$3 AND lot_id=$4")
        .bind(item_type.as_str()).bind(item_id).bind(from).bind(lot_id).fetch_optional(&mut *tx).await?.flatten();
    let mut out = Move::new(item_type, item_id, from, lot_id, -qty, txn_type).by(user).note(note);
    let mut inn = Move::new(item_type, item_id, to, lot_id, qty, txn_type).by(user).note(note).expiry(exp);
    if let (Some(rt), Some(ri)) = (ref_type, ref_id) { out = out.reference(rt, ri); inn = inn.reference(rt, ri); }
    post_all(tx, vec![out, inn]).await
}

/// Consume `qty` from a zone using FEFO (earliest expiry first, then oldest received). Returns `(lot_id, qty)` taken.
pub async fn consume_fefo(tx: &mut PgConnection, item_type: ItemType, item_id: Uuid, zone: &'static str, qty: Decimal,
                          txn_type: &'static str, ref_type: Option<&'static str>, ref_id: Option<Uuid>, user: Option<Uuid>, note: &str)
                          -> AppResult<Vec<(Uuid, Decimal)>> {
    check_zone(zone)?;
    let rows = sqlx::query(
        "SELECT lot_id, qty FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone=$3 AND qty>0
         ORDER BY expiry_date ASC NULLS LAST, received_at ASC, lot_id ASC FOR UPDATE")
        .bind(item_type.as_str()).bind(item_id).bind(zone).fetch_all(&mut *tx).await?;
    let total: Decimal = rows.iter().map(|r| r.get::<Decimal, _>("qty")).sum();
    if total < qty {
        return Err(AppError::conflict_with("INSUFFICIENT_STOCK",
            format!("Insufficient stock in {zone}: have {}, need {}", total.normalize(), qty.normalize()),
            json!({ "item_type": item_type.as_str(), "item_id": item_id, "zone": zone, "available": total, "required": qty })));
    }
    let group = Uuid::new_v4();
    let mut left = qty;
    let mut taken = vec![];
    for r in rows {
        if left.is_zero() { break; }
        let lot: Uuid = r.get("lot_id");
        let have: Decimal = r.get("qty");
        let take = have.min(left);
        let mut m = Move::new(item_type, item_id, zone, lot, -take, txn_type).by(user).note(note);
        if let (Some(rt), Some(ri)) = (ref_type, ref_id) { m = m.reference(rt, ri); }
        post(&mut *tx, &m, group).await?;
        taken.push((lot, take));
        left -= take;
    }
    Ok(taken)
}

/// Total quantity of an item in a zone (all lots). Not locked – informational.
pub async fn available(db: &mut PgConnection, item_type: ItemType, item_id: Uuid, zone: &str) -> AppResult<Decimal> {
    let q: Option<Decimal> = sqlx::query_scalar("SELECT SUM(qty) FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone=$3")
        .bind(item_type.as_str()).bind(item_id).bind(zone).fetch_one(&mut *db).await?;
    Ok(q.unwrap_or_default())
}
