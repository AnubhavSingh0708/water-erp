mod common;
mod materials_fixtures;
use materials_fixtures::*;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::ItemType;

#[sqlx::test(migrations = "./migrations")]
async fn bom_math_per_crate_and_per_1000l_and_shortage_listing(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    stock_plant(&pool, &p).await;
    fresh_water_log(&pool).await;

    // 10 crates x 24 x 500 ml = 120 L => mineral 2.5 per 1000 L = 0.3
    let (s, chk) = c.get(&format!("/api/production/bom-check?product_id={}&crates=10", p.p500)).await;
    assert_eq!(s, 200, "{chk}");
    assert_eq!(chk["ok"], true);
    assert_eq!(dv(&chk["litres"]), d("120"));
    let lines = chk["lines"].as_array().unwrap();
    let by = |id: Uuid| lines.iter().find(|l| l["item_id"] == json!(id)).unwrap().clone();
    assert_eq!(dv(&by(p.cap)["required"]), d("240"));
    assert_eq!(dv(&by(p.bottle)["required"]), d("240"));
    assert_eq!(dv(&by(p.mineral)["required"]), d("0.3"));
    assert_eq!(by(p.mineral)["basis"], "PER_1000L");
    assert_eq!(chk["pallets"], 0);
    assert_eq!(chk["loose_crates"], 10);

    let (s, b) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 10, "shift": "DAY" })).await;
    assert_eq!(s, 201, "{b}");
    let bid = uid(&b["id"]);
    let plant_code: String = sqlx::query_scalar("SELECT value #>> '{}' FROM system_settings WHERE key='plant.code'").fetch_one(&pool).await.unwrap();
    assert!(b["batch_no"].as_str().unwrap().starts_with(&format!("{plant_code}-B-")), "{}", b["batch_no"]);
    assert_eq!(b["qc_status"], "QUARANTINE");
    assert_eq!(dv(&b["litres"]), d("120"));
    assert_eq!(qty(&pool, "RAW", p.cap, "RM_STORE").await, d("99760"));
    assert_eq!(qty(&pool, "RAW", p.bottle, "BLOW_FLOOR").await, d("99760"));
    assert_eq!(qty(&pool, "RAW", p.mineral, "RM_STORE").await, d("999.7"));
    assert_eq!(lot_qty(&pool, "PRODUCT", p.p500, "QC_QUARANTINE", bid).await, d("10"));
    assert_eq!(qty(&pool, "PRODUCT", p.p500, "FG_READY").await, d("0"));
    let life: i32 = sqlx::query_scalar("SELECT expiry_date - mfg_date FROM production_batches WHERE id=$1").bind(bid).fetch_one(&pool).await.unwrap();
    assert_eq!(life, 180);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM batch_material_usage WHERE batch_id=$1").bind(bid).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 3);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='QC_PENDING' AND resolved_at IS NULL").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    assert!(b["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("pallet")), "not a whole pallet => info warning");
    assert_eq!(b["pallets"], 0);
    let (_, det) = c.get(&format!("/api/production/batches/{}", b["id"].as_str().unwrap())).await;
    assert_eq!(det["materials"].as_array().unwrap().len(), 3);
    assert_eq!(det["stock"]["QC_QUARANTINE"], "10.0000");
    check_invariants(&pool).await;

    // shortage: all short lines are listed together, nothing is consumed
    let before_cap = qty(&pool, "RAW", p.cap, "RM_STORE").await;
    let (s, e) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 5000, "shift": "DAY", "confirm": true })).await;
    assert_eq!((s, code(&e)), (409, "INSUFFICIENT_STOCK"), "{e}");
    let sh = e["error"]["details"]["shortages"].as_array().unwrap();
    assert_eq!(sh.len(), 2, "{e}"); // cap and empty bottles (120000 needed each)
    assert_eq!(qty(&pool, "RAW", p.cap, "RM_STORE").await, before_cap);
    let (_, chk) = c.get(&format!("/api/production/bom-check?product_id={}&crates=5000", p.p500)).await;
    assert_eq!(chk["ok"], false);
}

