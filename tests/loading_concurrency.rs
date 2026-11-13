mod loading_support;
use axum::http::StatusCode;
use loading_support::*;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

/// Simulates a phone whose add is committed *while* another phone is already waiting on the batch row.
/// Without the separate pre-lock statement the waiter would resume with a stale snapshot and over-allocate.
async fn race_against_open_reservation(pool: PgPool, by_product: bool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 12, "APPROVED", 90).await;
    let (t1, t2) = (c.trip(c.vehicle(100).await).await, c.trip(c.vehicle(100).await).await);
    // phone 1: open transaction that has locked the batch and reserved ALL 12 crates, not committed yet
    let mut raw = c.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM production_batches WHERE id=$1 FOR UPDATE").bind(b).fetch_one(&mut *raw).await.unwrap();
    sqlx::query("INSERT INTO trip_items (trip_id,batch_id,product_id,crates) VALUES ($1,$2,$3,12)").bind(t1).bind(b).bind(p).execute(&mut *raw).await.unwrap();
    // phone 2 asks for 1 crate and blocks on the batch lock
    let (app, key) = (c.app.clone(), c.key.clone());
    let body = if by_product { json!({ "product_id": p, "crates": 1 }) } else { json!({ "batch_id": b, "crates": 1 }) };
    let h = tokio::spawn(async move { common::call(&app, "POST", &format!("/api/loading/trips/{t2}/items"), Some(&key), Some(body)).await });
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!h.is_finished(), "the second adder must be waiting for the batch lock");
    raw.commit().await.unwrap();
    let (s, v) = h.await.unwrap();
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "stale-snapshot over-allocation: {v}");
    let reserved: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(crates),0)::bigint FROM trip_items WHERE batch_id=$1").bind(b).fetch_one(&c.pool).await.unwrap();
    assert_eq!(reserved, 12);
}

#[sqlx::test(migrations = "./migrations")]
async fn waiting_adder_by_batch_sees_the_committed_reservation(pool: PgPool) { race_against_open_reservation(pool, false).await }

#[sqlx::test(migrations = "./migrations")]
async fn waiting_adder_by_product_sees_the_committed_reservation(pool: PgPool) { race_against_open_reservation(pool, true).await }

async fn race_for_last_crates(pool: PgPool, by_product: bool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 5, "APPROVED", 90).await;
    let mut trips = vec![];
    for _ in 0..4 { trips.push(c.trip(c.vehicle(100).await).await); }
    let mut hs = vec![];
    for t in trips {
        let (app, key) = (c.app.clone(), c.key.clone());
        let body = if by_product { json!({ "product_id": p, "crates": 5 }) } else { json!({ "batch_id": b, "crates": 5 }) };
        hs.push(tokio::spawn(async move { common::call(&app, "POST", &format!("/api/loading/trips/{t}/items"), Some(&key), Some(body)).await }));
    }
    let mut ok = 0;
    for h in hs {
        let (s, v) = h.await.unwrap();
        if s == StatusCode::CREATED { ok += 1 } else { assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}") }
    }
    assert_eq!(ok, 1, "exactly one phone gets the last crates");
    let reserved: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(crates),0)::bigint FROM trip_items WHERE batch_id=$1").bind(b).fetch_one(&c.pool).await.unwrap();
    assert_eq!(reserved, 5);
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_adders_on_last_crates_by_batch(pool: PgPool) { race_for_last_crates(pool, false).await }

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_adders_on_last_crates_by_product(pool: PgPool) { race_for_last_crates(pool, true).await }

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_gate_pass_double_submit_deducts_once(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 20, "APPROVED", 90).await;
    let t = c.trip(c.vehicle(100).await).await;
    assert_eq!(c.add(t, json!({ "batch_id": b, "crates": 8 })).await.0, StatusCode::CREATED);
    let mut hs = vec![];
    for _ in 0..4 {
        let (app, key) = (c.app.clone(), c.key.clone());
        hs.push(tokio::spawn(async move { common::call(&app, "POST", &format!("/api/loading/trips/{t}/gate-pass"), Some(&key), Some(json!({}))).await }));
    }
    let mut res: Vec<(StatusCode, Value)> = vec![];
    for h in hs { res.push(h.await.unwrap()); }
    assert_eq!(res.iter().filter(|r| r.0 == StatusCode::OK).count(), 1, "{res:?}");
    for (s, v) in res.iter().filter(|r| r.0 != StatusCode::OK) { assert_eq!((*s, code(v)), (StatusCode::CONFLICT, "INVALID_STATE"), "{v}"); }
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(12));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_ledger WHERE txn_type='GATE_PASS' AND ref_id=$1").bind(t).fetch_one(&c.pool).await.unwrap();
    assert_eq!(n, 1);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM loading_trips WHERE gate_pass_no IS NOT NULL").fetch_one(&c.pool).await.unwrap();
    assert_eq!(n, 1);
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_settle_double_submit_restocks_once(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 20, "APPROVED", 90).await;
    let t = c.trip(c.vehicle(100).await).await;
    assert_eq!(c.add(t, json!({ "batch_id": b, "crates": 10 })).await.0, StatusCode::CREATED);
    assert_eq!(c.gate(t).await.0, StatusCode::OK);
    let (_, trip) = c.get(&format!("/api/loading/trips/{t}")).await;
    let item = trip["items"][0]["id"].clone();
    let mut hs = vec![];
    for _ in 0..3 {
        let (app, key, item) = (c.app.clone(), c.key.clone(), item.clone());
        hs.push(tokio::spawn(async move {
            common::call(&app, "POST", &format!("/api/loading/trips/{t}/settle"), Some(&key),
                Some(json!({ "items": [{ "item_id": item, "delivered": 6, "returned": 4, "damaged": 0 }] }))).await
        }));
    }
    let mut ok = 0;
    for h in hs { let (s, v) = h.await.unwrap(); if s == StatusCode::OK { ok += 1 } else { assert_eq!(code(&v), "INVALID_STATE", "{v}") } }
    assert_eq!(ok, 1);
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(14));
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn gate_pass_and_dispatch_cannot_both_take_the_last_crates(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 6, "APPROVED", 90).await;
    let t = c.trip(c.vehicle(100).await).await;
    // a trip holds all 6; a walk-in dispatch must not be able to take them, before or after the gate pass
    assert_eq!(c.add(t, json!({ "batch_id": b, "crates": 6 })).await.0, StatusCode::CREATED);
    let dn = json!({ "party_name": "Walk-in", "lines": [{ "batch_id": b, "crates": 1 }] });
    let (s, v) = c.post("/api/loading/dispatches", dn.clone()).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}");
    assert_eq!(c.gate(t).await.0, StatusCode::OK);
    let (s, v) = c.post("/api/loading/dispatches", dn).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}");
    let _ = Uuid::nil();
}
