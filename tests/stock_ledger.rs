mod common;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType, Move};

#[sqlx::test(migrations = "./migrations")]
async fn ledger_balances_and_non_negative(pool: PgPool) {
    let item = Uuid::new_v4();
    let lot = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Raw, item, "RM_STORE", lot, dec!(100), "GRN")]).await.unwrap();
    stock::transfer(&mut tx, ItemType::Raw, item, lot, "RM_STORE", "BLOW_FLOOR", dec!(40), "TRANSFER", None, None, None, "").await.unwrap();
    assert_eq!(stock::available(&mut tx, ItemType::Raw, item, "RM_STORE").await.unwrap(), dec!(60));
    let err = stock::post_all(&mut tx, vec![Move::new(ItemType::Raw, item, "RM_STORE", lot, dec!(-61), "ADJUST")]).await.unwrap_err();
    assert!(matches!(err, water_erp::error::AppError::Conflict { code: "INSUFFICIENT_STOCK", .. }));
}

#[sqlx::test(migrations = "./migrations")]
async fn ledger_is_append_only(pool: PgPool) {
    let item = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Raw, item, "RM_STORE", Uuid::nil(), dec!(5), "GRN")]).await.unwrap();
    tx.commit().await.unwrap();
    assert!(sqlx::query("UPDATE stock_ledger SET note='x'").execute(&pool).await.is_err());
    assert!(sqlx::query("DELETE FROM stock_ledger").execute(&pool).await.is_err());
}

#[sqlx::test(migrations = "./migrations")]
async fn fefo_takes_earliest_expiry_first(pool: PgPool) {
    let item = Uuid::new_v4();
    let (l1, l2) = (Uuid::new_v4(), Uuid::new_v4());
    let mut tx = pool.begin().await.unwrap();
    let d = |s: &str| Some(chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap());
    stock::post_all(&mut tx, vec![
        Move::new(ItemType::Product, item, "FG_READY", l1, dec!(10), "BATCH_OUTPUT").expiry(d("2027-06-01")),
        Move::new(ItemType::Product, item, "FG_READY", l2, dec!(10), "BATCH_OUTPUT").expiry(d("2027-01-01")),
    ]).await.unwrap();
    let taken = stock::consume_fefo(&mut tx, ItemType::Product, item, "FG_READY", dec!(15), "GATE_PASS", None, None, None, "").await.unwrap();
    assert_eq!(taken, vec![(l2, dec!(10)), (l1, dec!(5))]);
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_deductions_never_go_negative(pool: PgPool) {
    let item = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Product, item, "FG_READY", Uuid::nil(), dec!(10), "ADJUST")]).await.unwrap();
    tx.commit().await.unwrap();
    let mut hs = vec![];
    for _ in 0..20 {
        let p = pool.clone();
        hs.push(tokio::spawn(async move {
            let mut tx = p.begin().await.unwrap();
            match stock::post_all(&mut tx, vec![Move::new(ItemType::Product, item, "FG_READY", Uuid::nil(), dec!(-1), "GATE_PASS")]).await {
                Ok(_) => { tx.commit().await.unwrap(); true }
                Err(_) => false,
            }
        }));
    }
    let mut ok = 0;
    for h in hs { if h.await.unwrap() { ok += 1; } }
    assert_eq!(ok, 10);
    let q: rust_decimal::Decimal = sqlx::query_scalar("SELECT qty FROM stock_balances WHERE item_id=$1").bind(item).fetch_one(&pool).await.unwrap();
    assert_eq!(q, dec!(0));
}