#[sqlx::test(migrations = "./migrations")]
async fn returnable_batch_consumes_clean_containers_never_preforms(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    stock_plant(&pool, &p).await;
    fresh_water_log(&pool).await;
    put_stock(&pool, ItemType::Raw, p.preform, "RM_STORE", Uuid::new_v4(), "5000", None).await;
    let jar = product(&pool, "JAR20", "RETURNABLE", Some(20000), 1, Some(30)).await;
    bom(&pool, jar, p.cap, "1", "RM_STORE", "PER_CRATE").await;
    bom(&pool, jar, p.mineral, "2", "RM_STORE", "PER_1000L").await;
    // bad data: a RETURNABLE BOM that lists a preform and an empty bottle must still never consume them
    bom(&pool, jar, p.preform, "1", "RM_STORE", "PER_CRATE").await;
    bom(&pool, jar, p.bottle, "1", "BLOW_FLOOR", "PER_CRATE").await;
    put_stock(&pool, ItemType::Product, jar, "JAR_CLEAN", Uuid::nil(), "100", None).await;

    let bottles = qty(&pool, "RAW", p.bottle, "BLOW_FLOOR").await;
    let (s, b) = c.post("/api/production/batches", json!({ "product_id": jar, "crates": 30, "shift": "DAY" })).await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(qty(&pool, "PRODUCT", jar, "JAR_CLEAN").await, d("70"));
    assert_eq!(qty(&pool, "RAW", p.preform, "RM_STORE").await, d("5000"));
    assert_eq!(qty(&pool, "RAW", p.bottle, "BLOW_FLOOR").await, bottles);
    assert_eq!(qty(&pool, "RAW", p.cap, "RM_STORE").await, d("99970"));
    assert_eq!(dv(&b["litres"]), d("600")); // 30 x 20 L
    assert_eq!(qty(&pool, "RAW", p.mineral, "RM_STORE").await, d("998.8")); // 2 per 1000 L x 600 L = 1.2
    assert_eq!(lot_qty(&pool, "PRODUCT", jar, "QC_QUARANTINE", uid(&b["id"])).await, d("30"));
    assert_eq!(b["pallets"], 1);
    // not enough clean containers => the container line is in shortages
    let (s, e) = c.post("/api/production/batches", json!({ "product_id": jar, "crates": 80, "shift": "DAY", "confirm": true })).await;
    assert_eq!((s, code(&e)), (409, "INSUFFICIENT_STOCK"), "{e}");
    assert!(e["error"]["details"]["shortages"].as_array().unwrap().iter().any(|x| x["item_type"] == "PRODUCT"));
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn quality_gate_water_log_pallet_and_anomaly(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    stock_plant(&pool, &p).await;
    // no water log at all => soft warning only
    let (s, b) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 60, "shift": "DAY" })).await;
    assert_eq!(s, 201, "{b}");
    let w = b["warnings"].as_array().unwrap();
    assert!(w.iter().any(|x| x.as_str().unwrap().contains("No water log in the last hour")));
    assert!(!w.iter().any(|x| x.as_str().unwrap().contains("pallet")), "60 crates = exactly one pallet");
    // out-of-range reading blocks until confirmed
    let (s, wl) = c.post("/api/production/water-logs", json!({ "tds_ppm": 900, "ph": 7.2, "uv_ok": true, "minerals": { "Calcium chloride": 1.2 } })).await;
    assert_eq!(s, 201, "{wl}");
    assert_eq!(wl["out_of_range"], true);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE event_type='WATER_OUT_OF_RANGE'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    let body = json!({ "product_id": p.p500, "crates": 60, "shift": "DAY" });
    let (s, e) = c.post("/api/production/batches", body.clone()).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"), "{e}");
    assert!(e["error"]["details"]["warnings"].as_array().unwrap().iter().any(|x| x.as_str().unwrap().contains("OUT OF RANGE")));
    let (s, dry) = c.post("/api/production/batches?dry_run=1", body.clone()).await;
    assert_eq!(s, 200, "{dry}");
    assert!(dry["warnings"].as_array().unwrap().iter().any(|x| x.as_str().unwrap().contains("OUT OF RANGE")));
    let mut ok = body.clone();
    ok["confirm"] = json!(true);
    let (s, b) = c.post("/api/production/batches", ok).await;
    assert_eq!(s, 201, "{b}");
    // a good reading clears the gate; a 3x bigger batch than the recent maximum needs confirmation
    c.post("/api/production/water-logs", json!({ "tds_ppm": 150, "ph": 7.2, "uv_ok": true })).await;
    let (s, e) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 150, "shift": "DAY" })).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"), "{e}");
    let (s, b) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 120, "shift": "NIGHT" })).await;
    assert_eq!(s, 201, "{b}");
    assert!(!b["warnings"].as_array().unwrap().iter().any(|x| x.as_str().unwrap().contains("water")));
    let (_, st) = c.get("/api/production/water-logs/status").await;
    assert_eq!(st["overdue"], false);
}

