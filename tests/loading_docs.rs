mod loading_support;
use axum::http::StatusCode;
use loading_support::*;
use rust_decimal_macros::dec;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test(migrations = "./migrations")]
async fn dispatch_note_is_reservation_aware_fefo_and_voidable(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, Some(60)).await;
    let (b1, b2) = (c.batch(p, 6, "APPROVED", 30).await, c.batch(p, 10, "APPROVED", 60).await);
    let q = c.batch(p, 50, "QUARANTINE", 20).await;
    let cust = c.customer("Hotel Sun").await;
    // a trip reserves 5 of b1 and 8 of b2 => free: 1 + 2
    let t = c.trip(c.vehicle(100).await).await;
    assert_eq!(c.add(t, json!({ "batch_id": b1, "crates": 5 })).await.0, StatusCode::CREATED);
    assert_eq!(c.add(t, json!({ "batch_id": b2, "crates": 8 })).await.0, StatusCode::CREATED);
    let (s, v) = c.post("/api/loading/dispatches", json!({ "customer_id": cust, "lines": [{ "product_id": p, "crates": 4 }] })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}");
    assert_eq!(v["error"]["details"]["available"], 3);
    // quarantine stock can never be dispatched, strict or not
    c.set("loading.strict_qc_lock", json!(false)).await;
    let (s, v) = c.post("/api/loading/dispatches", json!({ "party_name": "x", "lines": [{ "batch_id": q, "crates": 1 }] })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "QC_LOCKED"), "{v}");
    // validation
    for bad in [json!({ "lines": [{ "product_id": p, "crates": 1 }] }), json!({ "party_name": "x", "lines": [] }),
                json!({ "party_name": "x", "lines": [{ "product_id": p, "crates": 0 }] }),
                json!({ "party_name": "x", "lines": [{ "product_id": p, "batch_id": b1, "crates": 1 }] }),
                json!({ "party_name": "x", "price": 5, "lines": [{ "product_id": p, "crates": 1 }] })] {
        let (s, _) = c.post("/api/loading/dispatches", bad.clone()).await;
        assert!(s == StatusCode::UNPROCESSABLE_ENTITY || s == StatusCode::BAD_REQUEST, "{bad} -> {s}");
    }
    // dry run: effects, no trace, no number burnt
    let ok_body = json!({ "customer_id": cust, "vehicle_no": "MH12AB1234", "note": "pick-up", "lines": [{ "product_id": p, "crates": 3 }] });
    let (s, d) = c.post("/api/loading/dispatches?dry_run=1", ok_body.clone()).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["dry_run"], true);
    assert_eq!(d["effects"].as_array().unwrap().len(), 2);
    assert_eq!(c.qty(p, "FG_READY", b1).await, dec!(6));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM dispatch_notes").fetch_one(&c.pool).await.unwrap();
    assert_eq!(n, 0);
    // real
    let (s, v) = c.post("/api/loading/dispatches", ok_body).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let dn = v["id"].as_str().unwrap().to_string();
    assert!(v["dn_no"].as_str().unwrap().starts_with("DN-"));
    assert_eq!(v["lines"].as_array().unwrap().len(), 2, "FEFO spans b1 (1 free) and b2 (2 free)");
    assert_eq!(v["totals"]["crates"], 3);
    assert_eq!(c.qty(p, "FG_READY", b1).await, dec!(5));
    assert_eq!(c.qty(p, "FG_READY", b2).await, dec!(8));
    // nothing free any more: the trip's reservation is intact
    let (s, _) = c.post("/api/loading/dispatches", json!({ "party_name": "x", "lines": [{ "product_id": p, "crates": 1 }] })).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(c.gate(t).await.0, StatusCode::OK, "the reserved trip can still be dispatched");
    // list / detail
    let (_, l) = c.get("/api/loading/dispatches?q=pick").await;
    assert_eq!(l["total"], 0);
    let (_, l) = c.get("/api/loading/dispatches?q=Hotel").await;
    assert_eq!(l["total"], 1);
    assert_eq!(c.get(&format!("/api/loading/dispatches/{dn}")).await.1["customer_name"], "Hotel Sun");
    // void restocks the same lots; second void fails
    let uri = format!("/api/loading/dispatches/{dn}/void");
    assert_eq!(c.post(&uri, json!({ "reason": "x" })).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, v) = c.post(&uri, json!({ "reason": "customer cancelled" })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "VOID");
    assert_eq!((c.qty(p, "FG_READY", b1).await, c.qty(p, "FG_READY", b2).await), (dec!(1), dec!(2)), "the same lots are restocked (trip took the rest)");
    let (s, v) = c.post(&uri, json!({ "reason": "customer cancelled" })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INVALID_STATE"));
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn dispatch_void_window_and_returnable_rules(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 10, "APPROVED", 60).await;
    let (s, v) = c.post("/api/loading/dispatches", json!({ "party_name": "Walk-in", "lines": [{ "batch_id": b, "crates": 4 }] })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let dn = v["id"].as_str().unwrap().to_string();
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(6));
    sqlx::query("UPDATE dispatch_notes SET created_at = now() - interval '13 hours'").execute(&c.pool).await.unwrap();
    let uri = format!("/api/loading/dispatches/{dn}/void");
    let (s, v) = c.post(&uri, json!({ "reason": "wrong party" })).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "FORBIDDEN"), "{v}");
    let (s, v) = c.post_as(&c.admin, &uri, json!({ "reason": "wrong party" })).await;
    assert_eq!(s, StatusCode::OK, "admin may void outside the window: {v}");
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(10));
    // returnable containers go through Jars, not dispatch notes
    let jar = c.product("RETURNABLE", 1, None).await;
    let jb = c.batch(jar, 10, "APPROVED", 60).await;
    let (s, _) = c.post("/api/loading/dispatches", json!({ "party_name": "x", "lines": [{ "batch_id": jb, "crates": 1 }] })).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = c.post("/api/loading/dispatches", json!({ "party_name": "x", "lines": [{ "product_id": jar, "crates": 1 }] })).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action IN ('DISPATCH_NOTE','DISPATCH_VOID')").fetch_one(&c.pool).await.unwrap();
    assert_eq!(audit, 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn gate_pass_void_restocks_and_cancels(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let (b1, b2) = (c.batch(p, 10, "APPROVED", 30).await, c.batch(p, 10, "APPROVED", 60).await);
    let t = c.trip(c.vehicle(100).await).await;
    assert_eq!(c.add(t, json!({ "product_id": p, "crates": 14 })).await.0, StatusCode::CREATED);
    // cannot void a trip that has not left
    let uri = format!("/api/loading/trips/{t}/void");
    let (s, v) = c.post(&uri, json!({ "reason": "changed my mind", "confirm": true })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INVALID_STATE"), "{v}");
    assert_eq!(c.gate(t).await.0, StatusCode::OK);
    assert_eq!((c.qty(p, "FG_READY", b1).await, c.qty(p, "FG_READY", b2).await), (dec!(0), dec!(6)));
    // confirmation is always required
    let (s, v) = c.post(&uri, json!({ "reason": "loaded the wrong goods" })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFIRM_REQUIRED"), "{v}");
    // reason is mandatory
    assert_eq!(c.post(&uri, json!({ "reason": "no", "confirm": true })).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    // dry run leaves no trace
    let (s, d) = c.post(&format!("{uri}?dry_run=1"), json!({ "reason": "loaded the wrong goods", "confirm": true })).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(c.qty(p, "FG_READY", b1).await, dec!(0));
    // window: a non-admin is blocked after the window, Admin is not
    sqlx::query("UPDATE loading_trips SET gate_pass_issued_at = now() - interval '20 hours'").execute(&c.pool).await.unwrap();
    let (s, v) = c.post(&uri, json!({ "reason": "loaded the wrong goods", "confirm": true })).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "FORBIDDEN"), "{v}");
    let (s, v) = c.post_as(&c.admin, &uri, json!({ "reason": "loaded the wrong goods", "confirm": true })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "CANCELLED");
    assert_eq!(v["void_reason"], "loaded the wrong goods");
    assert_eq!((c.qty(p, "FG_READY", b1).await, c.qty(p, "FG_READY", b2).await), (dec!(10), dec!(10)), "each lot restocked exactly");
    // a voided trip cannot be settled, voided again, or gate-passed again
    assert_eq!(c.post_as(&c.admin, &uri, json!({ "reason": "loaded the wrong goods", "confirm": true })).await.0, StatusCode::CONFLICT);
    assert_eq!(c.gate(t).await.0, StatusCode::CONFLICT);
    let (s, _) = c.post(&format!("/api/loading/trips/{t}/settle"), json!({ "items": [] })).await;
    assert!(s == StatusCode::CONFLICT || s == StatusCode::UNPROCESSABLE_ENTITY);
    // stock reserved again?  no: the trip is CANCELLED, crates are free
    let (_, av) = c.get(&format!("/api/loading/availability?product_id={p}")).await;
    assert_eq!(av[0]["available"], 10);
    // a settled trip cannot be voided
    let t2 = c.trip(c.vehicle(100).await).await;
    assert_eq!(c.add(t2, json!({ "batch_id": b1, "crates": 2 })).await.0, StatusCode::CREATED);
    assert_eq!(c.gate(t2).await.0, StatusCode::OK);
    let item = c.get(&format!("/api/loading/trips/{t2}")).await.1["items"][0]["id"].clone();
    assert_eq!(c.post(&format!("/api/loading/trips/{t2}/settle"), json!({ "items": [{ "item_id": item, "delivered": 2, "returned": 0, "damaged": 0 }] })).await.0, StatusCode::OK);
    let (s, v) = c.post_as(&c.admin, &format!("/api/loading/trips/{t2}/void"), json!({ "reason": "too late now", "confirm": true })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INVALID_STATE"));
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn dry_run_on_items_and_gate_pass_leaves_no_trace(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 10, "APPROVED", 60).await;
    let t = c.trip(c.vehicle(100).await).await;
    let (s, d) = c.post(&format!("/api/loading/trips/{t}/items?dry_run=1"), json!({ "product_id": p, "crates": 4 })).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["dry_run"], true);
    assert_eq!(d["effects"].as_array().unwrap().len(), 1);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM trip_items").fetch_one(&c.pool).await.unwrap();
    assert_eq!(n, 0);
    // dry run reports the same error as the real call
    let (s, v) = c.post(&format!("/api/loading/trips/{t}/items?dry_run=1"), json!({ "batch_id": b, "crates": 11 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"));
    assert_eq!(c.add(t, json!({ "batch_id": b, "crates": 4 })).await.0, StatusCode::CREATED);
    let (s, d) = c.post(&format!("/api/loading/trips/{t}/gate-pass?dry_run=true"), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["effects"][0]["delta"], "-4");
    assert_eq!(d["effects"][0]["after"], "6");
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(10));
    let (st, gp): (String, Option<String>) = sqlx::query_as("SELECT status, gate_pass_no FROM loading_trips WHERE id=$1").bind(t).fetch_one(&c.pool).await.unwrap();
    assert_eq!((st.as_str(), gp), ("LOADING", None));
    let a: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts").fetch_one(&c.pool).await.unwrap();
    let l: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_ledger WHERE txn_type='GATE_PASS'").fetch_one(&c.pool).await.unwrap();
    assert_eq!((a, l), (0, 0));
    // the gap-free GP series was not burnt by the dry run
    assert_eq!(c.gate(t).await.0, StatusCode::OK);
    let gp: String = sqlx::query_scalar("SELECT gate_pass_no FROM loading_trips WHERE id=$1").bind(t).fetch_one(&c.pool).await.unwrap();
    assert!(gp.ends_with("-0001"), "{gp}");
}
