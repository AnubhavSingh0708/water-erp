mod loading_support;
use axum::http::StatusCode;
use loading_support::*;
use rust_decimal_macros::dec;
use serde_json::json;
use sqlx::PgPool;
use water_erp::stock::MAIN_LOCATION;

#[sqlx::test(migrations = "./migrations")]
async fn stock_is_per_godown_and_reservations_do_not_cross(pool: PgPool) {
    let c = ctx(pool).await;
    let g1 = c.godown("G1").await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 10, "APPROVED", 60).await; // 10 at MAIN
    c.stock_at(g1, p, b, 8).await;                 // 8 of the same batch in G1
    let p2 = c.product("BOTTLED", 12, None).await;
    let b2: uuid::Uuid = sqlx::query_scalar("INSERT INTO production_batches (batch_no,product_id,crates,qc_status,expiry_date) VALUES ('B'||floor(random()*1e9)::text,$1,5,'APPROVED',current_date+60) RETURNING id")
        .bind(p2).fetch_one(&c.pool).await.unwrap();
    c.stock_at(g1, p2, b2, 5).await;               // product that exists ONLY in G1
    let (tm, tg) = (c.trip(c.vehicle(100).await).await, {
        let (s, v) = c.post("/api/loading/trips", json!({ "vehicle_id": c.vehicle(100).await, "location_id": g1 })).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        assert_eq!(v["location_id"], json!(g1));
        assert_eq!(v["location_name"], "G1");
        uuid(&v["id"])
    });
    // a MAIN trip cannot load what only G1 holds (both modes), nor more than MAIN holds
    let (s, v) = c.add(tm, json!({ "batch_id": b2, "crates": 1 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}");
    let (s, v) = c.add(tm, json!({ "product_id": p2, "crates": 1 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}");
    let (s, v) = c.add(tm, json!({ "batch_id": b, "crates": 11 })).await;
    assert_eq!((s, code(&v), v["error"]["details"]["available"].as_i64()), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK", Some(10)), "{v}");
    assert_eq!(c.add(tm, json!({ "batch_id": b, "crates": 10 })).await.0, StatusCode::CREATED);
    // the G1 trip has its own reservations: MAIN's 10 reserved crates do not reduce G1's 8
    let (s, v) = c.add(tg, json!({ "product_id": p, "crates": 8 })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["trip"]["location_id"], json!(g1));
    let (s, v) = c.add(tg, json!({ "batch_id": b, "crates": 1 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}");
    assert_eq!(c.add(tg, json!({ "product_id": p2, "crates": 5 })).await.0, StatusCode::CREATED);
    // availability follows the godown (explicit, or the trip's)
    let (_, av) = c.get(&format!("/api/loading/availability?product_id={p}&location_id={g1}")).await;
    assert_eq!((av[0]["fg_ready"].as_i64(), av[0]["reserved"].as_i64(), av[0]["available"].as_i64()), (Some(8), Some(8), Some(0)));
    assert_eq!(av[0]["location_name"], "G1");
    let (_, av) = c.get(&format!("/api/loading/availability?product_id={p}")).await;
    assert_eq!((av[0]["fg_ready"].as_i64(), av[0]["reserved"].as_i64()), (Some(10), Some(10)));
    let (_, av) = c.get(&format!("/api/loading/availability?product_id={p2}&trip_id={tg}")).await;
    assert_eq!(av[0]["in_trip"], 5);
    let (_, av) = c.get(&format!("/api/loading/availability?product_id={p2}")).await;
    assert!(av.as_array().unwrap().is_empty(), "nothing of p2 at MAIN");
    // a dispatch note from MAIN cannot take G1 stock and respects MAIN's reservation
    let (s, v) = c.post("/api/loading/dispatches", json!({ "party_name": "x", "lines": [{ "product_id": p2, "crates": 1 }] })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}");
    let (s, v) = c.post("/api/loading/dispatches", json!({ "party_name": "x", "location_id": g1, "lines": [{ "batch_id": b, "crates": 1 }] })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "G1's 8 crates are reserved by the G1 trip: {v}");
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn gate_pass_void_settle_and_dispatch_act_on_their_godown_only(pool: PgPool) {
    let c = ctx(pool).await;
    let g1 = c.godown("G1").await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 20, "APPROVED", 60).await;
    c.stock_at(g1, p, b, 20).await;
    let mk = |loc: uuid::Uuid| { let c = &c; async move {
        let (_, v) = c.post("/api/loading/trips", json!({ "vehicle_id": c.vehicle(100).await, "location_id": loc })).await;
        uuid(&v["id"]) } };
    let t = mk(g1).await;
    assert_eq!(c.add(t, json!({ "batch_id": b, "crates": 12 })).await.0, StatusCode::CREATED);
    let (s, v) = c.gate(t).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["location_name"], "G1");
    assert_eq!((c.qty_at(g1, p, "FG_READY", b).await, c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await), (dec!(8), dec!(20)), "only G1 is deducted");
    let led: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_ledger WHERE ref_id=$1 AND txn_type='GATE_PASS' AND location_id=$2").bind(t).bind(g1).fetch_one(&c.pool).await.unwrap();
    assert_eq!(led, 1);
    let (_, gp) = c.get(&format!("/api/loading/trips/{t}/gate-pass")).await;
    assert_eq!(gp["location_name"], "G1");
    // settle: 7 delivered, 3 returned, 2 damaged -> returns/scrap land in G1
    let item = c.get(&format!("/api/loading/trips/{t}")).await.1["items"][0]["id"].clone();
    let (s, v) = c.post(&format!("/api/loading/trips/{t}/settle"), json!({ "confirm": true, "items": [{ "item_id": item, "delivered": 7, "returned": 3, "damaged": 2 }] })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((c.qty_at(g1, p, "FG_READY", b).await, c.qty_at(g1, p, "SCRAP_YARD", b).await), (dec!(11), dec!(2)));
    assert_eq!((c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, c.qty_at(MAIN_LOCATION, p, "SCRAP_YARD", b).await), (dec!(20), dec!(0)));
    // void in G1 restocks G1
    let t2 = mk(g1).await;
    assert_eq!(c.add(t2, json!({ "batch_id": b, "crates": 5 })).await.0, StatusCode::CREATED);
    assert_eq!(c.gate(t2).await.0, StatusCode::OK);
    assert_eq!(c.qty_at(g1, p, "FG_READY", b).await, dec!(6));
    let (s, v) = c.post(&format!("/api/loading/trips/{t2}/void"), json!({ "reason": "wrong truck", "confirm": true })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((c.qty_at(g1, p, "FG_READY", b).await, c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await), (dec!(11), dec!(20)));
    // dispatch note from G1 and its void
    let (s, v) = c.post("/api/loading/dispatches", json!({ "party_name": "Walk-in", "location_id": g1, "lines": [{ "product_id": p, "crates": 4 }] })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((v["location_id"].clone(), v["location_name"].clone()), (json!(g1), json!("G1")));
    assert_eq!((c.qty_at(g1, p, "FG_READY", b).await, c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await), (dec!(7), dec!(20)));
    let dn = v["id"].as_str().unwrap().to_string();
    assert_eq!(c.post(&format!("/api/loading/dispatches/{dn}/void"), json!({ "reason": "customer cancelled" })).await.0, StatusCode::OK);
    assert_eq!((c.qty_at(g1, p, "FG_READY", b).await, c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await), (dec!(11), dec!(20)));
    // trip location is immutable: no way to change it through the API
    let (s, _) = c.post(&format!("/api/loading/trips/{t}/items"), json!({ "batch_id": b, "crates": 1, "location_id": MAIN_LOCATION })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn godown_restricted_users_are_blocked_and_filtered(pool: PgPool) {
    let c = ctx(pool).await;
    let g1 = c.godown("G1").await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 20, "APPROVED", 60).await;
    c.stock_at(g1, p, b, 20).await;
    let u = common::make_user(&c.pool, "g1loader", false, &["loading"], &[]).await;
    sqlx::query("UPDATE users SET allowed_location_ids=$2 WHERE id=$1").bind(u).bind(json!([g1])).execute(&c.pool).await.unwrap();
    let k = common::session_for(&c.pool, u).await;
    let call = |m: &'static str, uri: String, body: Option<serde_json::Value>| { let (app, k) = (c.app.clone(), k.clone()); async move { common::call(&app, m, &uri, Some(&k), body).await } };
    // a MAIN trip made by the unrestricted user
    let tm = c.trip(c.vehicle(100).await).await;
    assert_eq!(c.add(tm, json!({ "batch_id": b, "crates": 3 })).await.0, StatusCode::CREATED);
    // restricted user: default godown = G1; explicit MAIN = 403
    let (s, v) = call("POST", "/api/loading/trips".into(), Some(json!({ "vehicle_id": c.vehicle(100).await }))).await;
    assert_eq!((s, v["location_id"].clone()), (StatusCode::CREATED, json!(g1)), "{v}");
    let tg = uuid(&v["id"]);
    let (s, _) = call("POST", "/api/loading/trips".into(), Some(json!({ "vehicle_id": c.vehicle(100).await, "location_id": MAIN_LOCATION }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // cannot touch the MAIN trip in any way
    for (m, uri, body) in [
        ("GET", format!("/api/loading/trips/{tm}"), None),
        ("GET", format!("/api/loading/trips/{tm}/gate-pass"), None),
        ("POST", format!("/api/loading/trips/{tm}/items"), Some(json!({ "batch_id": b, "crates": 1 }))),
        ("POST", format!("/api/loading/trips/{tm}/gate-pass"), Some(json!({}))),
        ("POST", format!("/api/loading/trips/{tm}/cancel"), Some(json!({}))),
        ("POST", format!("/api/loading/trips/{tm}/settle"), Some(json!({ "items": [] }))),
        ("POST", format!("/api/loading/trips/{tm}/void"), Some(json!({ "reason": "nope nope", "confirm": true }))),
        ("GET", format!("/api/loading/availability?location_id={MAIN_LOCATION}"), None),
        ("GET", format!("/api/loading/trips?location_id={MAIN_LOCATION}"), None),
        ("POST", "/api/loading/dispatches".to_string(), Some(json!({ "party_name": "x", "location_id": MAIN_LOCATION, "lines": [{ "batch_id": b, "crates": 1 }] }))),
    ] {
        let (s, v) = call(m, uri.clone(), body).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {uri}: {v}");
    }
    // lists are silently filtered to G1; the MAIN trip is invisible, G1 work succeeds
    let (_, l) = call("GET", "/api/loading/trips".into(), None).await;
    assert_eq!(l["total"], 1);
    assert_eq!(l["items"][0]["id"], json!(tg));
    let (_, l) = c.get("/api/loading/trips").await;
    assert_eq!(l["total"], 2, "unrestricted user sees both");
    let (s, v) = call("POST", format!("/api/loading/trips/{tg}/items"), Some(json!({ "product_id": p, "crates": 20 }))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = call("POST", "/api/loading/dispatches".into(), Some(json!({ "party_name": "x", "lines": [{ "product_id": p, "crates": 1 }] }))).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "G1 fully reserved: {v}");
    assert_eq!(call("POST", format!("/api/loading/trips/{tg}/gate-pass"), Some(json!({}))).await.0, StatusCode::OK);
    let (_, s1) = call("GET", "/api/loading/summary".into(), None).await;
    assert_eq!((s1["open_trips"].as_i64(), s1["on_road"].as_i64()), (Some(0), Some(1)), "summary counts only G1");
    let (_, av) = call("GET", "/api/loading/availability".into(), None).await;
    assert!(av.as_array().unwrap().is_empty());
    c.assert_ledger_consistent().await;
}