#[sqlx::test(migrations = "./migrations")]
async fn waste_is_deducted_flagged_and_rolls_back_on_shortage(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    stock_plant(&pool, &p).await;
    fresh_water_log(&pool).await;
    let cap0 = qty(&pool, "RAW", p.cap, "RM_STORE").await;
    // 10 crates: BOM cap 240; waste 10 (4 %) => soft-free, deducted on top
    let (s, b) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 10, "shift": "DAY",
        "waste": [{ "raw_material_id": p.cap, "qty": 10, "reason": "jammed capper" }] })).await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(qty(&pool, "RAW", p.cap, "RM_STORE").await, cap0 - d("250"));
    assert_eq!(b["waste"][0]["reason"].as_str().unwrap().contains("jammed capper"), true);
    assert_eq!(dv(&b["waste"][0]["qty"]), d("10"));
    let w: rust_decimal::Decimal = sqlx::query_scalar("SELECT -SUM(qty_delta) FROM stock_ledger WHERE txn_type='BATCH_WASTE' AND ref_id=$1").bind(uid(&b["id"])).fetch_one(&pool).await.unwrap();
    assert_eq!(w, d("10"));
    let (_, tr) = c.get(&format!("/api/production/batches/{}/trace", b["id"].as_str().unwrap())).await;
    assert!(tr["materials"].as_array().unwrap().iter().any(|m| m["kind"] == "WASTE"));
    // 6 % => soft warning, 30 % => confirm
    let (s, b) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 10, "shift": "DAY",
        "waste": [{ "raw_material_id": p.cap, "qty": 15, "reason": "label roll jam" }] })).await;
    assert_eq!(s, 201, "{b}");
    assert!(b["warnings"].as_array().unwrap().iter().any(|x| x.as_str().unwrap().contains("more than 5%")));
    let big = json!({ "product_id": p.p500, "crates": 10, "shift": "DAY", "waste": [{ "raw_material_id": p.cap, "qty": 80, "reason": "film roll ruined" }] });
    let (s, e) = c.post("/api/production/batches", big.clone()).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"), "{e}");
    // validation: empty bottles / non-BOM materials / short reason / duplicates
    for w in [json!([{ "raw_material_id": p.bottle, "qty": 5, "reason": "dropped" }]),
              json!([{ "raw_material_id": p.preform, "qty": 5, "reason": "dropped" }]),
              json!([{ "raw_material_id": p.cap, "qty": 5, "reason": "x" }]),
              json!([{ "raw_material_id": p.cap, "qty": 0, "reason": "dropped" }]),
              json!([{ "raw_material_id": p.cap, "qty": 1, "reason": "dropped" }, { "raw_material_id": p.cap, "qty": 1, "reason": "dropped" }])] {
        let (s, e) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 10, "shift": "DAY", "waste": w })).await;
        assert_eq!(s, 422, "{e}");
    }
    // not enough stock for the waste part => whole batch refused, nothing consumed or created
    let left = qty(&pool, "RAW", p.cap, "RM_STORE").await;
    let mut tx = pool.begin().await.unwrap();
    water_erp::stock::post_all(&mut tx, vec![water_erp::stock::Move::new(ItemType::Raw, p.cap, "RM_STORE", Uuid::nil(), d("1"), "GRN")]).await.unwrap();
    tx.commit().await.unwrap();
    let have = qty(&pool, "RAW", p.cap, "RM_STORE").await;
    assert_eq!(have, left + d("1"));
    let batches: i64 = sqlx::query_scalar("SELECT count(*) FROM production_batches").fetch_one(&pool).await.unwrap();
    // drain cap down to exactly the BOM need of 240 so only the waste is short
    let mut tx = pool.begin().await.unwrap();
    water_erp::stock::consume_fefo(&mut tx, ItemType::Raw, p.cap, "RM_STORE", have - d("240"), "ADJUST", None, None, None, "drain").await.unwrap();
    tx.commit().await.unwrap();
    let (s, e) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 10, "shift": "DAY", "waste": [{ "raw_material_id": p.cap, "qty": 10, "reason": "jammed" }] })).await;
    assert_eq!((s, code(&e)), (409, "INSUFFICIENT_STOCK"), "{e}");
    assert_eq!(qty(&pool, "RAW", p.cap, "RM_STORE").await, d("240"));
    assert_eq!(qty(&pool, "RAW", p.bottle, "BLOW_FLOOR").await, d("100000") - d("480"));
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM production_batches").fetch_one(&pool).await.unwrap(), batches);
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn twenty_parallel_batches_never_go_negative(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    // stock for exactly 12 batches of 10 crates (caps: 240 each)
    put_stock(&pool, ItemType::Raw, p.bottle, "BLOW_FLOOR", Uuid::nil(), "100000", None).await;
    put_stock(&pool, ItemType::Raw, p.cap, "RM_STORE", Uuid::new_v4(), "2880", None).await;
    put_stock(&pool, ItemType::Raw, p.mineral, "RM_STORE", Uuid::new_v4(), "100", None).await;
    fresh_water_log(&pool).await;
    let mut hs = vec![];
    for _ in 0..20 {
        let app = c.app.clone();
        let key = c.key.clone();
        let pid = p.p500;
        hs.push(tokio::spawn(async move {
            call(&app, "POST", "/api/production/batches", Some(&key), Some(json!({ "product_id": pid, "crates": 10, "shift": "DAY", "confirm": true }))).await
        }));
    }
    let mut ok = 0;
    for h in hs {
        let (s, v) = h.await.unwrap();
        match s { 201 => ok += 1, 409 => assert!(["INSUFFICIENT_STOCK", "RETRY"].contains(&code(&v)), "{v}"), x => panic!("unexpected {x} {v}") }
    }
    assert!(ok <= 12 && ok >= 8, "{ok} batches created");
    assert_eq!(qty(&pool, "RAW", p.cap, "RM_STORE").await, d("2880") - d("240") * rust_decimal::Decimal::from(ok));
    assert_eq!(qty(&pool, "PRODUCT", p.p500, "QC_QUARANTINE").await, d("10") * rust_decimal::Decimal::from(ok));
    let batches: i64 = sqlx::query_scalar("SELECT count(*) FROM production_batches").fetch_one(&pool).await.unwrap();
    assert_eq!(batches, ok);
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn batch_validation_dry_run_and_mass_assignment(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    stock_plant(&pool, &p).await;
    fresh_water_log(&pool).await;
    for body in [json!({ "product_id": p.p500, "crates": 0, "shift": "DAY" }), json!({ "product_id": p.p500, "crates": 1000001, "shift": "DAY" }),
                 json!({ "product_id": p.p500, "crates": 5, "shift": "EVENING" }), json!({ "product_id": Uuid::new_v4(), "crates": 5, "shift": "DAY" }),
                 json!({ "product_id": p.p500, "crates": 5, "shift": "DAY", "produced_on": future(3) })] {
        let (s, e) = c.post("/api/production/batches", body.clone()).await;
        assert_eq!(s, 422, "{body} {e}");
    }
    for extra in ["qc_status", "created_by", "status", "batch_no", "id"] {
        let (s, e) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 5, "shift": "DAY", extra: "APPROVED" })).await;
        assert_eq!((s, code(&e)), (400, "BAD_REQUEST"), "{extra}");
    }
    let (s, e) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 5.5, "shift": "DAY" })).await;
    assert_eq!(s, 400, "{e}");
    // inactive product refused
    let inactive = product(&pool, "OFF", "BOTTLED", Some(500), 12, None).await;
    bom(&pool, inactive, p.cap, "1", "RM_STORE", "PER_CRATE").await;
    sqlx::query("UPDATE products SET is_active=false WHERE id=$1").bind(inactive).execute(&pool).await.unwrap();
    let (s, _) = c.post("/api/production/batches", json!({ "product_id": inactive, "crates": 5, "shift": "DAY" })).await;
    assert_eq!(s, 422);
    // dry run: effects, nothing written, the sequence is not burnt
    let (s, dry) = c.post("/api/production/batches?dry_run=1", json!({ "product_id": p.p500, "crates": 5, "shift": "DAY" })).await;
    assert_eq!(s, 200, "{dry}");
    assert!(dry["effects"].as_array().unwrap().len() >= 4);
    assert!(dry["effects"].as_array().unwrap().iter().any(|x| x["zone"] == "QC_QUARANTINE" && dv(&x["delta"]) == d("5")));
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM production_batches").fetch_one(&pool).await.unwrap(), 0);
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM alerts WHERE event_type='QC_PENDING'").fetch_one(&pool).await.unwrap(), 0);
    let (s, b) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 5, "shift": "DAY" })).await;
    assert_eq!(s, 201);
    assert!(b["batch_no"].as_str().unwrap().ends_with("-0001"), "{}", b["batch_no"]);
    // list / filters / csv
    let (s, l) = c.get(&format!("/api/production/batches?qc_status=QUARANTINE&product_id={}", p.p500)).await;
    assert_eq!((s, l["total"].clone()), (200, json!(1)));
    let (s, e) = c.get("/api/production/batches?qc_status=BOGUS").await;
    assert_eq!((s, code(&e)), (422, "VALIDATION"));
    let (s, h, csv) = send(&c.app, "GET", "/api/production/batches?format=csv", Some(&c.key), None, &[]).await;
    assert_eq!(s, 200);
    assert!(h.get("content-type").unwrap().to_str().unwrap().contains("text/csv"));
    assert!(csv.starts_with("Batch,Product"));
}

