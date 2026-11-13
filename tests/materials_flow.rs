mod common;
mod materials_fixtures;
use materials_fixtures::*;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType, Move};

#[sqlx::test(migrations = "./migrations")]
async fn grn_conversion_rollup_duplicate_over_receipt_and_void(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let sup = supplier(&pool, "S1", 5).await;
    let cap = material(&pool, "CAP-X", "CAP", "1000", "0", Some(sup)).await;

    let (s, po) = c.post("/api/raw-materials/purchase-orders", json!({ "supplier_id": sup, "lines": [{ "raw_material_id": cap, "qty_ordered": "10" }] })).await;
    assert_eq!(s, 201, "{po}");
    let po_id = po["id"].as_str().unwrap().to_string();
    assert_eq!(po["status"], "OPEN");

    // 4 bags = 4000 pcs
    let (s, g1) = c.post("/api/raw-materials/grns", json!({ "po_id": po_id, "supplier_id": sup, "challan_no": "CH-1",
        "lines": [{ "raw_material_id": cap, "qty_purchase": 4, "lot_no": "L1", "expiry_date": future(200) }] })).await;
    assert_eq!(s, 201, "{g1}");
    let lot1 = uid(&g1["lines"][0]["lot_id"]);
    assert_eq!(dv(&g1["lines"][0]["qty_stock"]), d("4000"));
    assert_eq!(lot_qty(&pool, "RAW", cap, "RM_STORE", lot1).await, d("4000"));
    let (_, po) = c.get(&format!("/api/raw-materials/purchase-orders/{po_id}")).await;
    assert_eq!(po["status"], "PARTIAL");
    assert_eq!(dv(&po["lines"][0]["qty_received"]), d("4"));

    // duplicate challan (case-insensitive) is never confirmable
    let dup = json!({ "po_id": po_id, "supplier_id": sup, "challan_no": "ch-1", "confirm": true, "lines": [{ "raw_material_id": cap, "qty_purchase": 1 }] });
    let (s, e) = c.post("/api/raw-materials/grns", dup).await;
    assert_eq!((s, code(&e)), (409, "DUPLICATE"), "{e}");

    // 7 more bags => exactly 110 % -> allowed, PO RECEIVED
    let (s, g2) = c.post("/api/raw-materials/grns", json!({ "po_id": po_id, "supplier_id": sup, "challan_no": "CH-2", "lines": [{ "raw_material_id": cap, "qty_purchase": "7" }] })).await;
    assert_eq!(s, 201, "{g2}");
    let (_, po) = c.get(&format!("/api/raw-materials/purchase-orders/{po_id}")).await;
    assert_eq!(po["status"], "RECEIVED");
    // anything beyond 110 % is a hard error
    let (s, e) = c.post("/api/raw-materials/grns", json!({ "po_id": po_id, "supplier_id": sup, "challan_no": "CH-3", "confirm": true, "lines": [{ "raw_material_id": cap, "qty_purchase": 1 }] })).await;
    assert_eq!((s, code(&e)), (409, "BAD_REQUEST"), "{e}");

    // the first GRN's lot is partly consumed -> void refused
    let mut tx = pool.begin().await.unwrap();
    stock::consume_fefo(&mut tx, ItemType::Raw, cap, "RM_STORE", d("500"), "BATCH_CONSUME", None, None, None, "").await.unwrap();
    tx.commit().await.unwrap();
    let g1_id = g1["id"].as_str().unwrap();
    let (s, e) = c.post(&format!("/api/raw-materials/grns/{g1_id}/void"), json!({ "reason": "wrong delivery" })).await;
    // FEFO consumed the earliest expiry first: lot1 expires in 200 days, g2 lot has no expiry => lot1 consumed
    assert_eq!((s, code(&e)), (409, "INSUFFICIENT_STOCK"), "{e}");

    // void the untouched second GRN: stock reversed, PO rolled back to PARTIAL, challan reusable
    let g2_id = g2["id"].as_str().unwrap();
    let (s, v) = c.post(&format!("/api/raw-materials/grns/{g2_id}/void"), json!({ "reason": "entered twice by mistake" })).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "VOID");
    let (_, po) = c.get(&format!("/api/raw-materials/purchase-orders/{po_id}")).await;
    assert_eq!(po["status"], "PARTIAL");
    assert_eq!(dv(&po["lines"][0]["qty_received"]), d("4"));
    assert_eq!(qty(&pool, "RAW", cap, "RM_STORE").await, d("3500"));
    let (s, e) = c.post(&format!("/api/raw-materials/grns/{g2_id}/void"), json!({ "reason": "again again" })).await;
    assert_eq!((s, code(&e)), (409, "INVALID_STATE"), "{e}");
    let (s, _) = c.post("/api/raw-materials/grns", json!({ "supplier_id": sup, "challan_no": "CH-2", "lines": [{ "raw_material_id": cap, "qty_purchase": 1 }] })).await;
    assert_eq!(s, 201);
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn grn_validation_confirm_dry_run_and_idempotency(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let sup = supplier(&pool, "S1", 5).await;
    let other = supplier(&pool, "S2", 5).await;
    let m = material(&pool, "M1", "CAP", "10", "0", Some(sup)).await;
    // mass assignment / unknown fields
    let (s, e) = c.post("/api/raw-materials/grns", json!({ "supplier_id": sup, "status": "VOID", "lines": [{ "raw_material_id": m, "qty_purchase": 1 }] })).await;
    assert_eq!((s, code(&e)), (400, "BAD_REQUEST"), "{e}");
    // zero / negative / too many decimals
    for q in [json!(0), json!(-3), json!("1.00001"), json!("1e12")] {
        let (s, e) = c.post("/api/raw-materials/grns", json!({ "supplier_id": sup, "lines": [{ "raw_material_id": m, "qty_purchase": q }] })).await;
        assert_eq!(s, 422, "{q} {e}");
    }
    // unknown material / supplier ids are validation errors, not 500s
    let (s, _) = c.post("/api/raw-materials/grns", json!({ "supplier_id": sup, "lines": [{ "raw_material_id": Uuid::new_v4(), "qty_purchase": 1 }] })).await;
    assert_eq!(s, 422);
    let (s, _) = c.post("/api/raw-materials/grns", json!({ "supplier_id": Uuid::new_v4(), "lines": [{ "raw_material_id": m, "qty_purchase": 1 }] })).await;
    assert_eq!(s, 422);
    // PO of another supplier
    let (_, po) = c.post("/api/raw-materials/purchase-orders", json!({ "supplier_id": other, "lines": [{ "raw_material_id": m, "qty_ordered": 5 }] })).await;
    let (s, _) = c.post("/api/raw-materials/grns", json!({ "po_id": po["id"], "supplier_id": sup, "lines": [{ "raw_material_id": m, "qty_purchase": 1 }] })).await;
    assert_eq!(s, 422);
    // po_line_id belonging to another PO is rejected
    let (_, po2) = c.post("/api/raw-materials/purchase-orders", json!({ "supplier_id": sup, "lines": [{ "raw_material_id": m, "qty_ordered": 5 }] })).await;
    let (s, _) = c.post("/api/raw-materials/grns", json!({ "po_id": po2["id"], "supplier_id": sup, "lines": [{ "raw_material_id": m, "po_line_id": po["lines"][0]["id"], "qty_purchase": 1 }] })).await;
    assert_eq!(s, 422);

    // history of 3 receipts of 10 => 150 is > 10x median => CONFIRM_REQUIRED
    for i in 0..3 {
        let (s, _) = c.post("/api/raw-materials/grns", json!({ "supplier_id": sup, "challan_no": format!("H{i}"), "lines": [{ "raw_material_id": m, "qty_purchase": 10 }] })).await;
        assert_eq!(s, 201);
    }
    let big = json!({ "supplier_id": sup, "challan_no": "BIG", "lines": [{ "raw_material_id": m, "qty_purchase": 150 }] });
    let (s, e) = c.post("/api/raw-materials/grns", big.clone()).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"), "{e}");
    assert!(e["error"]["details"]["warnings"].as_array().unwrap().len() >= 1);
    // dry run: no confirm needed, shows effects + warnings, nothing persisted, no sequence burnt
    let before = qty(&pool, "RAW", m, "RM_STORE").await;
    let (s, dry) = c.post("/api/raw-materials/grns?dry_run=1", big.clone()).await;
    assert_eq!(s, 200, "{dry}");
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dv(&dry["effects"][0]["delta"]), d("1500"));
    assert!(!dry["warnings"].as_array().unwrap().is_empty());
    assert_eq!(qty(&pool, "RAW", m, "RM_STORE").await, before);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM grns WHERE challan_no='BIG'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
    // confirmed + idempotent retry: same key replays, only one GRN
    let mut b2 = big.clone();
    b2["confirm"] = json!(true);
    let (s1, _, t1) = send(&c.app, "POST", "/api/raw-materials/grns", Some(&c.key), Some(b2.clone()), &[("idempotency-key", "idem-key-0001")]).await;
    let (s2, h2, t2) = send(&c.app, "POST", "/api/raw-materials/grns", Some(&c.key), Some(b2), &[("idempotency-key", "idem-key-0001")]).await;
    assert_eq!((s1, s2), (201, 201));
    assert_eq!(t1, t2);
    assert!(h2.get("idempotent-replay").is_some());
    assert_eq!(qty(&pool, "RAW", m, "RM_STORE").await, before + d("1500"));
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn supplier_return_flow(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let sup = supplier(&pool, "S1", 5).await;
    let other = supplier(&pool, "S2", 5).await;
    let m = material(&pool, "M1", "CAP", "100", "0", Some(sup)).await;
    let (_, g) = c.post("/api/raw-materials/grns", json!({ "supplier_id": sup, "challan_no": "A1", "lines": [{ "raw_material_id": m, "qty_purchase": 2 }] })).await;
    let lot = g["lines"][0]["lot_id"].clone();
    // wrong supplier / wrong lot / more than received are rejected
    let (s, _) = c.post("/api/raw-materials/supplier-returns", json!({ "supplier_id": other, "lines": [{ "raw_material_id": m, "lot_id": lot, "qty_stock": 10, "reason": "damaged" }] })).await;
    assert_eq!(s, 422);
    let (s, _) = c.post("/api/raw-materials/supplier-returns", json!({ "supplier_id": sup, "lines": [{ "raw_material_id": m, "lot_id": Uuid::new_v4(), "qty_stock": 10, "reason": "damaged" }] })).await;
    assert_eq!(s, 422);
    let (s, e) = c.post("/api/raw-materials/supplier-returns", json!({ "supplier_id": sup, "lines": [{ "raw_material_id": m, "lot_id": lot, "qty_stock": 201, "reason": "damaged" }] })).await;
    assert_eq!((s, code(&e)), (409, "BAD_REQUEST"), "{e}");
    let (s, r) = c.post("/api/raw-materials/supplier-returns", json!({ "supplier_id": sup, "grn_id": g["id"], "lines": [{ "raw_material_id": m, "lot_id": lot, "qty_stock": 150, "reason": "crushed cartons" }] })).await;
    assert_eq!(s, 201, "{r}");
    assert!(r["rtv_no"].as_str().unwrap().starts_with("RTV-"));
    assert_eq!(qty(&pool, "RAW", m, "RM_STORE").await, d("50"));
    // only 50 left of the 200 received -> returning 60 more exceeds what was received (150 already returned)
    let (s, _) = c.post("/api/raw-materials/supplier-returns", json!({ "supplier_id": sup, "lines": [{ "raw_material_id": m, "lot_id": lot, "qty_stock": 60, "reason": "again" }] })).await;
    assert_eq!(s, 409);
    let (s, l) = c.get("/api/raw-materials/supplier-returns").await;
    assert_eq!(s, 200);
    assert_eq!(l["total"], 1);
    let (s, one) = c.get(&format!("/api/raw-materials/supplier-returns/{}", r["id"].as_str().unwrap())).await;
    assert_eq!(s, 200);
    assert_eq!(one["lines"].as_array().unwrap().len(), 1);
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn reorder_engine_min_burn_dedupe_resolve_and_create_po(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let sup = supplier(&pool, "S1", 5).await;
    // below static minimum
    let a = material(&pool, "A", "CAP", "1000", "1000", Some(sup)).await;
    put_stock(&pool, ItemType::Raw, a, "RM_STORE", Uuid::new_v4(), "500", None).await;
    // burn-rate only: min 0, consumption 1400 in the window => 100/day; lead 5 + safety 3 => point 800; stock 700
    let b = material(&pool, "B", "LABEL", "1000", "0", Some(sup)).await;
    put_stock(&pool, ItemType::Raw, b, "RM_STORE", Uuid::new_v4(), "2100", None).await;
    let mut tx = pool.begin().await.unwrap();
    stock::consume_fefo(&mut tx, ItemType::Raw, b, "RM_STORE", d("1000"), "BATCH_CONSUME", None, None, None, "").await.unwrap();
    stock::consume_fefo(&mut tx, ItemType::Raw, b, "RM_STORE", d("400"), "BATCH_WASTE", None, None, None, "waste").await.unwrap();
    tx.commit().await.unwrap();
    // an empty-bottle material is ignored
    let eb = material(&pool, "EB", "EMPTY_BOTTLE", "1", "100", None).await;
    let _ = eb;

    let r = water_erp::workers::reorder::run_once(&c.st).await.unwrap();
    assert_eq!(r["open_recommendations"], 2, "{r}");
    let (s, recs) = c.get("/api/raw-materials/reorder-recommendations?status=OPEN").await;
    assert_eq!(s, 200);
    let rec_a = recs.as_array().unwrap().iter().find(|x| x["raw_material_id"] == json!(a)).unwrap().clone();
    let rec_b = recs.as_array().unwrap().iter().find(|x| x["raw_material_id"] == json!(b)).unwrap().clone();
    assert_eq!(rec_a["reason"], "BELOW_MIN");
    assert_eq!(dv(&rec_a["suggested_qty_purchase"]), d("2")); // ceil((2*1000-500)/1000)
    assert_eq!(rec_b["reason"], "BURN_RATE");
    assert_eq!(dv(&rec_b["avg_daily_burn"]), d("100"));
    assert_eq!(dv(&rec_b["reorder_point"]), d("800"));
    assert_eq!(dv(&rec_b["suggested_qty_purchase"]), d("1")); // ceil((1600-700)/1000)

    // dedupe: run again => same rows, one open alert each
    c.post("/api/raw-materials/reorder-recommendations/run", json!({})).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM reorder_recommendations WHERE status='OPEN'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 2);
    let low: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='LOW_STOCK' AND resolved_at IS NULL").fetch_one(&pool).await.unwrap();
    let rr: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='REORDER_RECOMMENDED' AND resolved_at IS NULL").fetch_one(&pool).await.unwrap();
    assert_eq!((low, rr), (1, 1));

    // list view carries status + burn
    let (_, mats) = c.get("/api/raw-materials?low=1").await;
    assert!(mats.as_array().unwrap().iter().any(|m| m["id"] == json!(a) && m["status"] == "LOW" && m["open_reorder"] == true));

    // create PO from recommendation: ORDERED, a second click is INVALID_STATE, engine does not re-create (on order)
    let (s, po) = c.post(&format!("/api/raw-materials/reorder-recommendations/{}/create-po", rec_a["id"].as_str().unwrap()), json!({})).await;
    assert_eq!(s, 201, "{po}");
    assert_eq!(dv(&po["lines"][0]["qty_ordered"]), d("2"));
    let (s, e) = c.post(&format!("/api/raw-materials/reorder-recommendations/{}/create-po", rec_a["id"].as_str().unwrap()), json!({})).await;
    assert_eq!((s, code(&e)), (409, "INVALID_STATE"));
    water_erp::workers::reorder::run_once(&c.st).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM reorder_recommendations WHERE raw_material_id=$1 AND status='OPEN'").bind(a).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);

    // dismiss the other one
    let (s, _) = c.post(&format!("/api/raw-materials/reorder-recommendations/{}/dismiss", rec_b["id"].as_str().unwrap()), json!({})).await;
    assert_eq!(s, 200);
    water_erp::workers::reorder::run_once(&c.st).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM reorder_recommendations WHERE raw_material_id=$1 AND status='OPEN'").bind(b).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0, "dismissed recommendation must not come straight back");

    // goods receipt for the ordered PO: stock recovers, LOW_STOCK alert + recommendation resolved
    let (s, g) = c.post("/api/raw-materials/grns", json!({ "po_id": po["id"], "supplier_id": sup, "challan_no": "R1", "lines": [{ "raw_material_id": a, "qty_purchase": 2 }] })).await;
    assert_eq!(s, 201, "{g}");
    let low: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='LOW_STOCK' AND dedupe_key=$1 AND resolved_at IS NULL").bind(a.to_string()).fetch_one(&pool).await.unwrap();
    assert_eq!(low, 0);
    let st: String = sqlx::query_scalar("SELECT status FROM reorder_recommendations WHERE id=$1").bind(uid(&rec_a["id"])).fetch_one(&pool).await.unwrap();
    assert_eq!(st, "RESOLVED");
}

