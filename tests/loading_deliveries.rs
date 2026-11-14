//! Feature X: per-customer deliveries of bottled goods recorded at trip settlement (`customer_deliveries`),
//! `GET /api/loading/deliveries` and `/deliveries/summary`, trip detail `customer_deliveries` / `unassigned_crates`.
mod loading_support;
use axum::http::StatusCode;
use loading_support::*;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

/// Dispatch a trip carrying `loads = [(batch, crates)]`; returns the trip id and the item id of each load (same order).
async fn dispatched(c: &Ctx, loads: &[(Uuid, i64)]) -> (Uuid, Vec<Value>) {
    let t = c.trip(c.vehicle(500).await).await;
    for (b, n) in loads { assert_eq!(c.add(t, json!({ "batch_id": b, "crates": n })).await.0, StatusCode::CREATED); }
    assert_eq!(c.gate(t).await.0, StatusCode::OK);
    let (_, trip) = c.get(&format!("/api/loading/trips/{t}")).await;
    let items = trip["items"].as_array().unwrap();
    let ids = loads.iter().map(|(b, _)| items.iter().find(|i| i["batch_id"] == json!(b)).unwrap()["id"].clone()).collect();
    (t, ids)
}

async fn count(c: &Ctx, sql: &str, id: Uuid) -> i64 { sqlx::query_scalar(sql).bind(id).fetch_one(&c.pool).await.unwrap() }

