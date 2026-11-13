mod common;
use axum::{http::StatusCode, Router};
use chrono::NaiveDate;
use common::{app, call, make_user, session_for};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType, Move, NIL_LOT};

struct Fx { app: Router, key: String, product: Uuid, customer: Uuid, today: NaiveDate }

async fn today(pool: &PgPool) -> NaiveDate {
    let mut c = pool.acquire().await.unwrap();
    water_erp::util::plant_today(&mut c).await.unwrap()
}
async fn returnable(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO products (code,name,kind,units_per_crate,max_cycles) VALUES ('J'||floor(random()*1e9)::text,$1,'RETURNABLE',1,50) RETURNING id").bind(name).fetch_one(pool).await.unwrap()
}
async fn customer(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO customers (code,name,phone) VALUES ('C'||floor(random()*1e9)::text,$1,'555') RETURNING id").bind(name).fetch_one(pool).await.unwrap()
}
async fn fx(pool: PgPool) -> Fx {
    let (app, _) = app(pool.clone()).await;
    let u = make_user(&pool, "jarsuser", false, &["jars"], &[]).await;
    let key = session_for(&pool, u).await;
    Fx { app, key, product: returnable(&pool, "20L Jar").await, customer: customer(&pool, "Acme Cafe").await, today: today(&pool).await }
}
async fn put_stock(pool: &PgPool, p: Uuid, zone: &'static str, lot: Uuid, qty: i64) {
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Product, p, zone, lot, Decimal::from(qty), "ADJUST")]).await.unwrap();
    tx.commit().await.unwrap();
}
async fn zone(pool: &PgPool, p: Uuid, zone: &str) -> i64 {
    let d: Decimal = sqlx::query_scalar("SELECT COALESCE(SUM(qty),0) FROM stock_balances WHERE item_id=$1 AND zone=$2").bind(p).bind(zone).fetch_one(pool).await.unwrap();
    d.to_string().split('.').next().unwrap().parse().unwrap()
}
async fn holding(pool: &PgPool, c: Uuid, p: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(SUM(delta),0)::bigint FROM jar_ledger WHERE customer_id=$1 AND product_id=$2").bind(c).bind(p).fetch_one(pool).await.unwrap()
}
async fn ledger_row(pool: &PgPool, c: Uuid, p: Uuid, t: &str, delta: i32, date: NaiveDate) {
    sqlx::query("INSERT INTO jar_ledger (customer_id,product_id,txn_type,delta,txn_date) VALUES ($1,$2,$3,$4,$5)").bind(c).bind(p).bind(t).bind(delta).bind(date).execute(pool).await.unwrap();
}
async fn approved_batch(pool: &PgPool, p: Uuid, crates: i32, in_zone: &'static str, qc: &str, expiry_days: i32) -> Uuid {
    let b: Uuid = sqlx::query_scalar("INSERT INTO production_batches (batch_no,product_id,crates,qc_status,expiry_date) VALUES ('B'||floor(random()*1e9)::text,$1,$2,$3,current_date+$4) RETURNING id")
        .bind(p).bind(crates).bind(qc).bind(expiry_days).fetch_one(pool).await.unwrap();
    put_stock(pool, p, in_zone, b, crates as i64).await;
    b
}
async fn event_count(pool: &PgPool, p: Uuid, ty: &str, reason: Option<&str>) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(SUM(qty),0)::bigint FROM jar_fleet_events WHERE product_id=$1 AND event_type=$2 AND ($3::text IS NULL OR reason_code=$3)").bind(p).bind(ty).bind(reason).fetch_one(pool).await.unwrap()
}
/// stock_balances.qty must equal the sum of its ledger deltas for every (item, zone, lot)
async fn assert_ledger_invariant(pool: &PgPool) {
    let bad: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM stock_balances b WHERE b.qty <> COALESCE((SELECT SUM(l.qty_delta) FROM stock_ledger l
           WHERE l.item_type=b.item_type AND l.item_id=b.item_id AND l.zone=b.zone AND l.lot_id=b.lot_id),0)").fetch_one(pool).await.unwrap();
    assert_eq!(bad, 0, "stock_balances diverged from the ledger");
}
async fn post(f: &Fx, path: &str, body: Value) -> (StatusCode, Value) { call(&f.app, "POST", path, Some(&f.key), Some(body)).await }