#[sqlx::test(migrations = "./migrations")]
async fn stock_expiry_alerts_and_listing(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let m = material(&pool, "E1", "CAP", "1", "0", None).await;
    let lot = Uuid::new_v4();
    put_stock(&pool, ItemType::Raw, m, "RM_STORE", lot, "50", Some(future(5))).await;
    put_stock(&pool, ItemType::Raw, m, "RM_STORE", Uuid::new_v4(), "50", Some(future(400))).await;
    water_erp::workers::reorder::run_once(&c.st).await.unwrap();
    water_erp::workers::reorder::run_once(&c.st).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='STOCK_EXPIRY' AND resolved_at IS NULL AND dedupe_key=$1").bind(format!("lot:{lot}:RM_STORE")).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    let (s, ex) = c.get("/api/warehouse/expiring?days=30").await;
    assert_eq!(s, 200);
    assert_eq!(ex.as_array().unwrap().len(), 1);
    assert_eq!(ex[0]["days_left"], 5);
    // consume the lot entirely => alert resolves
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Raw, m, "RM_STORE", lot, d("-50"), "ADJUST")]).await.unwrap();
    tx.commit().await.unwrap();
    water_erp::workers::reorder::run_once(&c.st).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='STOCK_EXPIRY' AND resolved_at IS NULL").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn page_access_filters_pagination_and_injection(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let qc_only = c.user("qcuser", &["qc"]).await;
    let store = c.user("storeuser", &["raw-materials"]).await;
    for uri in ["/api/raw-materials", "/api/production/batches", "/api/warehouse/stock", "/api/dashboard", "/api/production/water-logs/status"] {
        let (s, e) = call(&c.app, "GET", uri, Some(&qc_only), None).await;
        assert_eq!((s, code(&e)), (403, "FORBIDDEN"), "{uri}");
    }
    let (s, _) = call(&c.app, "GET", "/api/qc/batches", Some(&qc_only), None).await;
    assert_eq!(s, 200);
    let (s, _) = call(&c.app, "GET", "/api/ref/products", Some(&qc_only), None).await;
    assert_eq!(s, 200);
    let (s, _) = call(&c.app, "GET", "/api/ref/zones", None, None).await;
    assert_eq!(s, 401);
    let (s, _) = call(&c.app, "POST", "/api/warehouse/adjustments", Some(&store), Some(json!({}))).await;
    assert_eq!(s, 403);

    let sup = supplier(&pool, "S", 3).await;
    for i in 0..5 { material(&pool, &format!("PAG{i}"), "CAP", "1", "0", Some(sup)).await; }
    // SQL-injection strings in q are inert
    for q in ["'%20OR%201=1%20--", "%25", "_", "%27%3B%20DROP%20TABLE%20users%3B--"] {
        let (s, v) = c.get(&format!("/api/raw-materials?q={q}")).await;
        assert_eq!(s, 200, "{q}");
        assert_eq!(v.as_array().unwrap().len(), 0, "q={q}");
    }
    let (s, v) = c.get("/api/raw-materials?q=PAG").await;
    assert_eq!((s, v.as_array().unwrap().len()), (200, 5));
    // bad enum / bad date => envelope, not a plain-text rejection
    let (s, e) = c.get("/api/raw-materials?category=NOPE").await;
    assert_eq!((s, code(&e)), (422, "VALIDATION"));
    let (s, e) = c.get("/api/warehouse/ledger?from=notadate").await;
    assert_eq!((s, code(&e)), (400, "BAD_REQUEST"));
    // pagination caps
    let (s, v) = c.get("/api/warehouse/ledger?limit=100000&offset=-5").await;
    assert_eq!(s, 200);
    assert!(v["items"].as_array().unwrap().len() <= 200);
    let (s, v) = c.get("/api/raw-materials/grns?limit=0").await;
    assert_eq!((s, v["items"].as_array().unwrap().len()), (200, 0));
    // CSV export with injection defence
    sqlx::query("UPDATE raw_materials SET name='=HYPERLINK(\"http://evil\")' WHERE code='PAG0'").execute(&pool).await.unwrap();
    let (s, h, body) = send(&c.app, "GET", "/api/raw-materials?format=csv&q=PAG0", Some(&c.key), None, &[]).await;
    assert_eq!(s, 200);
    assert!(h.get("content-type").unwrap().to_str().unwrap().starts_with("text/csv"));
    assert!(h.get("content-disposition").unwrap().to_str().unwrap().contains("attachment"));
    assert!(body.contains("\"'=HYPERLINK(\"\"http://evil\"\")\""), "{body}");
    assert!(!body.contains(",=HYPERLINK"));
}
