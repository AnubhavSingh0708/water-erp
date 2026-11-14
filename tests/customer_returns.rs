//! Feature Y: counter returns of finished goods outside trips (`/api/loading/customer-returns`).
//! Intact crates go back into FG_READY of the godown (same batch lot, never above what the batch produced), damaged crates to
//! SCRAP_YARD; large quantities need `confirm`; void only while the crates are still free; godown scoped; ledger invariant holds.
mod loading_support;
use axum::http::StatusCode;
use loading_support::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType, Move, MAIN_LOCATION};

const URI: &str = "/api/loading/customer-returns";

/// Fixture with the large-quantity percentage relaxed (default 20% would ask for confirmation on most small test batches);
/// `large_quantities_need_confirmation` uses the plain `ctx`.
async fn setup(pool: PgPool) -> Ctx {
    let c = ctx(pool).await;
    c.set("entry.large_adjust_pct", json!(1000)).await;
    c
}

/// Remove `n` crates of the batch from FG_READY of a godown (they were sold / shipped earlier).
async fn sold(c: &Ctx, loc: Uuid, product: Uuid, batch: Uuid, n: i64) {
    let mut tx = c.pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Product, product, "FG_READY", batch, Decimal::from(-n), "DISPATCH").at(loc)]).await.unwrap();
    tx.commit().await.unwrap();
}
/// APPROVED batch that produced `produced` crates of which `on_hand` are still in FG_READY at MAIN.
async fn batch_with(c: &Ctx, product: Uuid, produced: i64, on_hand: i64) -> Uuid {
    let b = c.batch(product, produced, "APPROVED", 90).await;
    if produced > on_hand { sold(c, MAIN_LOCATION, product, b, produced - on_hand).await; }
    b
}
fn line(p: Uuid, b: Uuid, intact: i64, damaged: i64) -> Value { json!({ "product_id": p, "batch_id": b, "crates_intact": intact, "crates_damaged": damaged, "reason": "customer changed mind" }) }
fn body(cust: Uuid, lines: Vec<Value>) -> Value { json!({ "customer_id": cust, "lines": lines }) }
async fn n(c: &Ctx, sql: &str) -> i64 { sqlx::query_scalar(sql).fetch_one(&c.pool).await.unwrap() }
async fn create(c: &Ctx, b: Value) -> Uuid {
    let (s, v) = c.post(URI, b).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    uuid(&v["id"])
}

