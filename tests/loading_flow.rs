mod loading_support;
use axum::http::StatusCode;
use loading_support::*;
use rust_decimal_macros::dec;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test(migrations = "./migrations")]
async fn bulk_add_is_fefo_across_batches_and_merges(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, Some(60)).await;
    let (b1, b2) = (c.batch(p, 5, "APPROVED", 30).await, c.batch(p, 10, "APPROVED", 60).await);
    let t = c.trip(c.vehicle(100).await).await;
    let (s, v) = c.add(t, json!({ "product_id": p, "crates": 8 })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["batch_id"], json!(b1));
    assert_eq!(items[0]["crates"], 5);
    assert_eq!(items[1]["batch_id"], json!(b2));
    assert_eq!(items[1]["crates"], 3);
    assert_eq!(items[0]["pallets"], 0);
    assert_eq!(items[0]["loose_crates"], 5);
    assert!(items[0]["qr_set_id"].is_null() && items[0]["code"].is_null());
    // adding more merges into the (trip, batch) item
    let (s, v) = c.add(t, json!({ "product_id": p, "crates": 2 })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (_, trip) = c.get(&format!("/api/loading/trips/{t}")).await;
    assert_eq!(trip["items"].as_array().unwrap().len(), 2);
    assert_eq!(trip["loaded_crates"], 10);
    // stock has NOT moved yet (reservation only)
    assert_eq!(c.qty(p, "FG_READY", b2).await, dec!(10));
    // availability: FEFO ordered, reserved included
    let (_, av) = c.get(&format!("/api/loading/availability?product_id={p}&trip_id={t}")).await;
    assert_eq!(av[0]["batch_id"], json!(b1));
    assert_eq!(av[0]["reserved"], 5);
    assert_eq!(av[0]["available"], 0);
    assert_eq!(av[1]["reserved"], 5);
    assert_eq!(av[1]["available"], 5);
    assert_eq!(av[1]["in_trip"], 5);
    // over-asking by product
    let (s, v) = c.add(t, json!({ "product_id": p, "crates": 6 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}");
    assert_eq!(v["error"]["details"]["available"], 5);
}

#[sqlx::test(migrations = "./migrations")]
async fn reservations_block_other_trips_and_own_increments(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 12, "APPROVED", 90).await;
    let (t1, t2) = (c.trip(c.vehicle(100).await).await, c.trip(c.vehicle(100).await).await);
    assert_eq!(c.add(t1, json!({ "batch_id": b, "crates": 10 })).await.0, StatusCode::CREATED);
    let (s, v) = c.add(t2, json!({ "batch_id": b, "crates": 3 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"));
    assert_eq!(v["error"]["details"]["available"], 2);
    let (s, v) = c.add(t2, json!({ "product_id": p, "crates": 3 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "product mode also sees reservations");
    assert_eq!(c.add(t2, json!({ "batch_id": b, "crates": 2 })).await.0, StatusCode::CREATED);
    // the owner of t1 cannot top up beyond what is free, either (increment is checked against free incl. own reservation)
    let (s, v) = c.add(t1, json!({ "batch_id": b, "crates": 1 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}");
    // cancelling releases the reservation
    assert_eq!(c.post(&format!("/api/loading/trips/{t1}/cancel"), json!({})).await.0, StatusCode::OK);
    assert_eq!(c.add(t2, json!({ "batch_id": b, "crates": 10 })).await.0, StatusCode::CREATED);
    // removing an item releases it too
    let (_, trip) = c.get(&format!("/api/loading/trips/{t2}")).await;
    let item = trip["items"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(c.delete(&format!("/api/loading/trips/{t2}/items/{item}")).await.0, StatusCode::OK);
    let (_, av) = c.get(&format!("/api/loading/availability?product_id={p}")).await;
    assert_eq!(av[0]["available"], 12);
    // a cancelled trip accepts nothing
    let (s, v) = c.add(t1, json!({ "batch_id": b, "crates": 1 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INVALID_STATE"));
}

#[sqlx::test(migrations = "./migrations")]
async fn qc_hard_lock_strict_and_non_strict(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let q = c.batch(p, 20, "QUARANTINE", 90).await;
    let r = c.batch(p, 5, "REJECTED", 90).await;
    let a = c.batch(p, 5, "APPROVED", 90).await;
    let t = c.trip(c.vehicle(100).await).await;
    // strict (default): quarantine and rejected are locked; product mode only ever picks approved stock
    for b in [q, r] {
        let (s, v) = c.add(t, json!({ "batch_id": b, "crates": 1 })).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "QC_LOCKED"), "{v}");
    }
    let (s, v) = c.add(t, json!({ "product_id": p, "crates": 6 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "quarantine stock is not allocatable: {v}");
    let (_, av) = c.get("/api/loading/availability").await;
    assert_eq!(av.as_array().unwrap().len(), 1);
    // non-strict: quarantine listed + loadable (with a warning), rejected never
    c.set("loading.strict_qc_lock", json!(false)).await;
    let (_, av) = c.get(&format!("/api/loading/availability?product_id={p}")).await;
    assert_eq!(av.as_array().unwrap().len(), 2);
    assert!(av.as_array().unwrap().iter().any(|x| x["qc_status"] == "QUARANTINE" && x["zone"] == "QC_QUARANTINE"));
    let (s, v) = c.add(t, json!({ "batch_id": r, "crates": 1 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "QC_LOCKED"));
    let (s, v) = c.add(t, json!({ "batch_id": q, "crates": 4 })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert!(!v["warnings"].as_array().unwrap().is_empty());
    // strict switched back on before the gate pass: the gate pass re-checks and refuses
    c.set("loading.strict_qc_lock", json!(true)).await;
    let (s, v) = c.gate(t).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "QC_LOCKED"), "{v}");
    assert_eq!(c.qty(p, "QC_QUARANTINE", q).await, dec!(20), "nothing was deducted");
    // non-strict again: the gate pass deducts the quarantine lot from QC_QUARANTINE
    c.set("loading.strict_qc_lock", json!(false)).await;
    assert_eq!(c.gate(t).await.0, StatusCode::OK);
    assert_eq!(c.qty(p, "QC_QUARANTINE", q).await, dec!(16));
    assert_eq!(c.qty(p, "FG_READY", a).await, dec!(5));
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn capacity_is_enforced_with_details_and_warning(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 100, "APPROVED", 90).await;
    let t = c.trip(c.vehicle(20).await).await;
    assert_eq!(c.add(t, json!({ "batch_id": b, "crates": 10 })).await.0, StatusCode::CREATED);
    let (s, v) = c.add(t, json!({ "batch_id": b, "crates": 11 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CAPACITY_EXCEEDED"));
    let d = &v["error"]["details"];
    assert_eq!((d["capacity"].as_i64(), d["loaded"].as_i64(), d["adding"].as_i64()), (Some(20), Some(10), Some(11)));
    let (s, v) = c.add(t, json!({ "batch_id": b, "crates": 10 })).await;
    assert_eq!(s, StatusCode::CREATED);
    assert!(v["warnings"][0].as_str().unwrap().contains("100%"), "{v}");
    // validation
    for bad in [json!({ "batch_id": b, "crates": 0 }), json!({ "batch_id": b, "crates": -3 }), json!({ "batch_id": b }),
                json!({ "crates": 1 }), json!({ "batch_id": b, "product_id": p, "crates": 1 })] {
        let (s, _) = c.add(t, bad.clone()).await;
        assert!(s == StatusCode::UNPROCESSABLE_ENTITY || s == StatusCode::BAD_REQUEST, "{bad} -> {s}");
    }
    let (s, v) = c.add(t, json!({ "batch_id": b, "crates": 1, "is_admin": true })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "unknown fields rejected: {v}");
    assert_eq!(code(&v), "BAD_REQUEST");
}

#[sqlx::test(migrations = "./migrations")]
async fn gate_pass_deducts_each_items_own_lot_and_is_single_shot(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, Some(60)).await;
    let (b1, b2) = (c.batch(p, 10, "APPROVED", 30).await, c.batch(p, 10, "APPROVED", 60).await);
    let t = c.trip(c.vehicle(100).await).await;
    // explicitly load the LATER-expiring lot: FEFO must not re-allocate at gate pass
    assert_eq!(c.add(t, json!({ "batch_id": b2, "crates": 7 })).await.0, StatusCode::CREATED);
    let (s, v) = c.gate(t).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "DISPATCHED");
    let gp = v["gate_pass_no"].as_str().unwrap().to_string();
    assert!(gp.starts_with("GP-"), "{gp}");
    assert_eq!(c.qty(p, "FG_READY", b2).await, dec!(3));
    assert_eq!(c.qty(p, "FG_READY", b1).await, dec!(10));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_ledger WHERE txn_type='GATE_PASS' AND ref_type='TRIP' AND ref_id=$1 AND qty_delta=-7 AND lot_id=$2")
        .bind(t).bind(b2).fetch_one(&c.pool).await.unwrap();
    assert_eq!(rows, 1);
    let alerts: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='GATE_PASS_ISSUED'").fetch_one(&c.pool).await.unwrap();
    assert_eq!(alerts, 1);
    let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='GATE_PASS_ISSUED'").fetch_one(&c.pool).await.unwrap();
    assert_eq!(audit, 1);
    // second call: state conflict, stock deducted only once
    let (s, v) = c.gate(t).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INVALID_STATE"));
    assert_eq!(c.qty(p, "FG_READY", b2).await, dec!(3));
    // items can no longer be changed
    let (s, v) = c.add(t, json!({ "batch_id": b1, "crates": 1 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INVALID_STATE"));
    // printable gate pass
    let (s, v) = c.get(&format!("/api/loading/trips/{t}/gate-pass")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["gate_pass_no"], json!(gp));
    assert_eq!(v["totals"]["crates"], 7);
    assert_eq!(v["totals"]["bottles"], 84);
    assert_eq!(v["items"][0]["units_per_crate"], 12);
    // an empty trip cannot get a gate pass
    let t2 = c.trip(c.vehicle(10).await).await;
    assert_eq!(c.gate(t2).await.0, StatusCode::BAD_REQUEST);
    // and no printable pass before dispatch
    assert_eq!(c.get(&format!("/api/loading/trips/{t2}/gate-pass")).await.0, StatusCode::CONFLICT);
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn expired_vehicle_documents_block_the_gate_pass(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 10, "APPROVED", 30).await;
    let v = c.vehicle(50).await;
    sqlx::query("UPDATE vehicles SET puc_expiry = current_date - 3, insurance_expiry = current_date + 100 WHERE id=$1").bind(v).execute(&c.pool).await.unwrap();
    let t = c.trip(v).await;
    assert_eq!(c.add(t, json!({ "batch_id": b, "crates": 5 })).await.0, StatusCode::CREATED);
    let (s, r) = c.gate(t).await;
    assert_eq!((s, code(&r)), (StatusCode::CONFLICT, "VEHICLE_DOCS_EXPIRED"), "{r}");
    assert_eq!(r["error"]["details"]["expired"][0]["doc"], "PUC");
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(10));
    // renewing the document unblocks it
    sqlx::query("UPDATE vehicles SET puc_expiry = current_date + 30 WHERE id=$1").bind(v).execute(&c.pool).await.unwrap();
    assert_eq!(c.gate(t).await.0, StatusCode::OK);
}

async fn dispatched_trip(c: &Ctx, p: uuid::Uuid, b: uuid::Uuid, crates: i64) -> (uuid::Uuid, String) {
    let t = c.trip(c.vehicle(500).await).await;
    assert_eq!(c.add(t, json!({ "batch_id": b, "crates": crates })).await.0, StatusCode::CREATED);
    assert_eq!(c.gate(t).await.0, StatusCode::OK);
    let (_, trip) = c.get(&format!("/api/loading/trips/{t}")).await;
    let _ = p;
    (t, trip["items"][0]["id"].as_str().unwrap().to_string())
}

#[sqlx::test(migrations = "./migrations")]
async fn settlement_restocks_returns_and_scraps_damage(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 50, "APPROVED", 60).await;
    let (t, item) = dispatched_trip(&c, p, b, 20).await;
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(30));
    let uri = format!("/api/loading/trips/{t}/settle");
    // 3 damaged of 20 = 15 % > 10 % => needs confirm
    let body = json!({ "items": [{ "item_id": item, "delivered": 14, "returned": 3, "damaged": 3 }] });
    let (s, v) = c.post(&uri, body.clone()).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFIRM_REQUIRED"), "{v}");
    assert!(!v["error"]["details"]["warnings"].as_array().unwrap().is_empty());
    // dry run with confirm leaves no trace
    let mut conf = body.clone();
    conf["confirm"] = json!(true);
    let (s, d) = c.post(&format!("{uri}?dry_run=1"), conf.clone()).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["dry_run"], true);
    assert!(d["effects"].as_array().unwrap().len() >= 2);
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(30));
    let st: String = sqlx::query_scalar("SELECT status FROM loading_trips WHERE id=$1").bind(t).fetch_one(&c.pool).await.unwrap();
    assert_eq!(st, "DISPATCHED");
    // real settle
    let (s, v) = c.post(&uri, conf).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "SETTLED");
    assert_eq!(v["items"][0]["crates_delivered"], 14);
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(33), "returned crates go back into the SAME lot");
    assert_eq!(c.qty(p, "SCRAP_YARD", b).await, dec!(3));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_ledger WHERE ref_id=$1 AND txn_type IN ('TRIP_RETURN','TRIP_DAMAGED')").bind(t).fetch_one(&c.pool).await.unwrap();
    assert_eq!(n, 2);
    let alerts: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='TRIP_SETTLED'").fetch_one(&c.pool).await.unwrap();
    assert_eq!(alerts, 1);
    // settling twice
    let (s, v) = c.post(&uri, body).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INVALID_STATE"));
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(33));
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn settlement_arithmetic_abuse_is_rejected(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 50, "APPROVED", 60).await;
    let (t, item) = dispatched_trip(&c, p, b, 10).await;
    // a second, unrelated trip's item id
    let (t2, foreign) = dispatched_trip(&c, p, b, 5).await;
    let _ = t2;
    let uri = format!("/api/loading/trips/{t}/settle");
    let cases = vec![
        ("sum too high", json!({ "items": [{ "item_id": item, "delivered": 10, "returned": 1, "damaged": 0 }] })),
        ("sum too low", json!({ "items": [{ "item_id": item, "delivered": 5, "returned": 1, "damaged": 0 }] })),
        ("negative returned (sums fine)", json!({ "items": [{ "item_id": item, "delivered": 15, "returned": -5, "damaged": 0 }] })),
        ("negative damaged", json!({ "items": [{ "item_id": item, "delivered": 12, "returned": 0, "damaged": -2 }] })),
        ("item twice", json!({ "items": [{ "item_id": item, "delivered": 10, "returned": 0, "damaged": 0 }, { "item_id": item, "delivered": 10, "returned": 0, "damaged": 0 }] })),
        ("foreign item", json!({ "items": [{ "item_id": foreign, "delivered": 5, "returned": 0, "damaged": 0 }] })),
        ("foreign + own", json!({ "items": [{ "item_id": item, "delivered": 10, "returned": 0, "damaged": 0 }, { "item_id": foreign, "delivered": 5, "returned": 0, "damaged": 0 }] })),
        ("missing item", json!({ "items": [] })),
        ("huge numbers", json!({ "items": [{ "item_id": item, "delivered": 9223372036854775807i64, "returned": 1, "damaged": 0 }] })),
        ("float", json!({ "items": [{ "item_id": item, "delivered": 9.5, "returned": 0.5, "damaged": 0 }] })),
    ];
    for (name, body) in cases {
        let (s, v) = c.post(&uri, body).await;
        assert!(s == StatusCode::UNPROCESSABLE_ENTITY || s == StatusCode::BAD_REQUEST, "{name}: {s} {v}");
    }
    let (s, v) = c.post(&uri, json!({ "items": [{ "item_id": item, "delivered": 10, "returned": 0, "damaged": 0 }], "jar_ledger": [] })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "unknown field: {v}");
    // nothing changed
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(35));
    let st: String = sqlx::query_scalar("SELECT status FROM loading_trips WHERE id=$1").bind(t).fetch_one(&c.pool).await.unwrap();
    assert_eq!(st, "DISPATCHED");
    // a clean settle still works afterwards (validation does not poison state)
    let (s, v) = c.post(&uri, json!({ "items": [{ "item_id": item, "delivered": "10", "returned": 0, "damaged": 0 }] })).await;
    assert_eq!(s, StatusCode::OK, "numeric strings are accepted: {v}");
    // nothing returned: stock unchanged
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(35));
}

#[sqlx::test(migrations = "./migrations")]
async fn returnable_trip_reconciles_jar_ledger_empties_and_fleet(pool: PgPool) {
    let c = ctx(pool).await;
    let jar = c.product("RETURNABLE", 1, Some(40)).await;
    let jb = c.batch(jar, 40, "APPROVED", 60).await;
    let (c1, c2) = (c.customer("Alpha").await, c.customer("Beta").await);
    // Alpha already holds 5 jars from before
    sqlx::query("INSERT INTO jar_ledger (customer_id,product_id,txn_type,delta) VALUES ($1,$2,'OPENING',5)").bind(c1).bind(jar).execute(&c.pool).await.unwrap();
    let (t, item) = dispatched_trip(&c, jar, jb, 30).await;
    let uri = format!("/api/loading/trips/{t}/settle");
    let items = |d, r, x| json!([{ "item_id": item, "delivered": d, "returned": r, "damaged": x }]);
    // customer lines must add up to the delivered jars
    let (s, v) = c.post(&uri, json!({ "items": items(26, 2, 2), "confirm": true,
        "customer_jars": [{ "customer_id": c1, "product_id": jar, "delivered": 10, "empties_collected": 4, "damaged": 1, "lost": 0 }] })).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    // a customer cannot return more than they hold: Beta holds 0, receives 16 and returns 17
    let (s, v) = c.post(&uri, json!({ "items": items(26, 2, 2), "confirm": true,
        "customer_jars": [{ "customer_id": c1, "product_id": jar, "delivered": 10, "empties_collected": 4, "damaged": 1, "lost": 0 },
                          { "customer_id": c2, "product_id": jar, "delivered": 16, "empties_collected": 17, "damaged": 0, "lost": 0 }] })).await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "BAD_REQUEST"), "{v}");
    // duplicate customer/product pair, foreign product, unknown customer
    let dup = json!({ "items": items(26, 2, 2), "confirm": true, "customer_jars": [
        { "customer_id": c1, "product_id": jar, "delivered": 10 }, { "customer_id": c1, "product_id": jar, "delivered": 16 }] });
    assert_eq!(c.post(&uri, dup).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let ghost = json!({ "items": items(26, 2, 2), "confirm": true, "customer_jars": [
        { "customer_id": uuid::Uuid::new_v4(), "product_id": jar, "delivered": 26 }] });
    assert_eq!(c.post(&uri, ghost).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let st: String = sqlx::query_scalar("SELECT status FROM loading_trips WHERE id=$1").bind(t).fetch_one(&c.pool).await.unwrap();
    assert_eq!(st, "DISPATCHED");
    // happy path
    let good = json!({ "items": items(26, 2, 2), "confirm": true, "customer_jars": [
        { "customer_id": c1, "product_id": jar, "delivered": 10, "empties_collected": 8, "damaged": 1, "lost": 1 },
        { "customer_id": c2, "product_id": jar, "delivered": 16, "empties_collected": 0, "damaged": 0, "lost": 0 }] });
    let (s, v) = c.post(&format!("{uri}?dry_run=1"), good.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM jar_ledger WHERE trip_id=$1").bind(t).fetch_one(&c.pool).await.unwrap();
    assert_eq!(n, 0, "dry run writes nothing");
    let (s, v) = c.post(&uri, good).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let hold = |cust| { let pool = c.pool.clone(); async move { sqlx::query_scalar::<_, i64>("SELECT COALESCE(SUM(delta),0)::bigint FROM jar_ledger WHERE customer_id=$1 AND product_id=$2").bind(cust).bind(jar).fetch_one(&pool).await.unwrap() } };
    assert_eq!(hold(c1).await, 5 + 10 - 8 - 1 - 1);
    assert_eq!(hold(c2).await, 16);
    // empties collected join JAR_EMPTY; damaged empties + damaged loaded jars go to SCRAP_YARD; returned filled jars back to the lot
    assert_eq!(c.qty(jar, "JAR_EMPTY", water_erp::stock::NIL_LOT).await, dec!(8));
    assert_eq!(c.qty(jar, "SCRAP_YARD", water_erp::stock::NIL_LOT).await, dec!(1));
    assert_eq!(c.qty(jar, "SCRAP_YARD", jb).await, dec!(2));
    assert_eq!(c.qty(jar, "FG_READY", jb).await, dec!(12));
    // fleet events: damaged in vehicle (2), damaged at customer (1), lost at customer (1)
    let ev: Vec<(String, i32, String, String)> = sqlx::query_as("SELECT event_type, qty, reason_code, location FROM jar_fleet_events WHERE trip_id=$1 ORDER BY location, reason_code")
        .bind(t).fetch_all(&c.pool).await.unwrap();
    assert_eq!(ev, vec![
        ("DESTROYED".into(), 1, "DAMAGED_RETURN".into(), "CUSTOMER".into()),
        ("DESTROYED".into(), 1, "LOST".into(), "CUSTOMER".into()),
        ("DESTROYED".into(), 2, "DAMAGED_RETURN".into(), "VEHICLE".into()),
    ]);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM jar_ledger WHERE trip_id=$1 AND delta=0").bind(t).fetch_one(&c.pool).await.unwrap();
    assert_eq!(rows, 0);
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn trip_create_rules_and_filters_resist_injection(pool: PgPool) {
    let c = ctx(pool).await;
    let v = c.vehicle(50).await;
    let t = c.trip(v).await;
    let (s, r) = c.post("/api/loading/trips", json!({ "vehicle_id": v })).await;
    assert_eq!((s, code(&r)), (StatusCode::CONFLICT, "DUPLICATE"), "one open LOADING trip per vehicle: {r}");
    sqlx::query("UPDATE vehicles SET is_active=false WHERE id=$1").bind(v).execute(&c.pool).await.unwrap();
    let v2 = c.vehicle(50).await;
    sqlx::query("UPDATE vehicles SET is_active=false WHERE id=$1").bind(v2).execute(&c.pool).await.unwrap();
    assert_eq!(c.post("/api/loading/trips", json!({ "vehicle_id": v2 })).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(c.post("/api/loading/trips", json!({ "vehicle_id": uuid::Uuid::new_v4() })).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    // injection strings are bound parameters; status is whitelisted
    for q in ["%27%20OR%201%3D1%20--", "%25", "_", "x%27%3B%20DROP%20TABLE%20loading_trips%3B--"] {
        let (s, r) = c.get(&format!("/api/loading/trips?q={q}")).await;
        assert_eq!(s, StatusCode::OK, "{r}");
        assert_eq!(r["total"], 0, "{q}");
        let (s, _) = c.get(&format!("/api/loading/dispatches?q={q}")).await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, _) = c.get("/api/loading/trips?status=LOADING%27%20OR%20%271%27%3D%271").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, r) = c.get("/api/loading/trips?status=LOADING").await;
    assert_eq!((s, r["total"].as_i64()), (StatusCode::OK, Some(1)));
    assert_eq!(c.get("/api/loading/trips/not-a-uuid").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(c.get(&format!("/api/loading/trips/{}", uuid::Uuid::new_v4())).await.0, StatusCode::NOT_FOUND);
    let (_, r) = c.get("/api/loading/summary").await;
    assert_eq!(r["open_trips"], 1);
    let _ = t;
}

#[sqlx::test(migrations = "./migrations")]
async fn page_permissions_are_enforced_per_route_family(pool: PgPool) {
    let c = ctx(pool).await;
    for uri in ["/api/loading/availability", "/api/loading/trips", "/api/loading/summary", "/api/loading/dispatches", "/api/qr/sets"] {
        let (s, _) = common::call(&c.app, "GET", uri, Some(&c.nobody), None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{uri}");
        let (s, _) = common::call(&c.app, "GET", uri, None, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{uri}");
    }
    let (s, _) = c.post_as(&c.nobody, "/api/loading/trips", json!({ "vehicle_id": uuid::Uuid::new_v4() })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // qr routes answer FEATURE_DISABLED while the feature is off, even for permitted users
    let (s, v) = c.get("/api/qr/sets").await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "FEATURE_DISABLED"));
    // and the {qr} loading mode is gated too
    let t = c.trip(c.vehicle(10).await).await;
    let (s, v) = c.add(t, json!({ "qr": "ABCDEFGHJK" })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "FEATURE_DISABLED"), "{v}");
}
