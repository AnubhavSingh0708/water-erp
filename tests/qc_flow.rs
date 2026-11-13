mod common;
mod materials_fixtures;
use materials_fixtures::*;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType, Move};

async fn new_batch(c: &Ctx, crates: i64) -> (water_erp_ids::Ids, Value) {
    let p = plant(&c.pool).await;
    stock_plant(&c.pool, &p).await;
    fresh_water_log(&c.pool).await;
    let (s, b) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": crates, "shift": "DAY" })).await;
    assert_eq!(s, 201, "{b}");
    (water_erp_ids::Ids { product: p.p500, batch: uid(&b["id"]) }, b)
}

mod water_erp_ids { pub struct Ids { pub product: uuid::Uuid, pub batch: uuid::Uuid } }

async fn add_set(pool: &PgPool, ids: &water_erp_ids::Ids, label: &str, status: &str) {
    sqlx::query("INSERT INTO qr_crate_sets (code, batch_id, product_id, set_label, seq, original_crates, crates, status) VALUES ($1,$2,$3,$4,1,5,5,$5)")
        .bind(Uuid::new_v4().simple().to_string()[..10].to_string()).bind(ids.batch).bind(ids.product).bind(label).bind(status).execute(pool).await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn qc_approve_rules_and_stock_release(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let (ids, _) = new_batch(&c, 10).await;
    let bid = ids.batch;
    add_set(&pool, &ids, "X", "QUARANTINE").await;
    let url = format!("/api/qc/batches/{bid}");

    // no test yet / failed latest test => INVALID_STATE
    let (s, e) = c.post(&format!("{url}/approve"), json!({})).await;
    assert_eq!((s, code(&e)), (409, "INVALID_STATE"), "{e}");
    assert!(e["error"]["message"].as_str().unwrap().contains("QC test required"));
    let (s, _) = c.post(&format!("{url}/tests"), json!({ "ph": 7.1, "tds_ppm": 120, "result": "FAIL", "remarks": "cloudy" })).await;
    assert_eq!(s, 201);
    let (s, e) = c.post(&format!("{url}/approve"), json!({})).await;
    assert_eq!((s, code(&e)), (409, "INVALID_STATE"), "{e}");
    // validation of the test form
    for bad in [json!({ "result": "MAYBE" }), json!({ "result": "PASS", "ph": 20 }), json!({ "result": "PASS", "tds_ppm": -1 }), json!({ "result": "PASS", "qc_status": "APPROVED" })] {
        let (s, _) = c.post(&format!("{url}/tests"), bad).await;
        assert!(s == 422 || s == 400);
    }
    let (s, _) = c.post(&format!("{url}/tests"), json!({ "ph": 7.1, "tds_ppm": 120, "microbial_ok": true, "seal_ok": true, "label_ok": true, "result": "PASS" })).await;
    assert_eq!(s, 201);

    // part of the crates already left quarantine (non-strict loading): only the REMAINING balance is released
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Product, ids.product, "QC_QUARANTINE", bid, d("-4"), "GATE_PASS")]).await.unwrap();
    tx.commit().await.unwrap();
    let (s, v) = c.post(&format!("{url}/approve"), json!({ "notes": "ok" })).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["qc_status"], "APPROVED");
    assert_eq!(lot_qty(&pool, "PRODUCT", ids.product, "FG_READY", bid).await, d("6"));
    assert_eq!(lot_qty(&pool, "PRODUCT", ids.product, "QC_QUARANTINE", bid).await, d("0"));
    let set: String = sqlx::query_scalar("SELECT status FROM qr_crate_sets WHERE batch_id=$1").bind(bid).fetch_one(&pool).await.unwrap();
    assert_eq!(set, "AVAILABLE");
    let by: Option<Uuid> = sqlx::query_scalar("SELECT qc_decided_by FROM production_batches WHERE id=$1").bind(bid).fetch_one(&pool).await.unwrap();
    assert_eq!(by, Some(c.admin));
    let exp: Option<chrono::NaiveDate> = sqlx::query_scalar("SELECT expiry_date FROM stock_balances WHERE zone='FG_READY' AND lot_id=$1").bind(bid).fetch_one(&pool).await.unwrap();
    assert!(exp.is_some(), "FEFO expiry must travel with the release");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='QC_APPROVED'").fetch_one(&pool).await.unwrap();
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='QC_PENDING' AND resolved_at IS NULL").fetch_one(&pool).await.unwrap();
    assert_eq!((n, open), (1, 0));
    let a: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='QC_APPROVE'").fetch_one(&pool).await.unwrap();
    assert_eq!(a, 1);

    // final: no second decision, no reversal, no more tests
    for (path, body) in [("approve", json!({})), ("reject", json!({ "reason": "changed my mind" })), ("tests", json!({ "result": "PASS" }))] {
        let (s, e) = c.post(&format!("{url}/{path}"), body).await;
        assert_eq!((s, code(&e)), (409, "INVALID_STATE"), "{path}: {e}");
    }
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn qc_reject_moves_to_scrap_and_flips_sets(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let (ids, _) = new_batch(&c, 10).await;
    add_set(&pool, &ids, "X", "QUARANTINE").await;
    add_set(&pool, &ids, "Y", "AVAILABLE").await;
    add_set(&pool, &ids, "Z", "LOADED").await;
    let url = format!("/api/qc/batches/{}", ids.batch);
    let (s, _) = c.post(&format!("{url}/reject"), json!({ "reason": "bad" })).await;
    assert_eq!(s, 422);
    let (s, _) = c.post(&format!("{url}/reject"), json!({})).await;
    assert_eq!(s, 400);
    let (s, v) = c.post(&format!("{url}/reject"), json!({ "reason": "turbidity too high" })).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["qc_status"], "REJECTED");
    assert_eq!(lot_qty(&pool, "PRODUCT", ids.product, "SCRAP_YARD", ids.batch).await, d("10"));
    assert_eq!(qty(&pool, "PRODUCT", ids.product, "FG_READY").await, d("0"));
    let sets: Vec<(String, String)> = sqlx::query_as("SELECT set_label, status FROM qr_crate_sets ORDER BY set_label").fetch_all(&pool).await.unwrap();
    assert_eq!(sets, vec![("X".into(), "REJECTED".into()), ("Y".into(), "REJECTED".into()), ("Z".into(), "LOADED".into())]);
    let (s, e) = c.post(&format!("{url}/approve"), json!({})).await;
    assert_eq!((s, code(&e)), (409, "INVALID_STATE"));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='QC_REJECTED'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    // lists
    let (_, q) = c.get("/api/qc/batches").await;
    assert_eq!(q["total"], 0);
    let (_, q) = c.get("/api/qc/batches?status=REJECTED").await;
    assert_eq!(q["total"], 1);
    let (_, q) = c.get("/api/qc/batches?status=ALL").await;
    assert_eq!(q["items"][0]["last_test_result"], Value::Null);
    let (s, e) = c.get("/api/qc/batches?status=WHATEVER").await;
    assert_eq!((s, code(&e)), (422, "VALIDATION"));
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_decisions_have_exactly_one_winner(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let (ids, _) = new_batch(&c, 10).await;
    let url = format!("/api/qc/batches/{}", ids.batch);
    let (s, _) = c.post(&format!("{url}/tests"), json!({ "result": "PASS" })).await;
    assert_eq!(s, 201);
    let mut hs = vec![];
    for i in 0..8 {
        let app = c.app.clone();
        let key = c.key.clone();
        let u = url.clone();
        hs.push(tokio::spawn(async move {
            if i % 2 == 0 { call(&app, "POST", &format!("{u}/approve"), Some(&key), Some(json!({}))).await }
            else { call(&app, "POST", &format!("{u}/reject"), Some(&key), Some(json!({ "reason": "contaminated sample" }))).await }
        }));
    }
    let mut wins = 0;
    for h in hs {
        let (s, v) = h.await.unwrap();
        if s == 200 { wins += 1 } else { assert_eq!((s, code(&v)), (409, "INVALID_STATE"), "{v}") }
    }
    assert_eq!(wins, 1);
    let total = qty(&pool, "PRODUCT", ids.product, "FG_READY").await + qty(&pool, "PRODUCT", ids.product, "SCRAP_YARD").await;
    assert_eq!(total, d("10"));
    assert_eq!(qty(&pool, "PRODUCT", ids.product, "QC_QUARANTINE").await, d("0"));
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn qc_hard_lock_cannot_be_bypassed_through_warehouse(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let (ids, _) = new_batch(&c, 10).await;
    let (p, b) = (ids.product, ids.batch);
    // adjustment: into FG_READY from a quarantined batch lot, inside QC_QUARANTINE, or without a lot
    for body in [json!({ "item_type": "PRODUCT", "item_id": p, "zone": "FG_READY", "lot_id": b, "delta": 5, "reason": "found some crates", "confirm": true }),
                 json!({ "item_type": "PRODUCT", "item_id": p, "zone": "QC_QUARANTINE", "lot_id": b, "delta": -5, "reason": "remove stock", "confirm": true }),
                 json!({ "item_type": "PRODUCT", "item_id": p, "zone": "QC_QUARANTINE", "lot_id": b, "delta": 5, "reason": "add stock", "confirm": true }),
                 json!({ "item_type": "PRODUCT", "item_id": p, "zone": "FG_READY", "delta": 5, "reason": "no lot given here", "confirm": true })] {
        let (s, e) = c.post("/api/warehouse/adjustments", body.clone()).await;
        assert!(s == 400 || s == 409 || s == 422, "{body}: {s} {e}");
        assert_ne!(s, 201);
    }
    // transfers: out of quarantine or into FG_READY
    for body in [json!({ "item_type": "PRODUCT", "item_id": p, "lot_id": b, "from_zone": "QC_QUARANTINE", "to_zone": "FG_READY", "qty": 5 }),
                 json!({ "item_type": "PRODUCT", "item_id": p, "lot_id": b, "from_zone": "QC_QUARANTINE", "to_zone": "SCRAP_YARD", "qty": 5, "note": "scrapping it" }),
                 json!({ "item_type": "PRODUCT", "item_id": p, "lot_id": b, "from_zone": "SCRAP_YARD", "to_zone": "FG_READY", "qty": 1 })] {
        let (s, _) = c.post("/api/warehouse/transfers", body).await;
        assert_eq!(s, 400);
    }
    // write-off and count in quarantine
    let (s, _) = c.post("/api/warehouse/write-offs", json!({ "item_type": "PRODUCT", "item_id": p, "zone": "QC_QUARANTINE", "lot_id": b, "qty": 5, "reason_code": "BREAKAGE", "note": "all broken", "confirm": true })).await;
    assert_eq!(s, 400);
    let (s, _) = c.post("/api/warehouse/counts", json!({ "zone": "QC_QUARANTINE" })).await;
    assert_eq!(s, 400);
    assert_eq!(lot_qty(&pool, "PRODUCT", p, "QC_QUARANTINE", b).await, d("10"));
    assert_eq!(qty(&pool, "PRODUCT", p, "FG_READY").await, d("0"));
    check_invariants(&pool).await;
    // QC page required for QC routes
    let nobody = c.user("nobody", &["warehouse"]).await;
    let (s, _) = call(&c.app, "POST", &format!("/api/qc/batches/{b}/approve"), Some(&nobody), Some(json!({}))).await;
    assert_eq!(s, 403);
}