#[sqlx::test(migrations = "./migrations")]
async fn return_restocks_intact_scraps_damaged_and_is_listed_and_printable(pool: PgPool) {
    let c = setup(pool).await;
    let p = c.product("BOTTLED", 12, Some(10)).await;
    let b = batch_with(&c, p, 100, 40).await;
    let alpha = c.customer("Alpha").await;
    let (s, v) = c.post(URI, json!({ "customer_id": alpha, "note": "counter", "lines": [line(p, b, 20, 3)] })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let cr_no = v["cr_no"].as_str().unwrap();
    assert!(cr_no.starts_with("CR-") && cr_no.len() == "CR-261114-0001".len() && cr_no.ends_with("-0001"), "{cr_no}");
    assert_eq!((v["status"].as_str(), v["customer_name"].as_str(), v["location_id"].clone()), (Some("ISSUED"), Some("Alpha"), json!(MAIN_LOCATION)));
    assert_eq!((v["totals"]["crates_intact"].as_i64(), v["totals"]["crates_damaged"].as_i64(), v["totals"]["bottles"].as_i64()), (Some(20), Some(3), Some(23 * 12)));
    assert_eq!((v["lines"][0]["pallets_intact"].as_i64(), v["lines"][0]["loose_intact"].as_i64()), (Some(2), Some(0)));
    assert_eq!(v["printHeader"]["title"], "CUSTOMER RETURN");
    assert_eq!(v["printHeader"]["docNo"], json!(cr_no));
    assert!(v["printHeader"]["plant_name"].is_string() && v["plant_name"].is_string());
    let id = uuid(&v["id"]);
    // stock: intact back in the same lot of FG_READY, damaged in the scrap yard, with the right ledger rows
    assert_eq!((c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, c.qty_at(MAIN_LOCATION, p, "SCRAP_YARD", b).await), (dec!(60), dec!(3)));
    let led: Vec<(String, String, Decimal)> = sqlx::query_as("SELECT txn_type, zone, qty_delta FROM stock_ledger WHERE ref_type='CUSTOMER_RETURN' AND ref_id=$1 ORDER BY zone")
        .bind(id).fetch_all(&c.pool).await.unwrap();
    assert_eq!(led, vec![("CUSTOMER_RETURN".into(), "FG_READY".into(), dec!(20)), ("CUSTOMER_RETURN_DAMAGED".into(), "SCRAP_YARD".into(), dec!(3))]);
    assert_eq!(n(&c, &format!("SELECT count(*) FROM audit_logs WHERE action='CUSTOMER_RETURN' AND entity_id='{id}'")).await, 1);
    // a walk-in party, damaged only, two lines on the same batch (aggregated into one move)
    let (s, v2) = c.post(URI, json!({ "party_name": "Walk-in Ravi", "lines": [line(p, b, 0, 2), line(p, b, 0, 1)] })).await;
    assert_eq!(s, StatusCode::CREATED, "{v2}");
    assert!(v2["cr_no"].as_str().unwrap().ends_with("-0002"));
    assert_eq!(v2["customer_id"], Value::Null);
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "SCRAP_YARD", b).await, dec!(6));
    assert_eq!(n(&c, &format!("SELECT count(*) FROM stock_ledger WHERE ref_id='{}' AND txn_type='CUSTOMER_RETURN_DAMAGED'", uuid(&v2["id"]))).await, 1);
    c.assert_ledger_consistent().await;

    // ---- reads
    let (s, l) = c.get(URI).await;
    assert_eq!((s, l["total"].as_i64(), l["items"].as_array().unwrap().len()), (StatusCode::OK, Some(2), 2), "{l}");
    assert_eq!(l["items"][0]["cr_no"], v2["cr_no"], "newest first");
    for k in ["id", "cr_no", "status", "customer_name", "party_name", "created_at", "location_name", "total_intact", "total_damaged"] { assert!(l["items"][0].get(k).is_some(), "{k}"); }
    let (_, l) = c.get(&format!("{URI}?customer_id={alpha}")).await;
    assert_eq!((l["total"].as_i64(), l["items"][0]["id"].clone()), (Some(1), json!(id)));
    let (_, l) = c.get(&format!("{URI}?status=VOID")).await;
    assert_eq!(l["total"], 0);
    let (_, l) = c.get(&format!("{URI}?status=ISSUED&q=ravi")).await;
    assert_eq!(l["total"], 1);
    let (_, l) = c.get(&format!("{URI}?from=2999-01-01")).await;
    assert_eq!(l["total"], 0);
    let (_, l) = c.get(&format!("{URI}?limit=1&offset=1")).await;
    assert_eq!((l["total"].as_i64(), l["items"].as_array().unwrap().len()), (Some(2), 1));
    assert_eq!(c.get(&format!("{URI}?status=WHATEVER")).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let today = (chrono::Utc::now() + chrono::Duration::minutes(c.st.settings.get().utc_offset_minutes as i64)).format("%Y-%m-%d").to_string();
    let (_, l) = c.get(&format!("{URI}?from={today}&to={today}")).await;
    assert_eq!(l["total"], 2);
    let (s, d) = c.get(&format!("{URI}/{id}")).await;
    assert_eq!((s, d["cr_no"].clone(), d["lines"].as_array().unwrap().len()), (StatusCode::OK, json!(cr_no), 1), "{d}");
    assert_eq!(c.get(&format!("{URI}/{}", Uuid::new_v4())).await.0, StatusCode::NOT_FOUND);
    assert_eq!(c.get(&format!("{URI}/not-a-uuid")).await.0, StatusCode::BAD_REQUEST);
    // no access without the loading page
    assert_eq!(common::call(&c.app, "GET", URI, Some(&c.nobody), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(common::call(&c.app, "POST", URI, Some(&c.nobody), Some(body(alpha, vec![line(p, b, 1, 0)]))).await.0, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "./migrations")]
async fn dry_run_leaves_no_trace_and_does_not_burn_a_number(pool: PgPool) {
    let c = setup(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = batch_with(&c, p, 100, 40).await;
    let alpha = c.customer("Alpha").await;
    let ledger_before = n(&c, "SELECT count(*) FROM stock_ledger").await;
    let audit_before = n(&c, "SELECT count(*) FROM audit_logs").await;
    for q in ["?dry_run=1", "?dry_run=true", "?dry_run", "?dry_run=yes"] {
        let (s, v) = c.post(&format!("{URI}{q}"), body(alpha, vec![line(p, b, 10, 2)])).await;
        assert_eq!(s, StatusCode::OK, "{q}: {v}");
        assert_eq!(v["dry_run"], true);
        let fx = v["effects"].as_array().unwrap();
        assert!(fx.iter().any(|e| e["txn_type"] == "CUSTOMER_RETURN" && e["zone"] == "FG_READY" && e["delta"] == "10" && e["after"] == "50"), "{v}");
        assert!(fx.iter().any(|e| e["txn_type"] == "CUSTOMER_RETURN_DAMAGED" && e["zone"] == "SCRAP_YARD" && e["delta"] == "2"), "{v}");
    }
    assert_eq!((n(&c, "SELECT count(*) FROM customer_returns").await, n(&c, "SELECT count(*) FROM customer_return_lines").await), (0, 0));
    assert_eq!((n(&c, "SELECT count(*) FROM stock_ledger").await, n(&c, "SELECT count(*) FROM audit_logs").await), (ledger_before, audit_before));
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, dec!(40));
    // dry runs return the same hard errors as the real call
    let (s, v) = c.post(&format!("{URI}?dry_run=1"), body(alpha, vec![line(p, b, 61, 0)])).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "OVER_PRODUCED"), "{v}");
    // ... and the first real document still gets number 0001 (the series has no gaps from previews / failures)
    let (s, v) = c.post(URI, body(alpha, vec![line(p, b, 61, 0)])).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "OVER_PRODUCED"), "{v}");
    let (s, v) = c.post(URI, body(alpha, vec![line(p, b, 1, 0)])).await;
    assert_eq!(s, StatusCode::CREATED);
    assert!(v["cr_no"].as_str().unwrap().ends_with("-0001"), "{v}");
}

