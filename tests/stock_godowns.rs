mod common;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType, Move, MAIN_LOCATION, TRANSIT_LOCATION};

async fn godown(pool: &PgPool, code: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO locations (code,name,kind) VALUES ($1,$1,'GODOWN') RETURNING id").bind(code).fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn balances_are_independent_per_godown(pool: PgPool) {
    let (g1, g2) = (godown(&pool, "G1").await, godown(&pool, "G2").await);
    let item = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![
        Move::new(ItemType::Raw, item, "RM_STORE", Uuid::nil(), dec!(10), "GRN"),                 // MAIN by default
        Move::new(ItemType::Raw, item, "RM_STORE", Uuid::nil(), dec!(7), "GRN").at(g1),
    ]).await.unwrap();
    assert_eq!(stock::available_at(&mut tx, MAIN_LOCATION, ItemType::Raw, item, "RM_STORE").await.unwrap(), dec!(10));
    assert_eq!(stock::available_at(&mut tx, g1, ItemType::Raw, item, "RM_STORE").await.unwrap(), dec!(7));
    assert_eq!(stock::available_at(&mut tx, g2, ItemType::Raw, item, "RM_STORE").await.unwrap(), dec!(0));
    // cannot take more than that godown holds even though MAIN has plenty
    let e = stock::post_all(&mut tx, vec![Move::new(ItemType::Raw, item, "RM_STORE", Uuid::nil(), dec!(-8), "ADJUST").at(g1)]).await.unwrap_err();
    assert!(matches!(e, water_erp::error::AppError::Conflict { code: "INSUFFICIENT_STOCK", .. }));
}

#[sqlx::test(migrations = "./migrations")]
async fn transit_hop_conserves_stock_and_ledger_matches(pool: PgPool) {
    let g1 = godown(&pool, "G1").await;
    let item = Uuid::new_v4();
    let lot = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Product, item, "FG_READY", lot, dec!(50), "QC_RELEASE")]).await.unwrap();
    // dispatch MAIN -> TRANSIT, then receive TRANSIT -> G1
    stock::transfer_between(&mut tx, ItemType::Product, item, lot, MAIN_LOCATION, "FG_READY", TRANSIT_LOCATION, "FG_READY", dec!(30), "GODOWN_TRANSFER_OUT", None, None, None, "").await.unwrap();
    stock::transfer_between(&mut tx, ItemType::Product, item, lot, TRANSIT_LOCATION, "FG_READY", g1, "FG_READY", dec!(30), "GODOWN_TRANSFER_IN", None, None, None, "").await.unwrap();
    tx.commit().await.unwrap();
    let rows: Vec<(Uuid, rust_decimal::Decimal)> = sqlx::query_as("SELECT location_id, qty FROM stock_balances WHERE item_id=$1 ORDER BY qty").bind(item).fetch_all(&pool).await.unwrap();
    let total: rust_decimal::Decimal = rows.iter().map(|r| r.1).sum();
    assert_eq!(total, dec!(50));
    assert!(rows.iter().any(|r| r.0 == g1 && r.1 == dec!(30)) && rows.iter().any(|r| r.0 == MAIN_LOCATION && r.1 == dec!(20)));
    // invariant per (location,item,zone,lot): balance == sum(ledger)
    let bad: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_balances b WHERE b.qty <> COALESCE((SELECT SUM(qty_delta) FROM stock_ledger l WHERE l.location_id=b.location_id AND l.item_type=b.item_type AND l.item_id=b.item_id AND l.zone=b.zone AND l.lot_id=b.lot_id),0)")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(bad, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn fefo_and_reservations_are_per_godown(pool: PgPool) {
    let g1 = godown(&pool, "G1").await;
    let p: Uuid = sqlx::query_scalar("INSERT INTO products (code,name,units_per_crate) VALUES ('P'||floor(random()*1e9)::text,'1L',12) RETURNING id").fetch_one(&pool).await.unwrap();
    let b: Uuid = sqlx::query_scalar("INSERT INTO production_batches (batch_no,product_id,crates,qc_status,expiry_date) VALUES ('B'||floor(random()*1e9)::text,$1,40,'APPROVED',current_date+90) RETURNING id").bind(p).fetch_one(&pool).await.unwrap();
    let v: Uuid = sqlx::query_scalar("INSERT INTO vehicles (reg_no,capacity_crates) VALUES ('V'||floor(random()*1e9)::text,500) RETURNING id").fetch_one(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![
        Move::new(ItemType::Product, p, "FG_READY", b, dec!(10), "QC_RELEASE"),
        Move::new(ItemType::Product, p, "FG_READY", b, dec!(30), "GODOWN_TRANSFER_IN").at(g1),
    ]).await.unwrap();
    tx.commit().await.unwrap();
    // a trip loading from G1 reserves only G1 stock
    let t: Uuid = sqlx::query_scalar("INSERT INTO loading_trips (trip_no,vehicle_id,capacity_crates,location_id) VALUES ('T'||floor(random()*1e9)::text,$1,500,$2) RETURNING id").bind(v).bind(g1).fetch_one(&pool).await.unwrap();
    sqlx::query("INSERT INTO trip_items (trip_id,batch_id,product_id,crates) VALUES ($1,$2,$3,25)").bind(t).bind(b).bind(p).execute(&pool).await.unwrap();
    let mut c = pool.acquire().await.unwrap();
    assert_eq!(stock::fg_free_at(&mut c, g1, p, b).await.unwrap(), dec!(5));
    assert_eq!(stock::fg_free_at(&mut c, MAIN_LOCATION, p, b).await.unwrap(), dec!(10));
    drop(c);
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(stock::allocate_free_fefo_at(&mut tx, MAIN_LOCATION, p, 10).await.unwrap(), vec![(b, 10)]);
    assert!(stock::allocate_free_fefo_at(&mut tx, g1, p, 6).await.is_err());
    assert_eq!(stock::allocate_free_fefo_at(&mut tx, g1, p, 5).await.unwrap(), vec![(b, 5)]);
}

#[sqlx::test(migrations = "./migrations")]
async fn user_godown_restriction(pool: PgPool) {
    let g1 = godown(&pool, "G1").await;
    let g2 = godown(&pool, "G2").await;
    let u = common::make_user(&pool, "store1", false, &["warehouse"], &[]).await;
    sqlx::query("UPDATE users SET allowed_location_ids = $2 WHERE id=$1").bind(u).bind(serde_json::json!([g1])).execute(&pool).await.unwrap();
    let st = common::state(pool.clone()).await;
    let key = common::session_for(&pool, u).await;
    let user = water_erp::auth::authenticate(&st, &key).await.unwrap();
    assert!(user.can_use_location(g1) && !user.can_use_location(g2));
    assert!(user.require_location(g2).is_err());
    let admin = common::make_user(&pool, "adm", true, &[], &[]).await;
    let akey = common::session_for(&pool, admin).await;
    assert!(water_erp::auth::authenticate(&st, &akey).await.unwrap().can_use_location(g2));
}
