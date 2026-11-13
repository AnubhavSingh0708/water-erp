mod common;
use axum::{http::StatusCode, Router};
use common::{app, call, make_user, session_for};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType, Move, MAIN_LOCATION, NIL_LOT};

struct Fx { app: Router, key: String, product: Uuid, g1: Uuid, g2: Uuid }

async fn godown(pool: &PgPool, code: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO locations (code,name,kind) VALUES ($1,$1,'GODOWN') RETURNING id").bind(code).fetch_one(pool).await.unwrap()
}
async fn fx(pool: PgPool) -> Fx {
    let (app, _) = app(pool.clone()).await;
    let u = make_user(&pool, "jarsuser", false, &["jars"], &[]).await;
    let key = session_for(&pool, u).await;
    let product: Uuid = sqlx::query_scalar("INSERT INTO products (code,name,kind,units_per_crate,max_cycles) VALUES ('J1','20L Jar','RETURNABLE',1,50) RETURNING id").fetch_one(&pool).await.unwrap();
    Fx { app, key, product, g1: godown(&pool, "G1").await, g2: godown(&pool, "G2").await }
}
async fn put(pool: &PgPool, p: Uuid, loc: Uuid, zone: &'static str, lot: Uuid, qty: i64) {
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Product, p, zone, lot, Decimal::from(qty), "ADJUST").at(loc)]).await.unwrap();
    tx.commit().await.unwrap();
}
async fn qty(pool: &PgPool, p: Uuid, loc: Uuid, zone: &str) -> i64 {
    let d: Decimal = sqlx::query_scalar("SELECT COALESCE(SUM(qty),0) FROM stock_balances WHERE item_id=$1 AND zone=$2 AND location_id=$3").bind(p).bind(zone).bind(loc).fetch_one(pool).await.unwrap();
    d.to_string().split('.').next().unwrap().parse().unwrap()
}
async fn post(f: &Fx, path: &str, body: Value) -> (StatusCode, Value) { call(&f.app, "POST", path, Some(&f.key), Some(body)).await }
async fn invariant(pool: &PgPool) {
    let bad: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_balances b WHERE b.qty <> COALESCE((SELECT SUM(qty_delta) FROM stock_ledger l WHERE l.location_id=b.location_id AND l.item_type=b.item_type AND l.item_id=b.item_id AND l.zone=b.zone AND l.lot_id=b.lot_id),0)")
        .fetch_one(pool).await.unwrap();
    assert_eq!(bad, 0);
}
async fn batch(pool: &PgPool, p: Uuid, loc: Uuid, crates: i32) -> Uuid {
    let b: Uuid = sqlx::query_scalar("INSERT INTO production_batches (batch_no,product_id,crates,qc_status,expiry_date,location_id) VALUES ('B'||floor(random()*1e9)::text,$1,$2,'APPROVED',current_date+100,$3) RETURNING id")
        .bind(p).bind(crates).bind(loc).fetch_one(pool).await.unwrap();
    put(pool, p, loc, "FG_READY", b, crates as i64).await;
    b
}
async fn reserve(pool: &PgPool, loc: Uuid, p: Uuid, b: Uuid, crates: i32) {
    let v: Uuid = sqlx::query_scalar("INSERT INTO vehicles (reg_no,capacity_crates) VALUES ('V'||floor(random()*1e9)::text,500) RETURNING id").fetch_one(pool).await.unwrap();
    let t: Uuid = sqlx::query_scalar("INSERT INTO loading_trips (trip_no,vehicle_id,capacity_crates,location_id) VALUES ('T'||floor(random()*1e9)::text,$1,500,$2) RETURNING id").bind(v).bind(loc).fetch_one(pool).await.unwrap();
    sqlx::query("INSERT INTO trip_items (trip_id,batch_id,product_id,crates) VALUES ($1,$2,$3,$4)").bind(t).bind(b).bind(p).bind(crates).execute(pool).await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn receive_wash_destroy_are_isolated_per_godown(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let p = f.product;
    let (s, v) = post(&f, "/api/jars/receive-new", json!({"product_id": p, "qty": 20, "location_id": f.g1})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["effects"][0]["location_id"], f.g1.to_string());
    post(&f, "/api/jars/receive-new", json!({"product_id": p, "qty": 5})).await; // omitted => MAIN
    assert_eq!((qty(&pool, p, f.g1, "JAR_EMPTY").await, qty(&pool, p, MAIN_LOCATION, "JAR_EMPTY").await), (20, 5));
    // wash in G1 does not touch MAIN, and MAIN cannot wash more than it holds although G1 has plenty
    let (s, _) = post(&f, "/api/jars/wash", json!({"product_id": p, "qty": 10, "location_id": f.g1})).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!((qty(&pool, p, f.g1, "JAR_EMPTY").await, qty(&pool, p, f.g1, "JAR_CLEAN").await, qty(&pool, p, MAIN_LOCATION, "JAR_EMPTY").await), (10, 10, 5));
    let (s, v) = post(&f, "/api/jars/wash", json!({"product_id": p, "qty": 6})).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("INSUFFICIENT_STOCK")), "{v}");
    // destroy: G2 has nothing, G1 has clean jars
    let d = json!({"product_id": p, "qty": 3, "location": "PLANT_CLEAN", "reason_code": "CRACKED", "note": "cracked ones", "confirm": true});
    let mut d2 = d.clone();
    d2["location_id"] = json!(f.g2);
    let (s, _) = post(&f, "/api/jars/destroy", d2).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let mut d1 = d.clone();
    d1["location_id"] = json!(f.g1);
    let (s, v) = post(&f, "/api/jars/destroy", d1).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((qty(&pool, p, f.g1, "JAR_CLEAN").await, qty(&pool, p, f.g1, "SCRAP_YARD").await, qty(&pool, p, MAIN_LOCATION, "SCRAP_YARD").await), (7, 3, 0));
    // events carry their godown
    let locs: Vec<(String, Option<Uuid>)> = sqlx::query_as("SELECT event_type, location_id FROM jar_fleet_events ORDER BY created_at").fetch_all(&pool).await.unwrap();
    assert_eq!(locs.len(), 4);
    assert!(locs.iter().all(|l| l.1.is_some()));
    assert_eq!(locs.iter().filter(|l| l.1 == Some(f.g1)).count(), 3);
    assert_eq!(locs.iter().filter(|l| l.1 == Some(MAIN_LOCATION)).count(), 1);
    // unknown / transit godowns are rejected
    let (s, _) = post(&f, "/api/jars/receive-new", json!({"product_id": p, "qty": 1, "location_id": Uuid::new_v4()})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = post(&f, "/api/jars/receive-new", json!({"product_id": p, "qty": 1, "location_id": stock::TRANSIT_LOCATION})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    invariant(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn counter_issue_and_reservations_per_godown(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let p = f.product;
    let c: Uuid = sqlx::query_scalar("INSERT INTO customers (code,name) VALUES ('C1','Cafe') RETURNING id").fetch_one(&pool).await.unwrap();
    let b_main = batch(&pool, p, MAIN_LOCATION, 12).await;
    let b_g1 = batch(&pool, p, f.g1, 5).await;
    reserve(&pool, MAIN_LOCATION, p, b_main, 10).await;   // 2 free at MAIN
    reserve(&pool, f.g1, p, b_g1, 3).await;               // 2 free at G1
    let issue = |loc: Option<Uuid>, n: i64| {
        let mut b = json!({"customer_id": c, "product_id": p, "qty": n});
        if let Some(l) = loc { b["location_id"] = json!(l); }
        post(&f, "/api/jars/issue", b)
    };
    let (s, v) = issue(Some(f.g1), 3).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("INSUFFICIENT_STOCK")), "{v}");
    assert_eq!(v["error"]["details"]["available"], "2.0000");
    let (s, v) = issue(None, 3).await;
    assert_eq!(s, StatusCode::CONFLICT, "MAIN has 2 free, G1's stock is not available here: {v}");
    let (s, v) = issue(Some(f.g1), 2).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((qty(&pool, p, f.g1, "FG_READY").await, qty(&pool, p, MAIN_LOCATION, "FG_READY").await), (3, 12));
    let (s, _) = issue(None, 2).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!((qty(&pool, p, f.g1, "FG_READY").await, qty(&pool, p, MAIN_LOCATION, "FG_READY").await), (3, 10));
    let (s, _) = issue(Some(f.g2), 1).await;
    assert_eq!(s, StatusCode::CONFLICT, "empty godown");
    // customer holdings are global
    let h: i64 = sqlx::query_scalar("SELECT SUM(delta)::bigint FROM jar_ledger WHERE customer_id=$1").bind(c).fetch_one(&pool).await.unwrap();
    assert_eq!(h, 4);
    // receive-empties lands in the chosen godown
    let (s, v) = post(&f, "/api/jars/receive-empties", json!({"customer_id": c, "product_id": p, "qty_good": 3, "qty_damaged": 0, "location_id": f.g2})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((qty(&pool, p, f.g2, "JAR_EMPTY").await, qty(&pool, p, MAIN_LOCATION, "JAR_EMPTY").await), (3, 0));
    invariant(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn summary_and_fleet_by_location_and_restricted_users(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let p = f.product;
    put(&pool, p, MAIN_LOCATION, "JAR_EMPTY", NIL_LOT, 5).await;
    put(&pool, p, f.g1, "JAR_EMPTY", NIL_LOT, 7).await;
    put(&pool, p, f.g1, "JAR_CLEAN", NIL_LOT, 4).await;
    put(&pool, p, f.g2, "JAR_CLEAN", NIL_LOT, 9).await;
    let get = |key: String, path: String| { let app = f.app.clone(); async move { call(&app, "GET", &path, Some(&key), None).await } };

    let (_, v) = get(f.key.clone(), format!("/api/jars/fleet?product_id={p}&location_id={}", f.g1)).await;
    assert_eq!((v["fleet"]["plant_empty"].clone(), v["fleet"]["plant_clean"].clone(), v["fleet"]["total"].clone()), (json!(7), json!(4), json!(11)));
    assert_eq!(v["by_location"].as_array().unwrap().len(), 3);
    let g2row = v["by_location"].as_array().unwrap().iter().find(|r| r["location_id"] == f.g2.to_string()).unwrap().clone();
    assert_eq!(g2row["plant_clean"], 9);
    let (_, v) = get(f.key.clone(), format!("/api/jars/fleet?product_id={p}&location_id=all")).await;
    assert_eq!(v["fleet"]["total"], 25);
    assert_eq!(v["location_id"], "all");
    let (_, v) = get(f.key.clone(), "/api/jars/summary?location_id=all".into()).await;
    assert_eq!((v[0]["plant_empty"].clone(), v[0]["fleet_total"].clone()), (json!(12), json!(25)));
    let (_, v) = get(f.key.clone(), "/api/jars/summary".into()).await;
    assert_eq!(v[0]["plant_empty"], 5, "omitted => MAIN");
    let (s, _) = get(f.key.clone(), "/api/jars/summary?location_id=nope".into()).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // a user restricted to G1
    let r = make_user(&pool, "g1only", false, &["jars"], &[]).await;
    sqlx::query("UPDATE users SET allowed_location_ids = $2 WHERE id=$1").bind(r).bind(json!([f.g1.to_string()])).execute(&pool).await.unwrap();
    let kr = session_for(&pool, r).await;
    let (s, _) = get(kr.clone(), format!("/api/jars/fleet?product_id={p}&location_id={}", f.g2)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = get(kr.clone(), format!("/api/jars/summary?location_id={MAIN_LOCATION}")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = get(kr.clone(), "/api/jars/summary?location_id=all".into()).await;
    assert_eq!((s, v[0]["plant_empty"].clone(), v[0]["plant_clean"].clone()), (StatusCode::OK, json!(7), json!(4)));
    let (_, v) = get(kr.clone(), format!("/api/jars/fleet?product_id={p}")).await;
    assert_eq!(v["fleet"]["total"], 11, "omitted => the only allowed godown");
    assert_eq!(v["by_location"].as_array().unwrap().len(), 1, "foreign godowns are silently filtered");
    // writes: foreign godown 403, omitted location resolves to the only allowed one
    let w = |loc: Option<Uuid>| { let mut b = json!({"product_id": p, "qty": 2}); if let Some(l) = loc { b["location_id"] = json!(l); } b };
    let (s, _) = call(&f.app, "POST", "/api/jars/wash", Some(&kr), Some(w(Some(MAIN_LOCATION)))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = call(&f.app, "POST", "/api/jars/wash", Some(&kr), Some(w(None))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((qty(&pool, p, f.g1, "JAR_EMPTY").await, qty(&pool, p, f.g1, "JAR_CLEAN").await, qty(&pool, p, MAIN_LOCATION, "JAR_EMPTY").await), (5, 6, 5));
    invariant(&pool).await;
}