#[sqlx::test(migrations = "./migrations")]
async fn bottled_products_are_rejected_everywhere(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let bottled: Uuid = sqlx::query_scalar("INSERT INTO products (code,name,kind,units_per_crate) VALUES ('B1','500ml','BOTTLED',24) RETURNING id").fetch_one(&pool).await.unwrap();
    let c = f.customer;
    for (path, body) in [
        ("/api/jars/opening", json!({"customer_id": c, "product_id": bottled, "qty": 1})),
        ("/api/jars/issue", json!({"customer_id": c, "product_id": bottled, "qty": 1})),
        ("/api/jars/receive-empties", json!({"customer_id": c, "product_id": bottled, "qty_good": 1})),
        ("/api/jars/receive-new", json!({"product_id": bottled, "qty": 1})),
        ("/api/jars/wash", json!({"product_id": bottled, "qty": 1})),
        ("/api/jars/destroy", json!({"product_id": bottled, "qty": 1, "location": "PLANT_EMPTY", "reason_code": "OTHER", "note": "testing", "confirm": true})),
    ] {
        let (s, v) = post(&f, path, body).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{path}: {v}");
    }
    let (s, _) = call(&f.app, "GET", &format!("/api/jars/fleet?product_id={bottled}"), Some(&f.key), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // summary lists only returnables
    let (_, v) = call(&f.app, "GET", "/api/jars/summary", Some(&f.key), None).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    // unknown product id is a plain 404
    let (s, _) = post(&f, "/api/jars/receive-new", json!({"product_id": Uuid::new_v4(), "qty": 1})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "./migrations")]
async fn page_guard_and_input_validation(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let other = make_user(&pool, "qcguy", false, &["qc", "maintenance"], &[]).await;
    let ko = session_for(&pool, other).await;
    for (m, path) in [("GET", "/api/jars/summary"), ("GET", "/api/jars/customers"), ("GET", "/api/jars/destroyed"), ("POST", "/api/jars/wash"), ("GET", "/api/jars/fleet")] {
        let (s, _) = call(&f.app, m, path, Some(&ko), if m == "POST" { Some(json!({})) } else { None }).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
    }
    let (s, _) = call(&f.app, "GET", "/api/jars/summary", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let admin = make_user(&pool, "boss", true, &[], &[]).await;
    let (s, _) = call(&f.app, "GET", "/api/jars/summary", Some(&session_for(&pool, admin).await), None).await;
    assert_eq!(s, StatusCode::OK);
    // validation
    let p = f.product;
    for body in [json!({"product_id": p, "qty": 0}), json!({"product_id": p, "qty": -3}), json!({"product_id": p, "qty": 1.5}), json!({"product_id": p, "qty": 2000000}), json!({"product_id": p, "qty": 1, "extra": 1})] {
        let (s, v) = post(&f, "/api/jars/receive-new", body.clone()).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {v}");
    }
    let (s, _) = call(&f.app, "POST", "/api/jars/wash", Some(&f.key), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "no body");
    let (s, v) = post(&f, "/api/jars/destroy", json!({"product_id": p, "qty": 1, "location": "MOON", "reason_code": "OTHER", "note": "xxxxx", "confirm": true})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    let (s, _) = post(&f, "/api/jars/destroy", json!({"product_id": p, "qty": 1, "location": "PLANT_EMPTY", "reason_code": "OTHER", "note": "abc", "confirm": true})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "short note");
    let (s, _) = post(&f, "/api/jars/destroy", json!({"product_id": p, "qty": 1, "location": "PLANT_EMPTY", "customer_id": f.customer, "reason_code": "OTHER", "note": "abcdef", "confirm": true})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "customer not allowed for plant locations");
    let (s, _) = post(&f, "/api/jars/destroy", json!({"product_id": p, "qty": 1, "location": "CUSTOMER", "reason_code": "LOST", "note": "abcdef", "confirm": true})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "customer required");
    // SQL injection strings in filters
    for q in ["q=%27%3B%20DROP%20TABLE%20jar_ledger%3B--", "q=%25", "q=_", "product_id=1%27%20OR%20%271", "overdue=1%27"] {
        let (s, _) = call(&f.app, "GET", &format!("/api/jars/customers?{q}"), Some(&f.key), None).await;
        assert!(s == StatusCode::OK || s == StatusCode::UNPROCESSABLE_ENTITY, "{q}: {s}");
    }
    let (s, _) = call(&f.app, "GET", "/api/jars/destroyed?reason=X%27%20OR%201=1", Some(&f.key), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jar_ledger").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn fifo_oldest_outstanding_and_overdue(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let d = |n: i64| f.today - chrono::Duration::days(n);
    let c2 = customer(&pool, "Zed Hotel").await;
    ledger_row(&pool, f.customer, f.product, "OPENING", 10, d(60)).await;
    ledger_row(&pool, f.customer, f.product, "ISSUE_FILLED", 5, d(20)).await;
    ledger_row(&pool, f.customer, f.product, "RETURN_EMPTY", -8, d(10)).await;   // consumes opening layer: 2 left from d(60)
    ledger_row(&pool, c2, f.product, "ISSUE_FILLED", 4, d(5)).await;
    let (s, v) = call(&f.app, "GET", &format!("/api/jars/customers?product_id={}", f.product), Some(&f.key), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "Acme Cafe", "overdue first");
    assert_eq!(rows[0]["holding"], 7);
    assert_eq!(rows[0]["oldest_outstanding_date"], d(60).to_string());
    assert_eq!(rows[0]["days_outstanding"], 60);
    assert_eq!(rows[0]["overdue"], true);
    assert_eq!(rows[1]["name"], "Zed Hotel");
    assert_eq!((rows[1]["holding"].clone(), rows[1]["overdue"].clone(), rows[1]["days_outstanding"].clone()), (json!(4), json!(false), json!(5)));
    // two more returned: the d(60) layer is used up, the oldest outstanding is now the d(20) issue (not overdue at 30 days)
    ledger_row(&pool, f.customer, f.product, "RETURN_EMPTY", -2, d(1)).await;
    let (_, v) = call(&f.app, "GET", "/api/jars/customers?overdue=1", Some(&f.key), None).await;
    assert_eq!(v.as_array().unwrap().len(), 0);
    let (_, v) = call(&f.app, "GET", "/api/jars/customers?q=acme", Some(&f.key), None).await;
    assert_eq!(v[0]["oldest_outstanding_date"], d(20).to_string());
    assert_eq!(v[0]["holding"], 5);
    // summary counts overdue customers; ledger shows running balance
    ledger_row(&pool, c2, f.product, "ISSUE_FILLED", 1, d(45)).await; // back-dated issue makes Zed overdue
    let (_, v) = call(&f.app, "GET", "/api/jars/summary", Some(&f.key), None).await;
    assert_eq!((v[0]["with_customers"].clone(), v[0]["overdue_customers"].clone()), (json!(10), json!(1)));
    let (s, v) = call(&f.app, "GET", &format!("/api/jars/customers/{}/ledger?product_id={}", f.customer, f.product), Some(&f.key), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["total"], 4);
    assert_eq!(v["items"][0]["balance_after"], 5);
    assert_eq!(v["items"][3]["balance_after"], 10);
    // holding zero customers are hidden unless all=1
    ledger_row(&pool, c2, f.product, "RETURN_EMPTY", -5, d(0)).await;
    let (_, v) = call(&f.app, "GET", "/api/jars/customers?q=zed", Some(&f.key), None).await;
    assert_eq!(v.as_array().unwrap().len(), 0);
    let (_, v) = call(&f.app, "GET", "/api/jars/customers?q=zed&all=1", Some(&f.key), None).await;
    assert_eq!((v[0]["holding"].clone(), v[0]["overdue"].clone(), v[0]["oldest_outstanding_date"].clone()), (json!(0), json!(false), Value::Null));
}

#[sqlx::test(migrations = "./migrations")]
async fn counter_issue_respects_reservations_and_qc(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let b = approved_batch(&pool, f.product, 12, "FG_READY", "APPROVED", 100).await;
    let _q = approved_batch(&pool, f.product, 50, "QC_QUARANTINE", "QUARANTINE", 10).await;
    let vehicle: Uuid = sqlx::query_scalar("INSERT INTO vehicles (reg_no,capacity_crates) VALUES ('V1',500) RETURNING id").fetch_one(&pool).await.unwrap();
    let trip: Uuid = sqlx::query_scalar("INSERT INTO loading_trips (trip_no,vehicle_id,capacity_crates) VALUES ('T1',$1,500) RETURNING id").bind(vehicle).fetch_one(&pool).await.unwrap();
    sqlx::query("INSERT INTO trip_items (trip_id,batch_id,product_id,crates) VALUES ($1,$2,$3,10)").bind(trip).bind(b).bind(f.product).execute(&pool).await.unwrap();
    // 12 in FG_READY, 10 reserved => only 2 free
    let (s, v) = post(&f, "/api/jars/issue", json!({"customer_id": f.customer, "product_id": f.product, "qty": 3})).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("INSUFFICIENT_STOCK")), "{v}");
    assert_eq!(v["error"]["details"]["available"], "2.0000");
    assert_eq!(zone(&pool, f.product, "FG_READY").await, 12);
    assert_eq!(holding(&pool, f.customer, f.product).await, 0);
    // dry run changes nothing
    let (s, v) = call(&f.app, "POST", "/api/jars/issue?dry_run=1", Some(&f.key), Some(json!({"customer_id": f.customer, "product_id": f.product, "qty": 2}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["effects"][0]["zone"], "FG_READY");
    assert_eq!(v["effects"][0]["delta"], "-2.0000");
    assert_eq!(v["effects"][0]["after"], "10.0000");
    assert_eq!(zone(&pool, f.product, "FG_READY").await, 12);
    assert_eq!(holding(&pool, f.customer, f.product).await, 0);
    // real issue
    let (s, v) = post(&f, "/api/jars/issue", json!({"customer_id": f.customer, "product_id": f.product, "qty": 2})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["holding"], 2);
    assert_eq!(zone(&pool, f.product, "FG_READY").await, 10);
    assert_eq!(holding(&pool, f.customer, f.product).await, 2);
    assert_eq!(zone(&pool, f.product, "QC_QUARANTINE").await, 50, "quarantined stock is untouched");
    let t: String = sqlx::query_scalar("SELECT txn_type FROM stock_ledger WHERE zone='FG_READY' AND qty_delta < 0").fetch_one(&pool).await.unwrap();
    assert_eq!(t, "JAR_ISSUE");
    let (s, _) = post(&f, "/api/jars/issue", json!({"customer_id": f.customer, "product_id": f.product, "qty": 1})).await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing free any more");
    // inactive customer
    sqlx::query("UPDATE customers SET is_active=false WHERE id=$1").bind(f.customer).execute(&pool).await.unwrap();
    let (s, _) = post(&f, "/api/jars/issue", json!({"customer_id": f.customer, "product_id": f.product, "qty": 1})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_ledger_invariant(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn issue_to_overdue_customer_needs_confirmation(pool: PgPool) {
    let f = fx(pool.clone()).await;
    approved_batch(&pool, f.product, 10, "FG_READY", "APPROVED", 100).await;
    ledger_row(&pool, f.customer, f.product, "OPENING", 3, f.today - chrono::Duration::days(90)).await;
    let (s, v) = post(&f, "/api/jars/issue", json!({"customer_id": f.customer, "product_id": f.product, "qty": 1})).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("CONFIRM_REQUIRED")));
    assert!(v["error"]["details"]["warnings"][0].as_str().unwrap().contains("90 days"));
    let (s, v) = post(&f, "/api/jars/issue", json!({"customer_id": f.customer, "product_id": f.product, "qty": 1, "confirm": true})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["holding"], 4);
}

#[sqlx::test(migrations = "./migrations")]
async fn receive_empties_flow_and_holding_never_negative(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let (s, v) = post(&f, "/api/jars/opening", json!({"customer_id": f.customer, "product_id": f.product, "qty": 6, "as_of": (f.today - chrono::Duration::days(3)).to_string()})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, _) = post(&f, "/api/jars/opening", json!({"customer_id": f.customer, "product_id": f.product, "qty": 6, "as_of": (f.today + chrono::Duration::days(3)).to_string()})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "future date");
    // second opening for the same pair asks for confirmation
    let (s, v) = post(&f, "/api/jars/opening", json!({"customer_id": f.customer, "product_id": f.product, "qty": 1})).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("CONFIRM_REQUIRED")));

    // good 3 + damaged 1 of 4 => 25 % damaged > 10 % => confirm
    let body = json!({"customer_id": f.customer, "product_id": f.product, "qty_good": 3, "qty_damaged": 1});
    let (s, v) = post(&f, "/api/jars/receive-empties", body.clone()).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("CONFIRM_REQUIRED")));
    assert_eq!(holding(&pool, f.customer, f.product).await, 6);
    let mut confirmed = body.clone();
    confirmed["confirm"] = json!(true);
    let (s, v) = post(&f, "/api/jars/receive-empties", confirmed).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["holding"], 2);
    assert_eq!(zone(&pool, f.product, "JAR_EMPTY").await, 3);
    assert_eq!(zone(&pool, f.product, "SCRAP_YARD").await, 1);
    assert_eq!(event_count(&pool, f.product, "DESTROYED", Some("DAMAGED_RETURN")).await, 1);
    let types: Vec<String> = sqlx::query_scalar("SELECT txn_type FROM jar_ledger WHERE customer_id=$1 ORDER BY txn_type").bind(f.customer).fetch_all(&pool).await.unwrap();
    assert_eq!(types, vec!["DAMAGED", "OPENING", "RETURN_EMPTY"]);

    // cannot return more than held
    let (s, v) = post(&f, "/api/jars/receive-empties", json!({"customer_id": f.customer, "product_id": f.product, "qty_good": 3})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, _) = post(&f, "/api/jars/receive-empties", json!({"customer_id": f.customer, "product_id": f.product})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "nothing returned");
    let (s, _) = post(&f, "/api/jars/receive-empties", json!({"customer_id": f.customer, "product_id": f.product, "qty_good": -1, "qty_damaged": 3})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "negative");
    assert_eq!(holding(&pool, f.customer, f.product).await, 2);

    // two concurrent returns of 2 against a holding of 2: exactly one wins
    let (a, b) = tokio::join!(
        post(&f, "/api/jars/receive-empties", json!({"customer_id": f.customer, "product_id": f.product, "qty_good": 2})),
        post(&f, "/api/jars/receive-empties", json!({"customer_id": f.customer, "product_id": f.product, "qty_good": 2})),
    );
    let ok = [a.0, b.0].iter().filter(|s| **s == StatusCode::CREATED).count();
    assert_eq!(ok, 1, "{a:?} {b:?}");
    assert_eq!(holding(&pool, f.customer, f.product).await, 0);
    assert_eq!(zone(&pool, f.product, "JAR_EMPTY").await, 5);
    assert_ledger_invariant(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn db_guard_blocks_negative_holdings_but_allows_any_order_within_a_tx(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let r = sqlx::query("INSERT INTO jar_ledger (customer_id,product_id,txn_type,delta) VALUES ($1,$2,'RETURN_EMPTY',-1)").bind(f.customer).bind(f.product).execute(&pool).await;
    assert!(r.is_err(), "negative holding must be rejected by the database");
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO jar_ledger (customer_id,product_id,txn_type,delta) VALUES ($1,$2,'RETURN_EMPTY',-2)").bind(f.customer).bind(f.product).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO jar_ledger (customer_id,product_id,txn_type,delta) VALUES ($1,$2,'ISSUE_FILLED',5)").bind(f.customer).bind(f.product).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(holding(&pool, f.customer, f.product).await, 3);
}

#[sqlx::test(migrations = "./migrations")]
async fn receive_new_wash_destroy_write_events_and_stock(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let p = f.product;
    let (s, v) = post(&f, "/api/jars/receive-new", json!({"product_id": p, "qty": 20, "note": "first lot"})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(zone(&pool, p, "JAR_EMPTY").await, 20);
    assert_eq!(event_count(&pool, p, "NEW_PURCHASED", None).await, 20);
    let (s, _) = post(&f, "/api/jars/receive-new", json!({"product_id": p, "qty": 1, "supplier_id": Uuid::new_v4()})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown supplier");

    // wash 10 good + 2 rejected = 12 leave JAR_EMPTY (2 rejects are > 10 % of 12 => confirm)
    let wb = json!({"product_id": p, "qty": 10, "rejected": 2});
    let (s, v) = post(&f, "/api/jars/wash", wb.clone()).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("CONFIRM_REQUIRED")));
    let (s, v) = call(&f.app, "POST", "/api/jars/wash?dry_run=1", Some(&f.key), Some(json!({"product_id": p, "qty": 10, "rejected": 2, "confirm": true}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["effects"].as_array().unwrap().len(), 3);
    assert_eq!(zone(&pool, p, "JAR_EMPTY").await, 20, "dry run rolled back");
    let (s, v) = post(&f, "/api/jars/wash", json!({"product_id": p, "qty": 10, "rejected": 2, "confirm": true})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((zone(&pool, p, "JAR_EMPTY").await, zone(&pool, p, "JAR_CLEAN").await, zone(&pool, p, "SCRAP_YARD").await), (8, 10, 2));
    assert_eq!(event_count(&pool, p, "WASHED", None).await, 10);
    assert_eq!(event_count(&pool, p, "DESTROYED", Some("WASH_REJECT")).await, 2);
    let (s, v) = post(&f, "/api/jars/wash", json!({"product_id": p, "qty": 9})).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("INSUFFICIENT_STOCK")));
    assert_eq!(zone(&pool, p, "JAR_EMPTY").await, 8);

    // destroy: confirm is mandatory
    let db = json!({"product_id": p, "qty": 3, "location": "PLANT_CLEAN", "reason_code": "LEAKING", "note": "leaking valves"});
    let (s, v) = post(&f, "/api/jars/destroy", db.clone()).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("CONFIRM_REQUIRED")));
    assert_eq!(zone(&pool, p, "JAR_CLEAN").await, 10);
    let mut c = db.clone();
    c["confirm"] = json!(true);
    let (s, v) = post(&f, "/api/jars/destroy", c).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((zone(&pool, p, "JAR_CLEAN").await, zone(&pool, p, "SCRAP_YARD").await), (7, 5));
    assert_eq!(event_count(&pool, p, "DESTROYED", Some("LEAKING")).await, 3);
    // never below zero
    let (s, v) = post(&f, "/api/jars/destroy", json!({"product_id": p, "qty": 50, "location": "PLANT_EMPTY", "reason_code": "OTHER", "note": "too many", "confirm": true})).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("INSUFFICIENT_STOCK")), "{v}");
    assert_eq!(zone(&pool, p, "JAR_EMPTY").await, 8);
    assert_eq!(event_count(&pool, p, "DESTROYED", Some("OTHER")).await, 0, "failed destroy leaves no event");

    // customer write-off
    ledger_row(&pool, f.customer, p, "OPENING", 4, f.today).await;
    let (s, _) = post(&f, "/api/jars/destroy", json!({"product_id": p, "qty": 5, "location": "CUSTOMER", "customer_id": f.customer, "reason_code": "LOST", "note": "customer lost them", "confirm": true})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "more than the customer holds");
    let (s, v) = post(&f, "/api/jars/destroy", json!({"product_id": p, "qty": 4, "location": "CUSTOMER", "customer_id": f.customer, "reason_code": "LOST", "note": "customer lost them", "confirm": true})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(holding(&pool, f.customer, p).await, 0);
    let wo: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jar_ledger WHERE txn_type='WRITE_OFF' AND delta=-4").fetch_one(&pool).await.unwrap();
    assert_eq!(wo, 1);

    // GET /destroyed
    let (s, v) = call(&f.app, "GET", "/api/jars/destroyed", Some(&f.key), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["total"], 3);
    let (_, v) = call(&f.app, "GET", &format!("/api/jars/destroyed?reason=LOST&product_id={p}"), Some(&f.key), None).await;
    assert_eq!(v["total"], 1);
    assert_eq!(v["items"][0]["customer_name"], "Acme Cafe");
    assert_eq!(v["items"][0]["qty"], 4);
    let (_, v) = call(&f.app, "GET", "/api/jars/destroyed?from=2999-01-01", Some(&f.key), None).await;
    assert_eq!(v["total"], 0);
    // audit trail exists
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE action LIKE 'jars.%'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 4);
    assert_ledger_invariant(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn wash_uses_non_nil_lots_fefo(pool: PgPool) {
    // other modules (settlement, production) may leave containers under a non-nil lot id
    let f = fx(pool.clone()).await;
    put_stock(&pool, f.product, "JAR_EMPTY", Uuid::new_v4(), 3).await;
    put_stock(&pool, f.product, "JAR_EMPTY", NIL_LOT, 4).await;
    let (s, v) = post(&f, "/api/jars/wash", json!({"product_id": f.product, "qty": 6})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((zone(&pool, f.product, "JAR_EMPTY").await, zone(&pool, f.product, "JAR_CLEAN").await), (1, 6));
    assert_ledger_invariant(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn fleet_analytics_numbers(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let p = f.product;
    let batch = |crates: i32, days_ago: i32| {
        let pool = pool.clone();
        async move {
            sqlx::query("INSERT INTO production_batches (batch_no,product_id,crates,qc_status,produced_on,expiry_date) VALUES ('B'||floor(random()*1e9)::text,$1,$2,'APPROVED',current_date-$3,current_date+100)")
                .bind(p).bind(crates).bind(days_ago).execute(&pool).await.unwrap();
        }
    };
    // plant-local "today" may differ from current_date by a day; keep fills well inside the 30 day window
    batch(100, 1).await; batch(60, 2).await; batch(40, 3).await; // 200 fills in the period
    batch(100, 90).await;                                         // lifetime only
    let ev = |kind: &'static str, qty: i32, reason: Option<&'static str>, loc: Option<&'static str>, days_ago: i32| {
        let pool = pool.clone();
        async move {
            sqlx::query("INSERT INTO jar_fleet_events (product_id,event_type,qty,reason_code,location,created_at) VALUES ($1,$2,$3,$4,$5, now() - ($6 || ' days')::interval)")
                .bind(p).bind(kind).bind(qty).bind(reason).bind(loc).bind(days_ago.to_string()).execute(&pool).await.unwrap();
        }
    };
    ev("DESTROYED", 4, Some("CRACKED"), Some("PLANT_EMPTY"), 2).await;
    ev("DESTROYED", 2, Some("LOST"), Some("CUSTOMER"), 3).await;
    ev("DESTROYED", 10, Some("AGE_RETIRED"), Some("PLANT_EMPTY"), 80).await; // outside the period
    ev("NEW_PURCHASED", 50, None, None, 5).await;
    ev("WASHED", 30, None, None, 1).await;
    for (zone_, q) in [("JAR_EMPTY", 10), ("JAR_CLEAN", 20), ("FG_READY", 5), ("QC_QUARANTINE", 3)] { put_stock(&pool, p, zone_, if q == 5 || q == 3 { Uuid::new_v4() } else { NIL_LOT }, q).await; }
    ledger_row(&pool, f.customer, p, "OPENING", 40, f.today - chrono::Duration::days(100)).await;
    ledger_row(&pool, f.customer, p, "ISSUE_FILLED", 7, f.today).await;
    ledger_row(&pool, f.customer, p, "RETURN_EMPTY", -4, f.today).await;
    let vehicle: Uuid = sqlx::query_scalar("INSERT INTO vehicles (reg_no,capacity_crates) VALUES ('V9',500) RETURNING id").fetch_one(&pool).await.unwrap();
    let b: Uuid = sqlx::query_scalar("SELECT id FROM production_batches WHERE product_id=$1 LIMIT 1").bind(p).fetch_one(&pool).await.unwrap();
    for (no, status, crates) in [("T9", "DISPATCHED", 6), ("T10", "LOADING", 99), ("T11", "SETTLED", 77)] {
        let t: Uuid = sqlx::query_scalar("INSERT INTO loading_trips (trip_no,vehicle_id,capacity_crates,status) VALUES ($1,$2,500,$3) RETURNING id").bind(no).bind(vehicle).bind(status).fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO trip_items (trip_id,batch_id,product_id,crates) VALUES ($1,$2,$3,$4)").bind(t).bind(b).bind(p).bind(crates).execute(&pool).await.unwrap();
    }

    let (s, v) = call(&f.app, "GET", &format!("/api/jars/fleet?product_id={p}"), Some(&f.key), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // total = 10 + 20 + 5 + 3 + (40+7-4) + 6 (DISPATCHED trip only) = 87
    assert_eq!(v["fleet"]["total"], 87);
    assert_eq!(v["fleet"]["with_customers"], 43);
    assert_eq!(v["fleet"]["in_vehicles"], 6);
    assert_eq!(v["fleet"]["plant_empty"], 10);
    assert_eq!(v["period"]["fills"], 200);
    assert_eq!(v["period"]["delivered"], 7);
    assert_eq!(v["period"]["empties_returned"], 4);
    assert_eq!(v["period"]["new_purchased"], 50);
    assert_eq!(v["period"]["washed"], 30);
    assert_eq!(v["period"]["destroyed_total"], 6);
    assert_eq!(v["period"]["destroyed_by_reason"]["CRACKED"], 4);
    assert_eq!(v["period"]["destroyed_by_reason"]["LOST"], 2);
    assert_eq!(v["period"]["destroyed_by_reason"]["AGE_RETIRED"], 0);
    assert_eq!(v["destroy_rate_pct"], "3.00");
    // lifetime: fills 300, destroyed 16 => 300 / (87 + 16) = 2.91 cycles; max 50 => 5.83 %
    assert_eq!(v["avg_cycles_estimate"], "2.91");
    assert_eq!(v["max_cycles"], 50);
    assert_eq!(v["retirement_pct"], "5.83");
    assert_eq!(v["alert"], "OK", "6/200 = 3.00 % is not above the 3 % limit");
    // one more destroyed container tips it over
    ev("DESTROYED", 1, Some("CRACKED"), Some("PLANT_EMPTY"), 1).await;
    let (_, v) = call(&f.app, "GET", &format!("/api/jars/fleet?product_id={p}"), Some(&f.key), None).await;
    assert_eq!(v["alert"], "HIGH_DESTROY_RATE");
    // explicit narrow period excludes everything but the newest event
    let from = (f.today - chrono::Duration::days(1)).to_string();
    let (_, v) = call(&f.app, "GET", &format!("/api/jars/fleet?product_id={p}&from={from}&to={}", f.today), Some(&f.key), None).await;
    assert_eq!(v["period"]["destroyed_total"], 1);
    assert_eq!(v["period"]["fills"], 100);
    // product_id may be omitted when exactly one returnable exists; bad dates are 422
    let (s, _) = call(&f.app, "GET", "/api/jars/fleet", Some(&f.key), None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(&f.app, "GET", "/api/jars/fleet?from=yesterday", Some(&f.key), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    // retirement alert
    sqlx::query("UPDATE products SET max_cycles = 3 WHERE id=$1").bind(p).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM jar_fleet_events WHERE reason_code='CRACKED'").execute(&pool).await.unwrap();
    let (_, v) = call(&f.app, "GET", &format!("/api/jars/fleet?product_id={p}"), Some(&f.key), None).await;
    assert_eq!(v["alert"], "NEAR_RETIREMENT", "{v}");
}