#[sqlx::test(migrations = "./migrations")]
async fn invalid_requests_are_refused_and_change_nothing(pool: PgPool) {
    let c = setup(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let other = c.product("BOTTLED", 12, None).await;
    let jar = c.product("RETURNABLE", 1, None).await;
    let b = batch_with(&c, p, 100, 40).await;
    let ob = batch_with(&c, other, 10, 10).await;
    let jb = batch_with(&c, jar, 10, 5).await;
    let alpha = c.customer("Alpha").await;
    let gone = c.customer("Gone").await;
    sqlx::query("UPDATE customers SET is_active=false WHERE id=$1").bind(gone).execute(&c.pool).await.unwrap();
    let mk = |l: Value| json!({ "customer_id": alpha, "lines": [l] });
    let mut bad: Vec<(&str, Value, StatusCode)> = vec![
        ("returnable product", mk(line(jar, jb, 1, 0)), StatusCode::UNPROCESSABLE_ENTITY),
        ("unknown product", mk(line(Uuid::new_v4(), b, 1, 0)), StatusCode::UNPROCESSABLE_ENTITY),
        ("unknown batch", mk(line(p, Uuid::new_v4(), 1, 0)), StatusCode::UNPROCESSABLE_ENTITY),
        ("batch of another product", mk(line(p, ob, 1, 0)), StatusCode::UNPROCESSABLE_ENTITY),
        ("both zero", mk(line(p, b, 0, 0)), StatusCode::UNPROCESSABLE_ENTITY),
        ("negative intact", mk(line(p, b, -1, 0)), StatusCode::UNPROCESSABLE_ENTITY),
        ("negative damaged", mk(line(p, b, 1, -1)), StatusCode::UNPROCESSABLE_ENTITY),
        ("huge", mk(line(p, b, 1_000_001, 0)), StatusCode::UNPROCESSABLE_ENTITY),
        ("fractional", json!({ "customer_id": alpha, "lines": [{ "product_id": p, "batch_id": b, "crates_intact": 1.5, "reason": "customer changed mind" }] }), StatusCode::BAD_REQUEST),
        ("no lines", json!({ "customer_id": alpha, "lines": [] }), StatusCode::UNPROCESSABLE_ENTITY),
        ("no customer and no party", json!({ "lines": [line(p, b, 1, 0)] }), StatusCode::UNPROCESSABLE_ENTITY),
        ("blank party", json!({ "party_name": "   ", "lines": [line(p, b, 1, 0)] }), StatusCode::UNPROCESSABLE_ENTITY),
        ("unknown customer", body(Uuid::new_v4(), vec![line(p, b, 1, 0)]), StatusCode::UNPROCESSABLE_ENTITY),
        ("inactive customer", body(gone, vec![line(p, b, 1, 0)]), StatusCode::UNPROCESSABLE_ENTITY),
        ("reason too short", mk(json!({ "product_id": p, "batch_id": b, "crates_intact": 1, "reason": "ab" })), StatusCode::UNPROCESSABLE_ENTITY),
        ("reason blank", mk(json!({ "product_id": p, "batch_id": b, "crates_intact": 1, "reason": "   " })), StatusCode::UNPROCESSABLE_ENTITY),
        ("reason too long", mk(json!({ "product_id": p, "batch_id": b, "crates_intact": 1, "reason": "x".repeat(201) })), StatusCode::UNPROCESSABLE_ENTITY),
        ("reason with control chars", mk(json!({ "product_id": p, "batch_id": b, "crates_intact": 1, "reason": "bad\u{0007}reason" })), StatusCode::UNPROCESSABLE_ENTITY),
        ("missing reason", mk(json!({ "product_id": p, "batch_id": b, "crates_intact": 1 })), StatusCode::BAD_REQUEST),
        ("unknown field", json!({ "customer_id": alpha, "lines": [line(p, b, 1, 0)], "price": 5 }), StatusCode::BAD_REQUEST),
        ("unknown line field", mk(json!({ "product_id": p, "batch_id": b, "crates_intact": 1, "reason": "customer changed mind", "qr": "x" })), StatusCode::BAD_REQUEST),
        ("unknown godown", json!({ "customer_id": alpha, "lines": [line(p, b, 1, 0)], "location_id": Uuid::new_v4() }), StatusCode::UNPROCESSABLE_ENTITY),
        ("101 lines", body(alpha, (0..101).map(|_| line(p, b, 0, 1)).collect()), StatusCode::UNPROCESSABLE_ENTITY),
    ];
    bad.push(("party too long", json!({ "party_name": "p".repeat(121), "lines": [line(p, b, 1, 0)] }), StatusCode::UNPROCESSABLE_ENTITY));
    for (name, req, want) in bad {
        let (s, v) = c.post(URI, req.clone()).await;
        assert_eq!(s, want, "{name}: {v}");
        let (s, _) = c.post(&format!("{URI}?dry_run=1"), req).await;
        assert_eq!(s, want, "{name} (dry run)");
    }
    // exactly 100 lines is fine
    let mut hundred = body(alpha, (0..100).map(|_| line(p, b, 0, 1)).collect());
    hundred["confirm"] = json!(true); // 100 crates in one return is a "large quantity"
    let (s, v) = c.post(URI, hundred).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["lines"].as_array().unwrap().len(), 100);
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "SCRAP_YARD", b).await, dec!(100));
    // only that one document exists; nothing else moved
    assert_eq!(n(&c, "SELECT count(*) FROM customer_returns").await, 1);
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, dec!(40));
    assert_eq!(c.qty_at(MAIN_LOCATION, jar, "FG_READY", jb).await, dec!(5));
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn intact_crates_need_an_approved_unexpired_batch_damaged_always_ok(pool: PgPool) {
    let c = setup(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let alpha = c.customer("Alpha").await;
    let quarantine = c.batch(p, 10, "QUARANTINE", 60).await;
    let rejected = c.batch(p, 10, "REJECTED", 60).await; // its 10 crates already sit in the scrap yard
    let expired = batch_with(&c, p, 50, 20).await;
    sqlx::query("UPDATE production_batches SET expiry_date = current_date - 5 WHERE id=$1").bind(expired).execute(&c.pool).await.unwrap();
    for (name, bt, why) in [("quarantine", quarantine, "NOT_APPROVED"), ("rejected", rejected, "NOT_APPROVED"), ("expired", expired, "EXPIRED")] {
        for q in ["", "?dry_run=1"] {
            let (s, v) = c.post(&format!("{URI}{q}"), body(alpha, vec![line(p, bt, 1, 0)])).await;
            assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "NOT_RESTOCKABLE"), "{name}: {v}");
            assert_eq!(v["error"]["details"]["reason"], why, "{name}: {v}");
            assert!(v["error"]["message"].as_str().unwrap().contains("Book them as damaged"), "{v}");
            assert!(v["error"]["hint"].as_str().unwrap().contains("damaged"), "{v}");
        }
        // confirm does not override it, and a mixed line is refused as a whole (the damaged part is not booked either)
        let mut req = body(alpha, vec![line(p, bt, 1, 4)]);
        req["confirm"] = json!(true);
        let (s, v) = c.post(URI, req).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "NOT_RESTOCKABLE"), "{name} confirmed: {v}");
        let scrap_before = c.qty_at(MAIN_LOCATION, p, "SCRAP_YARD", bt).await;
        assert_eq!(scrap_before, if bt == rejected { dec!(10) } else { dec!(0) }, "{name}: atomic");
        // booked as damaged it is accepted
        let (s, v) = c.post(URI, body(alpha, vec![line(p, bt, 0, 3)])).await;
        assert_eq!(s, StatusCode::CREATED, "{name}: {v}");
        assert_eq!(c.qty_at(MAIN_LOCATION, p, "SCRAP_YARD", bt).await, scrap_before + dec!(3), "{name}");
    }
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "QC_QUARANTINE", quarantine).await, dec!(10), "nothing went to quarantine");
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "FG_READY", expired).await, dec!(20));
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn intact_crates_never_exceed_what_the_batch_produced(pool: PgPool) {
    let c = setup(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let alpha = c.customer("Alpha").await;
    let full = batch_with(&c, p, 100, 100).await;
    let b = batch_with(&c, p, 100, 90).await;
    // the hard error is reported before (and instead of) the confirmation, never overridable, also in dry runs
    for (bt, intact) in [(full, 1), (b, 11), (b, 100)] {
        for q in ["", "?dry_run=1"] {
            let (s, v) = c.post(&format!("{URI}{q}"), body(alpha, vec![line(p, bt, intact, 0)])).await;
            assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "OVER_PRODUCED"), "{v}");
            assert!(v["error"]["hint"].is_string());
        }
        let mut req = body(alpha, vec![line(p, bt, intact, 0)]);
        req["confirm"] = json!(true);
        let (s, v) = c.post(URI, req).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "OVER_PRODUCED"), "confirm does not override: {v}");
    }
    let (_, v) = c.post(URI, body(alpha, vec![line(p, b, 11, 0)])).await;
    assert_eq!((num(&v["error"]["details"]["produced"]), num(&v["error"]["details"]["on_record"]), num(&v["error"]["details"]["adding"])), (100, 90, 11), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("only produced 100 crates"), "{v}");
    // two lines on the same batch are added up: 6 + 6 > 10 of room
    let (s, v) = c.post(URI, body(alpha, vec![line(p, b, 6, 0), line(p, b, 6, 0)])).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "OVER_PRODUCED"), "{v}");
    // damaged crates are not capped by the batch's FG (they never enter finished goods), a full batch can still get damaged returns
    assert_eq!(c.post(URI, body(alpha, vec![line(p, full, 0, 4)])).await.0, StatusCode::CREATED);
    // exactly filling the batch is fine; the void then frees the room again
    let (s, v) = c.post(URI, body(alpha, vec![line(p, b, 6, 0), line(p, b, 4, 0)])).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, dec!(100));
    assert_eq!(c.post(URI, body(alpha, vec![line(p, b, 1, 0)])).await.1["error"]["code"], "OVER_PRODUCED");
    c.assert_ledger_consistent().await;
}
/// Decimal / whole number in a JSON body (decimals are strings in this API).
fn num(v: &Value) -> i64 { v.as_str().map(|s| s.parse::<Decimal>().unwrap().normalize().to_string().parse().unwrap()).or_else(|| v.as_i64()).unwrap() }

