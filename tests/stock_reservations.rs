mod common;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType, Move};

/// product + approved batch with `qty` crates in FG_READY + a vehicle
async fn fixture(pool: &PgPool, qty: i64) -> (Uuid, Uuid, Uuid) {
    let p: Uuid = sqlx::query_scalar("INSERT INTO products (code,name,units_per_crate) VALUES ('P'||floor(random()*1e9)::text,'1L',12) RETURNING id").fetch_one(pool).await.unwrap();
    let b: Uuid = sqlx::query_scalar("INSERT INTO production_batches (batch_no,product_id,crates,qc_status,expiry_date) VALUES ('B'||floor(random()*1e9)::text,$1,$2,'APPROVED',current_date+100) RETURNING id")
        .bind(p).bind(qty as i32).fetch_one(pool).await.unwrap();
    let v: Uuid = sqlx::query_scalar("INSERT INTO vehicles (reg_no,capacity_crates) VALUES ('V'||floor(random()*1e9)::text,500) RETURNING id").fetch_one(pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Product, p, "FG_READY", b, rust_decimal::Decimal::from(qty), "QC_RELEASE")]).await.unwrap();
    tx.commit().await.unwrap();
    (p, b, v)
}
async fn reserve(pool: &PgPool, v: Uuid, p: Uuid, b: Uuid, crates: i32) {
    let t: Uuid = sqlx::query_scalar("INSERT INTO loading_trips (trip_no,vehicle_id,capacity_crates) VALUES ('T'||floor(random()*1e9)::text,$1,500) RETURNING id").bind(v).fetch_one(pool).await.unwrap();
    sqlx::query("INSERT INTO trip_items (trip_id,batch_id,product_id,crates) VALUES ($1,$2,$3,$4)").bind(t).bind(b).bind(p).bind(crates).execute(pool).await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn reserved_crates_are_not_free(pool: PgPool) {
    let (p, b, v) = fixture(&pool, 12).await;
    reserve(&pool, v, p, b, 10).await;
    let mut c = pool.acquire().await.unwrap();
    assert_eq!(stock::fg_reserved(&mut c, b).await.unwrap(), dec!(10));
    assert_eq!(stock::fg_free(&mut c, p, b).await.unwrap(), dec!(2));
    drop(c);
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(stock::allocate_free_fefo(&mut tx, p, 2).await.unwrap(), vec![(b, 2)]);
    let e = stock::allocate_free_fefo(&mut tx, p, 3).await.unwrap_err();
    assert!(matches!(e, water_erp::error::AppError::Conflict { code: "INSUFFICIENT_STOCK", .. }));
}

#[sqlx::test(migrations = "./migrations")]
async fn fefo_allocation_spans_batches_and_ignores_unapproved(pool: PgPool) {
    let (p, b1, v) = fixture(&pool, 5).await;
    // later-expiring approved batch + a quarantined batch (stock in QC_QUARANTINE must never be allocated)
    let b2: Uuid = sqlx::query_scalar("INSERT INTO production_batches (batch_no,product_id,crates,qc_status,expiry_date) VALUES ('B2'||floor(random()*1e9)::text,$1,8,'APPROVED',current_date+200) RETURNING id").bind(p).fetch_one(&pool).await.unwrap();
    let bq: Uuid = sqlx::query_scalar("INSERT INTO production_batches (batch_no,product_id,crates,qc_status,expiry_date) VALUES ('BQ'||floor(random()*1e9)::text,$1,50,'QUARANTINE',current_date+10) RETURNING id").bind(p).fetch_one(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![
        Move::new(ItemType::Product, p, "FG_READY", b2, dec!(8), "QC_RELEASE"),
        Move::new(ItemType::Product, p, "QC_QUARANTINE", bq, dec!(50), "BATCH_OUTPUT"),
    ]).await.unwrap();
    tx.commit().await.unwrap();
    reserve(&pool, v, p, b1, 2).await;
    let mut tx = pool.begin().await.unwrap();
    let a = stock::allocate_free_fefo(&mut tx, p, 9).await.unwrap();
    assert_eq!(a, vec![(b1, 3), (b2, 6)]);
    assert!(stock::allocate_free_fefo(&mut tx, p, 12).await.is_err(), "only 11 free; quarantine stock must not count");
}