#[sqlx::test(migrations = "./migrations")]
async fn settlement_records_deliveries_unassigned_and_reads_back(pool: PgPool) {
    let c = ctx(pool).await;
    let (p1, p2) = (c.product("BOTTLED", 12, Some(10)).await, c.product("BOTTLED", 24, None).await);
    let jar = c.product("RETURNABLE", 1, Some(40)).await;
    let (b1, b2, bj) = (c.batch(p1, 60, "APPROVED", 60).await, c.batch(p2, 20, "APPROVED", 60).await, c.batch(jar, 20, "APPROVED", 60).await);
    let (alpha, beta, gamma) = (c.customer("Alpha").await, c.customer("Beta").await, c.customer("Gamma").await);
    let (t, it) = dispatched(&c, &[(b1, 40), (b2, 10), (bj, 8)]).await;
    let uri = format!("/api/loading/trips/{t}/settle");
    // p1: 30 delivered, 6 returned, 4 damaged (8% of the 58 loaded: below the warning level); p2 and the jars are delivered in full
    let body = json!({
        "items": [{ "item_id": it[0], "delivered": 30, "returned": 6, "damaged": 4 }, { "item_id": it[1], "delivered": 10, "returned": 0, "damaged": 0 },
                  { "item_id": it[2], "delivered": 8, "returned": 0, "damaged": 0 }],
        "customer_jars": [{ "customer_id": gamma, "product_id": jar, "delivered": 8 }],
        "customer_deliveries": [{ "customer_id": alpha, "product_id": p1, "crates": 25 }, { "customer_id": beta, "product_id": p1, "crates": 2 },
                                { "customer_id": alpha, "product_id": p2, "crates": "10" }],
    });
    // dry run: effects + the unassigned warning, nothing written
    let (s, v) = c.post(&format!("{uri}?dry_run=1"), body.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["dry_run"], true);
    let fx: Vec<&Value> = v["effects"].as_array().unwrap().iter().filter(|e| e["txn_type"] == "TRIP_DELIVERY").collect();
    assert_eq!(fx.len(), 3, "{v}");
    assert!(fx.iter().any(|e| e["item"].as_str().unwrap().contains("Alpha") && e["delta"] == "25"), "{v}");
    assert!(v["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("3 crates") && w.as_str().unwrap().contains("not assigned")), "{v}");
    assert_eq!(count(&c, "SELECT count(*) FROM trip_customer_deliveries WHERE trip_id=$1", t).await, 0);
    assert_eq!(count(&c, "SELECT count(*) FROM audit_logs WHERE action='TRIP_SETTLED' AND entity_id=$1::text", t).await, 0);
    assert_eq!(count(&c, "SELECT count(*) FROM stock_ledger WHERE ref_id=$1 AND txn_type IN ('TRIP_RETURN','TRIP_DAMAGED')", t).await, 0);
    let st: String = sqlx::query_scalar("SELECT status FROM loading_trips WHERE id=$1").bind(t).fetch_one(&c.pool).await.unwrap();
    assert_eq!(st, "DISPATCHED");
    // real run
    let (s, v) = c.post(&uri, body.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "SETTLED");
    let cd = v["customer_deliveries"].as_array().unwrap();
    assert_eq!(cd.len(), 3, "{v}");
    assert_eq!((cd[0]["customer_name"].as_str(), cd[0]["crates"].as_i64(), cd[0]["pallets"].as_i64(), cd[0]["loose_crates"].as_i64()), (Some("Alpha"), Some(25), Some(2), Some(5)), "{v}");
    let un: Vec<(String, i64, i64, i64)> = v["unassigned_crates"].as_array().unwrap().iter()
        .map(|u| (u["product_id"].as_str().unwrap().to_string(), u["delivered"].as_i64().unwrap(), u["assigned"].as_i64().unwrap(), u["unassigned"].as_i64().unwrap())).collect();
    assert_eq!(un.len(), 2, "returnable containers are not part of the bottled split: {v}");
    assert!(un.contains(&(p1.to_string(), 30, 27, 3)) && un.contains(&(p2.to_string(), 10, 10, 0)), "{un:?}");
    // trip detail shows the same once settled; stock moves are exactly those of a settlement without deliveries
    let (_, d) = c.get(&format!("/api/loading/trips/{t}")).await;
    assert_eq!(d["customer_deliveries"].as_array().unwrap().len(), 3);
    assert_eq!(d["unassigned_crates"].as_array().unwrap().len(), 2);
    assert_eq!(c.qty(p1, "FG_READY", b1).await, dec!(26)); // 60 - 40 + 6 returned
    assert_eq!(c.qty(p1, "SCRAP_YARD", b1).await, dec!(4));
    assert_eq!(c.qty(p2, "FG_READY", b2).await, dec!(10));
    let led: i64 = count(&c, "SELECT count(*) FROM stock_ledger WHERE ref_id=$1 AND txn_type NOT IN ('GATE_PASS','TRIP_RETURN','TRIP_DAMAGED')", t).await;
    assert_eq!(led, 0, "deliveries have no stock effect");
    assert_eq!(count(&c, "SELECT count(*) FROM audit_logs WHERE action='TRIP_SETTLED' AND entity_id=$1::text AND (details->>'delivery_lines')::int = 3", t).await, 1);
    // an unsettled trip has no deliveries block
    let t2 = c.trip(c.vehicle(10).await).await;
    let (_, d2) = c.get(&format!("/api/loading/trips/{t2}")).await;
    assert!(d2.get("customer_deliveries").is_none());

    // ---- reads
    let (s, l) = c.get("/api/loading/deliveries").await;
    assert_eq!(s, StatusCode::OK, "{l}");
    assert_eq!(l["total"], 3);
    let first = &l["items"][0];
    for k in ["trip_id", "trip_no", "settled_at", "customer_id", "customer_name", "product_id", "product_name", "crates", "pallets", "loose_crates"] { assert!(first.get(k).is_some(), "missing {k}: {first}"); }
    let (_, l) = c.get(&format!("/api/loading/deliveries?customer_id={alpha}")).await;
    assert_eq!((l["total"].as_i64(), l["items"].as_array().unwrap().len()), (Some(2), 2));
    let (_, l) = c.get(&format!("/api/loading/deliveries?product_id={p2}")).await;
    assert_eq!(l["total"], 1);
    assert_eq!(l["items"][0]["pallets"], Value::Null, "product without pallet size");
    let (_, l) = c.get("/api/loading/deliveries?limit=2&offset=2").await;
    assert_eq!((l["total"].as_i64(), l["items"].as_array().unwrap().len()), (Some(3), 1));
    let (_, l) = c.get("/api/loading/deliveries?from=2999-01-01").await;
    assert_eq!(l["total"], 0);
    let (_, l) = c.get("/api/loading/deliveries?to=2000-01-01").await;
    assert_eq!(l["total"], 0);
    let today = (chrono::Utc::now() + chrono::Duration::minutes(c.st.settings.get().utc_offset_minutes as i64)).format("%Y-%m-%d").to_string();
    let (_, l) = c.get(&format!("/api/loading/deliveries?from={today}&to={today}")).await;
    assert_eq!(l["total"], 3, "settled today (plant date): {l}");
    let (_, l) = c.get(&format!("/api/loading/deliveries?from={today}")).await;
    assert_eq!(l["total"], 3);
    assert_eq!(c.get("/api/loading/deliveries?from=nope").await.0, StatusCode::BAD_REQUEST);
    let (_, sm) = c.get("/api/loading/deliveries/summary").await;
    assert_eq!(sm["total"], 2);
    assert_eq!(sm["totals"]["crates"], 37);
    assert_eq!(sm["items"][0]["customer_name"], "Alpha");
    assert_eq!((sm["items"][0]["total_crates"].as_i64(), sm["items"][0]["trips"].as_i64()), (Some(35), Some(1)));
    let prods = sm["items"][0]["products"].as_array().unwrap();
    assert_eq!(prods.len(), 2);
    assert_eq!(sm["items"][1]["customer_name"], "Beta");
    let (_, sm) = c.get("/api/loading/deliveries/summary?to=2000-01-01").await;
    assert_eq!((sm["total"].as_i64(), sm["items"].as_array().unwrap().len()), (Some(0), 0));
    // the returnable containers still went through the jar ledger
    let held: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(delta),0)::bigint FROM jar_ledger WHERE customer_id=$1 AND product_id=$2").bind(gamma).bind(jar).fetch_one(&c.pool).await.unwrap();
    assert_eq!(held, 8);
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn invalid_delivery_lines_are_refused_and_nothing_changes(pool: PgPool) {
    let c = ctx(pool).await;
    let p1 = c.product("BOTTLED", 12, None).await;
    let p_other = c.product("BOTTLED", 12, None).await;
    let jar = c.product("RETURNABLE", 1, None).await;
    let (b1, bj) = (c.batch(p1, 30, "APPROVED", 60).await, c.batch(jar, 10, "APPROVED", 60).await);
    let (alpha, beta) = (c.customer("Alpha").await, c.customer("Beta").await);
    let gone = c.customer("Gone").await;
    sqlx::query("UPDATE customers SET is_active=false WHERE id=$1").bind(gone).execute(&c.pool).await.unwrap();
    let (t, it) = dispatched(&c, &[(b1, 20), (bj, 5)]).await;
    let uri = format!("/api/loading/trips/{t}/settle");
    let items = json!([{ "item_id": it[0], "delivered": 12, "returned": 8, "damaged": 0 }, { "item_id": it[1], "delivered": 5, "returned": 0, "damaged": 0 }]);
    let jars = json!([{ "customer_id": alpha, "product_id": jar, "delivered": 5 }]);
    let line = |cust: Uuid, prod: Uuid, crates: Value| json!({ "customer_id": cust, "product_id": prod, "crates": crates });
    let many: Vec<Value> = (0..201).map(|_| line(alpha, p1, json!(1))).collect();
    let cases: Vec<(&str, Value, StatusCode)> = vec![
        ("sum above delivered", json!([line(alpha, p1, json!(7)), line(beta, p1, json!(6))]), StatusCode::UNPROCESSABLE_ENTITY),
        ("duplicate pair", json!([line(alpha, p1, json!(3)), line(alpha, p1, json!(4))]), StatusCode::UNPROCESSABLE_ENTITY),
        ("returnable product", json!([line(alpha, jar, json!(5))]), StatusCode::UNPROCESSABLE_ENTITY),
        ("product not on the trip", json!([line(alpha, p_other, json!(1))]), StatusCode::UNPROCESSABLE_ENTITY),
        ("inactive customer", json!([line(gone, p1, json!(1))]), StatusCode::UNPROCESSABLE_ENTITY),
        ("unknown customer", json!([line(Uuid::new_v4(), p1, json!(1))]), StatusCode::UNPROCESSABLE_ENTITY),
        ("zero crates", json!([line(alpha, p1, json!(0))]), StatusCode::UNPROCESSABLE_ENTITY),
        ("negative crates", json!([line(alpha, p1, json!(-3))]), StatusCode::UNPROCESSABLE_ENTITY),
        ("fractional crates", json!([line(alpha, p1, json!(1.5))]), StatusCode::BAD_REQUEST),
        ("more than 200 lines", Value::Array(many), StatusCode::UNPROCESSABLE_ENTITY),
        ("unknown field", json!([{ "customer_id": alpha, "product_id": p1, "crates": 1, "price": 5 }]), StatusCode::BAD_REQUEST),
    ];
    for (name, cd, want) in cases {
        let (s, v) = c.post(&uri, json!({ "items": items, "customer_jars": jars, "customer_deliveries": cd })).await;
        assert_eq!(s, want, "{name}: {v}");
        let (s, _) = c.post(&format!("{uri}?dry_run=1"), json!({ "items": items, "customer_jars": jars, "customer_deliveries": cd })).await;
        assert_eq!(s, want, "{name} (dry run answers the same)");
    }
    let (_, v) = c.post(&uri, json!({ "items": items, "customer_jars": jars, "customer_deliveries": [line(alpha, p1, json!(7)), line(beta, p1, json!(6))] })).await;
    assert!(v["error"]["fields"].to_string().contains("only 12 were delivered"), "{v}");
    // an item that delivered nothing cannot be assigned to anyone
    let none = json!([{ "item_id": it[0], "delivered": 0, "returned": 20, "damaged": 0 }, { "item_id": it[1], "delivered": 5, "returned": 0, "damaged": 0 }]);
    let (s, v) = c.post(&uri, json!({ "items": none, "customer_jars": jars, "customer_deliveries": [line(alpha, p1, json!(1))] })).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    // nothing was written anywhere
    assert_eq!(count(&c, "SELECT count(*) FROM trip_customer_deliveries WHERE trip_id=$1", t).await, 0);
    assert_eq!(count(&c, "SELECT count(*) FROM jar_ledger WHERE trip_id=$1", t).await, 0);
    assert_eq!(c.qty(p1, "FG_READY", b1).await, dec!(10));
    let st: String = sqlx::query_scalar("SELECT status FROM loading_trips WHERE id=$1").bind(t).fetch_one(&c.pool).await.unwrap();
    assert_eq!(st, "DISPATCHED");
    // a customer deactivated before the settlement cannot receive; without deliveries the old flow is unchanged and everything is 'unassigned'
    let (s, v) = c.post(&uri, json!({ "items": items, "customer_jars": jars })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["customer_deliveries"], json!([]));
    assert_eq!(v["unassigned_crates"][0]["unassigned"], 12);
    let (_, l) = c.get("/api/loading/deliveries").await;
    assert_eq!(l["total"], 0);
    // settling twice is refused (idempotent by state)
    let (s, v) = c.post(&uri, json!({ "items": items, "customer_jars": jars, "customer_deliveries": [line(alpha, p1, json!(1))] })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INVALID_STATE"), "{v}");
    assert_eq!(count(&c, "SELECT count(*) FROM trip_customer_deliveries WHERE trip_id=$1", t).await, 0);
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_settlements_with_deliveries_record_them_once(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 30, "APPROVED", 60).await;
    let (alpha, beta) = (c.customer("Alpha").await, c.customer("Beta").await);
    let (t, it) = dispatched(&c, &[(b, 20)]).await;
    let body = json!({ "items": [{ "item_id": it[0], "delivered": 15, "returned": 5, "damaged": 0 }],
        "customer_deliveries": [{ "customer_id": alpha, "product_id": p, "crates": 9 }, { "customer_id": beta, "product_id": p, "crates": 6 }] });
    let mut hs = vec![];
    for _ in 0..4 {
        let (app, key, body) = (c.app.clone(), c.key.clone(), body.clone());
        hs.push(tokio::spawn(async move { common::call(&app, "POST", &format!("/api/loading/trips/{t}/settle"), Some(&key), Some(body)).await }));
    }
    let mut ok = 0;
    for h in hs { let (s, v) = h.await.unwrap(); if s == StatusCode::OK { ok += 1 } else { assert_eq!(code(&v), "INVALID_STATE", "{v}") } }
    assert_eq!(ok, 1);
    assert_eq!(count(&c, "SELECT count(*) FROM trip_customer_deliveries WHERE trip_id=$1", t).await, 2);
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(15));
    // two trips settling at the same time for the same customers (customer rows are locked in one sorted statement: no deadlock)
    let (t2, it2) = dispatched(&c, &[(b, 5)]).await;
    let (t3, it3) = dispatched(&c, &[(b, 5)]).await;
    let mut hs = vec![];
    for (tt, i) in [(t2, it2[0].clone()), (t3, it3[0].clone())] {
        let (app, key) = (c.app.clone(), c.key.clone());
        let body = json!({ "items": [{ "item_id": i, "delivered": 5, "returned": 0, "damaged": 0 }],
            "customer_deliveries": [{ "customer_id": beta, "product_id": p, "crates": 2 }, { "customer_id": alpha, "product_id": p, "crates": 3 }] });
        hs.push(tokio::spawn(async move { common::call(&app, "POST", &format!("/api/loading/trips/{tt}/settle"), Some(&key), Some(body)).await }));
    }
    for h in hs { let (s, v) = h.await.unwrap(); assert_eq!(s, StatusCode::OK, "{v}"); }
    let (_, sm) = c.get("/api/loading/deliveries/summary").await;
    assert_eq!(sm["totals"]["crates"], 15 + 10);
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn deliveries_reads_are_godown_scoped(pool: PgPool) {
    let c = ctx(pool).await;
    let g1 = c.godown("G1").await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 20, "APPROVED", 60).await;
    c.stock_at(g1, p, b, 20).await;
    let alpha = c.customer("Alpha").await;
    // one trip from MAIN, one from G1
    let main_trip = {
        let (t, it) = dispatched(&c, &[(b, 4)]).await;
        let (s, v) = c.post(&format!("/api/loading/trips/{t}/settle"), json!({ "items": [{ "item_id": it[0], "delivered": 4, "returned": 0, "damaged": 0 }],
            "customer_deliveries": [{ "customer_id": alpha, "product_id": p, "crates": 4 }] })).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        t
    };
    let g1_trip = {
        let (_, v) = c.post("/api/loading/trips", json!({ "vehicle_id": c.vehicle(100).await, "location_id": g1 })).await;
        let t = uuid(&v["id"]);
        assert_eq!(c.add(t, json!({ "batch_id": b, "crates": 7 })).await.0, StatusCode::CREATED);
        assert_eq!(c.gate(t).await.0, StatusCode::OK);
        let (_, d) = c.get(&format!("/api/loading/trips/{t}")).await;
        let (s, v) = c.post(&format!("/api/loading/trips/{t}/settle"), json!({ "items": [{ "item_id": d["items"][0]["id"], "delivered": 7, "returned": 0, "damaged": 0 }],
            "customer_deliveries": [{ "customer_id": alpha, "product_id": p, "crates": 5 }] })).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        t
    };
    // admin sees both; a user restricted to G1 sees only G1's rows, 403 for MAIN explicitly
    let (_, all) = c.get("/api/loading/deliveries").await;
    assert_eq!(all["total"], 2);
    let u = common::make_user(&c.pool, "g1_loader", false, &["loading"], &[]).await;
    sqlx::query("UPDATE users SET allowed_location_ids = $2 WHERE id = $1").bind(u).bind(json!([g1.to_string()])).execute(&c.pool).await.unwrap();
    let k = common::session_for(&c.pool, u).await;
    let rd = |uri: String| { let (app, k) = (c.app.clone(), k.clone()); async move { common::call(&app, "GET", &uri, Some(&k), None).await } };
    let (s, l) = rd("/api/loading/deliveries".into()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(l["total"], 1);
    assert_eq!(l["items"][0]["trip_id"], json!(g1_trip));
    assert_eq!(l["items"][0]["crates"], 5);
    assert!(!l.to_string().contains(&main_trip.to_string()));
    let (s, l) = rd("/api/loading/deliveries/summary".into()).await;
    assert_eq!((s, l["totals"]["crates"].as_i64()), (StatusCode::OK, Some(5)), "{l}");
    for path in ["/api/loading/deliveries", "/api/loading/deliveries/summary"] {
        let (s, _) = rd(format!("{path}?location_id={}", water_erp::stock::MAIN_LOCATION)).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
        let (s, l) = rd(format!("{path}?location_id={g1}")).await;
        assert_eq!(s, StatusCode::OK, "{l}");
    }
    let (s, _) = rd(format!("/api/loading/trips/{main_trip}")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // a user without the loading page gets nothing
    let (s, _) = common::call(&c.app, "GET", "/api/loading/deliveries", Some(&c.nobody), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // a restricted user cannot settle a foreign trip with deliveries either
    let (t, it) = dispatched(&c, &[(b, 1)]).await;
    let (s, _) = c.post_as(&k, &format!("/api/loading/trips/{t}/settle"), json!({ "items": [{ "item_id": it[0], "delivered": 1, "returned": 0, "damaged": 0 }],
        "customer_deliveries": [{ "customer_id": alpha, "product_id": p, "crates": 1 }] })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