#[sqlx::test(migrations = "./migrations")]
async fn blow_runs_stock_scrap_rules_and_dry_run(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    put_stock(&pool, ItemType::Raw, p.preform, "RM_STORE", Uuid::new_v4(), "10000", None).await;
    let eq: Uuid = sqlx::query_scalar("INSERT INTO equipment (code,name,kind) VALUES ('BM1','Blow 1','BLOW_MOLDER') RETURNING id").fetch_one(&pool).await.unwrap();
    let ok = json!({ "shift": "DAY", "equipment_id": eq, "preform_material_id": p.preform, "bottle_material_id": p.bottle, "preforms_issued": 1000, "good_bottles": 990 });
    let (s, r) = c.post("/api/production/blow-runs", ok.clone()).await;
    assert_eq!(s, 201, "{r}");
    assert_eq!(dv(&r["scrap_pcs"]), d("10"));
    assert_eq!(dv(&r["scrap_pct"]), d("1"));
    assert_eq!(dv(&r["scrap_weight_kg"]), d("0.2")); // 10 x 20 g
    assert_eq!(qty(&pool, "RAW", p.preform, "RM_STORE").await, d("9000"));
    assert_eq!(qty(&pool, "RAW", p.bottle, "BLOW_FLOOR").await, d("990"));
    assert_eq!(qty(&pool, "RAW", p.preform, "SCRAP_YARD").await, d("10"));
    // rules
    let mut bad = ok.clone();
    bad["good_bottles"] = json!(1001);
    assert_eq!(c.post("/api/production/blow-runs", bad).await.0, 422);
    let mut bad = ok.clone();
    bad["preform_material_id"] = json!(p.cap);
    assert_eq!(c.post("/api/production/blow-runs", bad).await.0, 422);
    let mut bad = ok.clone();
    bad["bottle_material_id"] = json!(p.preform);
    assert_eq!(c.post("/api/production/blow-runs", bad).await.0, 422);
    let mut bad = ok.clone();
    bad["preforms_issued"] = json!(100000);
    bad["good_bottles"] = json!(100000);
    let (s, e) = c.post("/api/production/blow-runs", bad).await;
    assert_eq!((s, code(&e)), (409, "INSUFFICIENT_STOCK"), "{e}");
    // scrap above the warn limit (5 %) needs a note
    let mut hi = ok.clone();
    hi["good_bottles"] = json!(900);
    let (s, e) = c.post("/api/production/blow-runs", hi.clone()).await;
    assert_eq!((s, code(&e)), (422, "VALIDATION"), "{e}");
    hi["notes"] = json!("machine overheated");
    let (s, r) = c.post("/api/production/blow-runs", hi).await;
    assert_eq!(s, 201, "{r}");
    assert!(!r["warnings"].as_array().unwrap().is_empty());
    // dry run
    let (s, dry) = c.post("/api/production/blow-runs?dry_run=1", ok.clone()).await;
    assert_eq!(s, 200, "{dry}");
    assert_eq!(dry["effects"].as_array().unwrap().len(), 3);
    assert_eq!(qty(&pool, "RAW", p.preform, "RM_STORE").await, d("8000"));
    let (s, l) = c.get("/api/production/blow-runs").await;
    assert_eq!((s, l["total"].clone()), (200, json!(2)));
    let (s, h, csv) = send(&c.app, "GET", "/api/production/blow-runs?format=csv", Some(&c.key), None, &[]).await;
    assert_eq!(s, 200);
    assert!(h.get("content-disposition").is_some() && csv.contains("Preforms issued"));
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn water_log_ranges_and_access(pool: PgPool) {
    let c = setup(pool.clone()).await;
    for bad in [json!({ "tds_ppm": 100, "ph": 15, "uv_ok": true }), json!({ "tds_ppm": -1, "ph": 7, "uv_ok": true }), json!({ "tds_ppm": 100, "ph": 7 }),
                json!({ "tds_ppm": 100, "ph": 7, "uv_ok": true, "minerals": { "a": -1 } }), json!({ "tds_ppm": 100, "ph": 7, "uv_ok": true, "extra": 1 })] {
        let (s, _) = c.post("/api/production/water-logs", bad).await;
        assert!(s == 422 || s == 400, "{s}");
    }
    let (s, v) = c.post("/api/production/water-logs", json!({ "tds_ppm": 20, "ph": 9.5, "ozone_ppm": 0.3, "uv_ok": false })).await;
    assert_eq!(s, 201);
    assert_eq!(v["out_of_range"], true);
    assert!(v["warnings"].as_array().unwrap().len() >= 2);
    let (s, st) = c.get("/api/production/water-logs/status").await;
    assert_eq!(s, 200);
    assert_eq!(st["overdue"], false);
    sqlx::query("UPDATE water_treatment_logs SET logged_at = now() - interval '2 hours'").execute(&pool).await.unwrap();
    let (_, st) = c.get("/api/production/water-logs/status").await;
    assert_eq!(st["overdue"], true);
    let (s, l) = c.get("/api/production/water-logs?from=2020-01-01&to=2099-01-01").await;
    assert_eq!((s, l["total"].clone()), (200, json!(1)));
    let op = c.user("op", &["dashboard"]).await;
    let (s, _) = call(&c.app, "POST", "/api/production/water-logs", Some(&op), Some(json!({ "tds_ppm": 100, "ph": 7, "uv_ok": true }))).await;
    assert_eq!(s, 403);
}

#[sqlx::test(migrations = "./migrations")]
async fn trace_summary_and_dashboard(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    stock_plant(&pool, &p).await;
    fresh_water_log(&pool).await;
    // supplier lot shows in the trace
    let (_, g) = c.post("/api/raw-materials/grns", json!({ "supplier_id": p.sup, "challan_no": "T1", "lines": [{ "raw_material_id": p.cap, "qty_purchase": 1, "lot_no": "CAPLOT-7" }] })).await;
    let _ = g;
    let (_, b) = c.post("/api/production/batches", json!({ "product_id": p.p500, "crates": 10, "shift": "DAY" })).await;
    let (s, tr) = c.get(&format!("/api/production/batches/{}/trace", b["id"].as_str().unwrap())).await;
    assert_eq!(s, 200, "{tr}");
    assert!(tr["materials"].as_array().unwrap().len() >= 3);
    assert_eq!(dv(&tr["totals"]["still_in_stock"]), d("10"));
    let (s, sm) = c.get("/api/production/summary").await;
    assert_eq!(s, 200, "{sm}");
    assert_eq!(sm["days"][0]["batches"], 1);
    assert_eq!(sm["days"][0]["bottles"], 240);
    assert_eq!(sm["by_product"][0]["crates"], 10);
    let (s, e) = c.get("/api/production/summary?from=2020-01-01&to=2023-01-01").await;
    assert_eq!((s, code(&e)), (422, "VALIDATION"));
    let (s, dash) = c.get("/api/dashboard").await;
    assert_eq!(s, 200, "{dash}");
    assert_eq!(dash["kpis"]["batches_in_quarantine"], 1);
    assert_eq!(dash["pending_qc"].as_array().unwrap().len(), 1);
    assert_eq!(dash["kpis"]["water_log_overdue"], false);
    let (s, _) = c.get(&format!("/api/production/batches/{}", Uuid::new_v4())).await;
    assert_eq!(s, 404);
    let (s, _) = c.get("/api/production/batches/not-a-uuid").await;
    assert_eq!(s, 404);
}