#[sqlx::test(migrations = "./migrations")]
async fn large_quantities_need_confirmation(pool: PgPool) {
    let c = ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let alpha = c.customer("Alpha").await;
    // default entry.large_adjust_pct = 20. FG 50: 10 crates = 20% is fine, 11 is "more than 20%"
    let b = batch_with(&c, p, 1000, 50).await;
    let (s, v) = c.post(URI, body(alpha, vec![line(p, b, 11, 0)])).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFIRM_REQUIRED"), "{v}");
    assert!(v["error"]["details"]["warnings"][0].as_str().unwrap().contains("20%"), "{v}");
    assert_eq!(n(&c, "SELECT count(*) FROM customer_returns").await, 0);
    // dry runs return the confirmation request unchanged; with confirm they preview
    let (s, v) = c.post(&format!("{URI}?dry_run=1"), body(alpha, vec![line(p, b, 11, 0)])).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFIRM_REQUIRED"), "{v}");
    let mut big = body(alpha, vec![line(p, b, 11, 0)]);
    big["confirm"] = json!(true);
    let (s, v) = c.post(&format!("{URI}?dry_run=1"), big.clone()).await;
    assert_eq!((s, v["dry_run"].clone()), (StatusCode::OK, json!(true)), "{v}");
    let (s, v) = c.post(URI, big).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert!(v["warnings"][0].as_str().unwrap().contains("more than 20%"), "{v}");
    // FG is now 61: 12 crates = 19.7% passes, 13 does not; damaged crates count too
    assert_eq!(c.post(URI, body(alpha, vec![line(p, b, 6, 6)])).await.0, StatusCode::CREATED);
    // FG 67: 14 would be > 20%
    let (s, v) = c.post(URI, body(alpha, vec![line(p, b, 7, 7)])).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFIRM_REQUIRED"), "{v}");
    // splitting a large return over lines cannot dodge the check (per batch totals)
    let (s, v) = c.post(URI, body(alpha, vec![line(p, b, 5, 0), line(p, b, 5, 0), line(p, b, 5, 0)])).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFIRM_REQUIRED"), "{v}");
    // >= 100 crates always needs confirmation, 99 does not (FG big enough that the percentage is not the trigger)
    let big_fg = batch_with(&c, p, 1000, 900).await;
    let (s, v) = c.post(URI, body(alpha, vec![line(p, big_fg, 0, 100)])).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFIRM_REQUIRED"), "{v}");
    assert!(v["error"]["details"]["warnings"][0].as_str().unwrap().contains("100"), "{v}");
    assert_eq!(c.post(URI, body(alpha, vec![line(p, big_fg, 90, 9)])).await.0, StatusCode::CREATED);
    // a sold-out batch (FG 0): a small return is normal, only the 100-crate rule applies
    let gone = batch_with(&c, p, 1000, 0).await;
    assert_eq!(c.post(URI, body(alpha, vec![line(p, gone, 5, 0)])).await.0, StatusCode::CREATED);
    let (s, v) = c.post(URI, body(alpha, vec![line(p, gone, 60, 40)])).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFIRM_REQUIRED"), "{v}");
    // a sensible setting is honoured
    c.set("entry.large_adjust_pct", json!(1000)).await;
    assert_eq!(c.post(URI, body(alpha, vec![line(p, gone, 50, 0)])).await.0, StatusCode::CREATED);
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn void_reverses_the_stock_and_is_audited(pool: PgPool) {
    let c = setup(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = batch_with(&c, p, 100, 40).await;
    let alpha = c.customer("Alpha").await;
    let id = create(&c, body(alpha, vec![line(p, b, 10, 3)])).await;
    let uri = format!("{URI}/{id}/void");
    // reason rules, confirmation, dry run
    assert_eq!(c.post(&uri, json!({ "reason": "no", "confirm": true })).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(c.post(&uri, json!({ "confirm": true })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(c.post(&uri, json!({ "reason": "typed wrong batch", "confirm": true, "x": 1 })).await.0, StatusCode::BAD_REQUEST);
    let (s, v) = c.post(&uri, json!({ "reason": "typed the wrong batch" })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFIRM_REQUIRED"), "{v}");
    let (s, v) = c.post(&format!("{uri}?dry_run=1"), json!({ "reason": "typed the wrong batch", "confirm": true })).await;
    assert_eq!((s, v["dry_run"].clone()), (StatusCode::OK, json!(true)), "{v}");
    assert!(v["effects"].as_array().unwrap().iter().any(|e| e["txn_type"] == "CUSTOMER_RETURN_VOID" && e["delta"] == "-10"), "{v}");
    let st: String = sqlx::query_scalar("SELECT status FROM customer_returns WHERE id=$1").bind(id).fetch_one(&c.pool).await.unwrap();
    assert_eq!(st, "ISSUED");
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, dec!(50));
    // real void
    let (s, v) = c.post(&uri, json!({ "reason": "typed the wrong batch", "confirm": true })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((v["status"].as_str(), v["void_reason"].as_str()), (Some("VOID"), Some("typed the wrong batch")));
    assert!(v["voided_at"].is_string() && v["voided_by_name"].is_string());
    assert_eq!((c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, c.qty_at(MAIN_LOCATION, p, "SCRAP_YARD", b).await), (dec!(40), dec!(0)));
    let led: Vec<(String, Decimal)> = sqlx::query_as("SELECT txn_type, qty_delta FROM stock_ledger WHERE ref_id=$1 AND txn_type='CUSTOMER_RETURN_VOID' ORDER BY zone").bind(id).fetch_all(&c.pool).await.unwrap();
    assert_eq!(led, vec![("CUSTOMER_RETURN_VOID".into(), dec!(-10)), ("CUSTOMER_RETURN_VOID".into(), dec!(-3))]);
    assert_eq!(n(&c, &format!("SELECT count(*) FROM audit_logs WHERE action='CUSTOMER_RETURN_VOID' AND entity_id='{id}'")).await, 1);
    // a double void is refused; the voided document stays listed
    let (s, v) = c.post(&uri, json!({ "reason": "typed the wrong batch", "confirm": true })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INVALID_STATE"), "{v}");
    let (_, l) = c.get(&format!("{URI}?status=VOID")).await;
    assert_eq!(l["total"], 1);
    assert_eq!(c.post(&format!("{URI}/{}/void", Uuid::new_v4()), json!({ "reason": "typed the wrong batch", "confirm": true })).await.0, StatusCode::NOT_FOUND);
    // the freed room can be used again
    assert_eq!(c.post(URI, body(alpha, vec![line(p, b, 10, 0)])).await.0, StatusCode::CREATED);
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn void_is_refused_when_the_crates_are_reserved_or_gone(pool: PgPool) {
    let c = setup(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = batch_with(&c, p, 100, 40).await;
    let alpha = c.customer("Alpha").await;
    let id = create(&c, body(alpha, vec![line(p, b, 20, 0)])).await; // 60 in FG
    let void = |reason: &'static str| { let c = &c; async move { c.post(&format!("{URI}/{id}/void"), json!({ "reason": reason, "confirm": true })).await } };
    // an open trip holds 50 of the 60: only 10 are free, the return brought 20
    let t = c.trip(c.vehicle(200).await).await;
    assert_eq!(c.add(t, json!({ "batch_id": b, "crates": 50 })).await.0, StatusCode::CREATED);
    let (s, v) = void("typed the wrong batch").await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "RESERVED_STOCK"), "{v}");
    assert_eq!(num(&v["error"]["details"]["reserved"]), 50);
    assert_eq!(v["error"]["details"]["trips"].as_array().unwrap().len(), 1);
    assert!(v["error"]["hint"].is_string());
    let (s, v) = c.post(&format!("{URI}/{id}/void?dry_run=1"), json!({ "reason": "typed the wrong batch", "confirm": true })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "RESERVED_STOCK"), "dry run: {v}");
    // not even confirm/admin changes that; the unconfirmed call reports the hard error first
    let (s, v) = c.post(&format!("{URI}/{id}/void"), json!({ "reason": "typed the wrong batch" })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "RESERVED_STOCK"), "{v}");
    let (s, v) = c.post_as(&c.admin, &format!("{URI}/{id}/void"), json!({ "reason": "typed the wrong batch", "confirm": true })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "RESERVED_STOCK"), "admin: {v}");
    // remove the crates from the trip: now it works... unless they were sold meanwhile
    let (_, trip) = c.get(&format!("/api/loading/trips/{t}")).await;
    assert_eq!(c.delete(&format!("/api/loading/trips/{t}/items/{}", trip["items"][0]["id"].as_str().unwrap())).await.0, StatusCode::OK);
    let dn = c.post("/api/loading/dispatches", json!({ "party_name": "Walk-in", "lines": [{ "batch_id": b, "crates": 55 }] })).await;
    assert_eq!(dn.0, StatusCode::CREATED, "{}", dn.1);
    let (s, v) = void("typed the wrong batch").await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("still in finished goods"), "{v}");
    assert!(v["error"]["hint"].is_string());
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, dec!(5));
    // the dispatch note is voided: the stock is back and the return can be voided
    let dn_id = dn.1["id"].as_str().unwrap().to_string();
    assert_eq!(c.post(&format!("/api/loading/dispatches/{dn_id}/void"), json!({ "reason": "wrong customer" })).await.0, StatusCode::OK);
    assert_eq!(void("typed the wrong batch").await.0, StatusCode::OK);
    // damaged crates that were disposed of from the scrap yard block the void with a clear message
    let id2 = create(&c, body(alpha, vec![line(p, b, 0, 5)])).await;
    sqlx::query("UPDATE stock_balances SET qty = qty - 2 WHERE zone='SCRAP_YARD' AND lot_id=$1").bind(b).execute(&c.pool).await.unwrap();
    let (s, v) = c.post(&format!("{URI}/{id2}/void"), json!({ "reason": "typed the wrong batch", "confirm": true })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INSUFFICIENT_STOCK"), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("scrap yard"), "{v}");
    let st: String = sqlx::query_scalar("SELECT status FROM customer_returns WHERE id=$1").bind(id2).fetch_one(&c.pool).await.unwrap();
    assert_eq!(st, "ISSUED");
}

#[sqlx::test(migrations = "./migrations")]
async fn void_window_applies_to_non_admins(pool: PgPool) {
    let c = setup(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = batch_with(&c, p, 100, 40).await;
    let alpha = c.customer("Alpha").await;
    let id = create(&c, body(alpha, vec![line(p, b, 5, 0)])).await;
    sqlx::query("UPDATE customer_returns SET created_at = now() - interval '100 hours' WHERE id=$1").bind(id).execute(&c.pool).await.unwrap();
    let req = json!({ "reason": "typed the wrong batch", "confirm": true });
    let (s, v) = c.post(&format!("{URI}/{id}/void"), req.clone()).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "FORBIDDEN"), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("Administrator"));
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, dec!(45));
    let (s, v) = c.post_as(&c.admin, &format!("{URI}/{id}/void"), req).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, dec!(40));
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_returns_cannot_jointly_exceed_the_batch(pool: PgPool) {
    let c = setup(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = batch_with(&c, p, 100, 90).await;
    let alpha = c.customer("Alpha").await;
    let mut hs = vec![];
    for _ in 0..5 {
        let (app, key, req) = (c.app.clone(), c.key.clone(), body(alpha, vec![line(p, b, 4, 0)]));
        hs.push(tokio::spawn(async move { common::call(&app, "POST", URI, Some(&key), Some(req)).await }));
    }
    let mut ok = 0;
    for h in hs { let (s, v) = h.await.unwrap(); if s == StatusCode::CREATED { ok += 1 } else { assert_eq!(code(&v), "OVER_PRODUCED", "{v}") } }
    assert_eq!(ok, 2, "90 + 4 + 4 = 98 fits, a third return would make 102 > 100");
    assert_eq!(c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, dec!(98));
    assert_eq!(n(&c, "SELECT count(*) FROM customer_returns").await, 2);
    // gap-free numbering despite the refused attempts
    let nos: Vec<String> = sqlx::query_scalar("SELECT cr_no FROM customer_returns ORDER BY cr_no").fetch_all(&c.pool).await.unwrap();
    assert!(nos[0].ends_with("-0001") && nos[1].ends_with("-0002"), "{nos:?}");
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_double_void_reverses_once(pool: PgPool) {
    let c = setup(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = batch_with(&c, p, 100, 40).await;
    let alpha = c.customer("Alpha").await;
    let id = create(&c, body(alpha, vec![line(p, b, 10, 2)])).await;
    let mut hs = vec![];
    for _ in 0..4 {
        let (app, key) = (c.app.clone(), c.key.clone());
        hs.push(tokio::spawn(async move { common::call(&app, "POST", &format!("{URI}/{id}/void"), Some(&key), Some(json!({ "reason": "typed the wrong batch", "confirm": true }))).await }));
    }
    let mut ok = 0;
    for h in hs { let (s, v) = h.await.unwrap(); if s == StatusCode::OK { ok += 1 } else { assert_eq!(code(&v), "INVALID_STATE", "{v}") } }
    assert_eq!(ok, 1);
    assert_eq!((c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await, c.qty_at(MAIN_LOCATION, p, "SCRAP_YARD", b).await), (dec!(40), dec!(0)));
    assert_eq!(n(&c, &format!("SELECT count(*) FROM stock_ledger WHERE ref_id='{id}' AND txn_type='CUSTOMER_RETURN_VOID'")).await, 2);
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn void_racing_a_trip_that_loads_the_same_crates_has_one_winner(pool: PgPool) {
    let c = setup(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let alpha = c.customer("Alpha").await;
    for round in 0..4 {
        let b = batch_with(&c, p, 100, 0).await; // sold out
        let id = create(&c, body(alpha, vec![line(p, b, 20, 0)])).await; // exactly the returned crates are on hand
        let t = c.trip(c.vehicle(100).await).await;
        let (a1, k1) = (c.app.clone(), c.key.clone());
        let add = tokio::spawn(async move { common::call(&a1, "POST", &format!("/api/loading/trips/{t}/items"), Some(&k1), Some(json!({ "batch_id": b, "crates": 20 }))).await });
        let (a2, k2) = (c.app.clone(), c.key.clone());
        let vd = tokio::spawn(async move { common::call(&a2, "POST", &format!("{URI}/{id}/void"), Some(&k2), Some(json!({ "reason": "typed the wrong batch", "confirm": true }))).await });
        let ((sa, va), (sv, vv)) = (add.await.unwrap(), vd.await.unwrap());
        let added = sa == StatusCode::CREATED;
        let voided = sv == StatusCode::OK;
        assert!(added ^ voided, "round {round}: exactly one may win: add {sa} {va} / void {sv} {vv}");
        if added { assert_eq!(code(&vv), "RESERVED_STOCK", "{vv}"); } else { assert_eq!(code(&va), "INSUFFICIENT_STOCK", "{va}"); }
        let on_hand = c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await;
        let reserved: Decimal = sqlx::query_scalar("SELECT COALESCE(SUM(ti.crates),0)::numeric FROM trip_items ti JOIN loading_trips t ON t.id=ti.trip_id WHERE t.status='LOADING' AND ti.batch_id=$1")
            .bind(b).fetch_one(&c.pool).await.unwrap();
        assert!(reserved <= on_hand, "round {round}: reserved {reserved} > on hand {on_hand}");
    }
    c.assert_ledger_consistent().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn returns_are_per_godown_and_godown_scoped(pool: PgPool) {
    let c = setup(pool).await;
    let g1 = c.godown("G1").await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = batch_with(&c, p, 100, 30).await; // 30 at MAIN
    let alpha = c.customer("Alpha").await;
    // a return booked at G1 lands in G1 only (the cap counts the batch over all godowns: 30 on record, 100 produced)
    let mut req = body(alpha, vec![line(p, b, 20, 5)]);
    req["location_id"] = json!(g1);
    let (s, v) = c.post(URI, req).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((v["location_id"].clone(), v["location_name"].as_str()), (json!(g1), Some("G1")));
    let g1_id = uuid(&v["id"]);
    assert_eq!((c.qty_at(g1, p, "FG_READY", b).await, c.qty_at(g1, p, "SCRAP_YARD", b).await, c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await), (dec!(20), dec!(5), dec!(30)));
    // cross-godown cap: 30 + 20 = 50 on record, +51 would exceed the 100 produced even at the other godown
    let (s, v) = c.post(URI, body(alpha, vec![line(p, b, 51, 0)])).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "OVER_PRODUCED"), "{v}");
    let main_id = create(&c, body(alpha, vec![line(p, b, 10, 0)])).await;
    // a user restricted to G1: sees and voids only G1 documents, MAIN is 403
    let u = common::make_user(&c.pool, "g1_clerk", false, &["loading"], &[]).await;
    sqlx::query("UPDATE users SET allowed_location_ids = $2 WHERE id = $1").bind(u).bind(json!([g1.to_string()])).execute(&c.pool).await.unwrap();
    let k = common::session_for(&c.pool, u).await;
    let rd = |m: &'static str, uri: String, b: Option<Value>| { let (app, k) = (c.app.clone(), k.clone()); async move { common::call(&app, m, &uri, Some(&k), b).await } };
    let (s, l) = rd("GET", URI.into(), None).await;
    assert_eq!((s, l["total"].as_i64()), (StatusCode::OK, Some(1)), "{l}");
    assert_eq!(l["items"][0]["id"], json!(g1_id));
    assert_eq!(rd("GET", format!("{URI}?location_id={MAIN_LOCATION}"), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(rd("GET", format!("{URI}/{main_id}"), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(rd("GET", format!("{URI}/{g1_id}"), None).await.0, StatusCode::OK);
    let (s, _) = rd("POST", format!("{URI}/{main_id}/void"), Some(json!({ "reason": "typed the wrong batch", "confirm": true }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let mut foreign = body(alpha, vec![line(p, b, 1, 0)]);
    foreign["location_id"] = json!(MAIN_LOCATION);
    assert_eq!(rd("POST", URI.into(), Some(foreign)).await.0, StatusCode::FORBIDDEN);
    // default godown of a restricted user = their own
    let (s, v) = rd("POST", URI.into(), Some(body(alpha, vec![line(p, b, 0, 1)]))).await;
    assert_eq!((s, v["location_id"].clone()), (StatusCode::CREATED, json!(g1)), "{v}");
    // voiding at G1 touches only G1
    let (s, v) = rd("POST", format!("{URI}/{g1_id}/void"), Some(json!({ "reason": "typed the wrong batch", "confirm": true }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((c.qty_at(g1, p, "FG_READY", b).await, c.qty_at(MAIN_LOCATION, p, "FG_READY", b).await), (dec!(0), dec!(40)));
    c.assert_ledger_consistent().await;
}
