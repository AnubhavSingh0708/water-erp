mod common;
mod materials_fixtures;
use materials_fixtures::*;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType};

/// An APPROVED batch with `crates` in FG_READY.
async fn approved_batch(pool: &PgPool, product: Uuid, crates: i32) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO production_batches (batch_no, product_id, crates, qc_status, expiry_date) VALUES ('AB'||substr(gen_random_uuid()::text,1,8), $1, $2, 'APPROVED', current_date + 150) RETURNING id")
        .bind(product).bind(crates).fetch_one(pool).await.unwrap();
    put_stock(pool, ItemType::Product, product, "FG_READY", id, &crates.to_string(), Some(future(150))).await;
    id
}

async fn loading_trip(pool: &PgPool) -> Uuid {
    let v: Uuid = sqlx::query_scalar("INSERT INTO vehicles (reg_no, capacity_crates) VALUES ('VH'||substr(gen_random_uuid()::text,1,6), 1000) RETURNING id").fetch_one(pool).await.unwrap();
    sqlx::query_scalar("INSERT INTO loading_trips (trip_no, vehicle_id, capacity_crates) VALUES ('T'||substr(gen_random_uuid()::text,1,8), $1, 1000) RETURNING id").bind(v).fetch_one(pool).await.unwrap()
}

async fn reserve(pool: &PgPool, trip: Uuid, product: Uuid, batch: Uuid, crates: i32) {
    sqlx::query("INSERT INTO trip_items (trip_id, batch_id, product_id, crates) VALUES ($1,$2,$3,$4)").bind(trip).bind(batch).bind(product).bind(crates).execute(pool).await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn adjustment_rules_confirm_dry_run_and_lot_ownership(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let sup = supplier(&pool, "S", 3).await;
    let m = material(&pool, "M1", "CAP", "1", "0", Some(sup)).await;
    let other = material(&pool, "M2", "CAP", "1", "0", Some(sup)).await;
    let lot = Uuid::new_v4();
    put_stock(&pool, ItemType::Raw, m, "RM_STORE", lot, "1000", None).await;
    let base = |delta: Value| json!({ "item_type": "RAW", "item_id": m, "zone": "RM_STORE", "lot_id": lot, "delta": delta, "reason": "cycle count fix" });
    // small change: fine, ledger row + audit
    let (s, a) = c.post("/api/warehouse/adjustments", base(json!(50))).await;
    assert_eq!(s, 201, "{a}");
    assert_eq!(dv(&a["balance_after"]), d("1050"));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='STOCK_ADJUST'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    // large (> 20 %) needs confirm; dry run reports it without stopping
    let (s, e) = c.post("/api/warehouse/adjustments", base(json!(-500))).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"), "{e}");
    let (s, dry) = c.post("/api/warehouse/adjustments?dry_run=1", base(json!(-500))).await;
    assert_eq!(s, 200, "{dry}");
    assert_eq!(dv(&dry["effects"][0]["after"]), d("550"));
    assert!(!dry["warnings"].as_array().unwrap().is_empty());
    assert_eq!(qty(&pool, "RAW", m, "RM_STORE").await, d("1050"));
    let mut conf = base(json!(-500));
    conf["confirm"] = json!(true);
    conf["reason_code"] = json!("DAMAGE");
    let (s, _) = c.post("/api/warehouse/adjustments", conf).await;
    assert_eq!(s, 201);
    // hard errors
    let mut inf = base(json!(-5000));
    inf["confirm"] = json!(true);
    let (s, e) = c.post("/api/warehouse/adjustments", inf).await;
    assert_eq!((s, code(&e)), (409, "INSUFFICIENT_STOCK"), "{e}");
    for (body, want) in [
        (json!({ "item_type": "RAW", "item_id": m, "zone": "RM_STORE", "lot_id": lot, "delta": 0, "reason": "zero change" }), 422),
        (json!({ "item_type": "RAW", "item_id": m, "zone": "RM_STORE", "lot_id": lot, "delta": 5, "reason": "x" }), 422),
        (json!({ "item_type": "RAW", "item_id": m, "zone": "RM_STORE", "lot_id": lot, "delta": "1.00001", "reason": "too precise" }), 422),
        (json!({ "item_type": "RAW", "item_id": m, "zone": "RM_STORE", "lot_id": lot, "delta": 5, "reason": "bad code", "reason_code": "NOPE" }), 422),
        (json!({ "item_type": "RAW", "item_id": m, "zone": "NOWHERE", "lot_id": lot, "delta": 5, "reason": "bad zone here" }), 422),
        (json!({ "item_type": "RAW", "item_id": m, "zone": "FG_READY", "delta": 5, "reason": "wrong zone for raw" }), 422),
        (json!({ "item_type": "RAW", "item_id": Uuid::new_v4(), "zone": "RM_STORE", "delta": 5, "reason": "ghost material" }), 422),
        (json!({ "item_type": "RAW", "item_id": other, "zone": "RM_STORE", "lot_id": lot, "delta": 5, "reason": "lot of another item" }), 422),
        (json!({ "item_type": "RAW", "item_id": m, "zone": "RM_STORE", "lot_id": Uuid::new_v4(), "delta": 5, "reason": "invented lot id" }), 422),
        (json!({ "item_type": "RAW", "item_id": m, "zone": "RM_STORE", "lot_id": lot, "delta": 5, "reason": "mass assignment", "user_id": Uuid::new_v4() }), 400),
        (json!({ "item_type": "NOPE", "item_id": m, "zone": "RM_STORE", "delta": 5, "reason": "bad item type" }), 400),
    ] {
        let (s, e) = c.post("/api/warehouse/adjustments", body.clone()).await;
        assert_eq!(s, want, "{body} -> {e}");
    }
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn adjustments_in_parallel_never_go_negative(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let m = material(&pool, "M1", "CAP", "1", "0", None).await;
    let lot = Uuid::new_v4();
    put_stock(&pool, ItemType::Raw, m, "RM_STORE", lot, "100", None).await;
    let mut hs = vec![];
    for _ in 0..20 {
        let (app, key) = (c.app.clone(), c.key.clone());
        hs.push(tokio::spawn(async move {
            call(&app, "POST", "/api/warehouse/adjustments", Some(&key),
                Some(json!({ "item_type": "RAW", "item_id": m, "zone": "RM_STORE", "lot_id": lot, "delta": -10, "reason": "parallel test", "confirm": true }))).await
        }));
    }
    let mut ok = 0;
    for h in hs {
        let (s, v) = h.await.unwrap();
        match s { 201 => ok += 1, 409 => assert!(["INSUFFICIENT_STOCK", "RETRY"].contains(&code(&v)), "{v}"), x => panic!("{x} {v}") }
    }
    assert_eq!(ok, 10);
    assert_eq!(qty(&pool, "RAW", m, "RM_STORE").await, d("0"));
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn transfers_rules(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let m = material(&pool, "M1", "CAP", "1", "0", None).await;
    let lot = Uuid::new_v4();
    put_stock(&pool, ItemType::Raw, m, "RM_STORE", lot, "100", Some(future(100))).await;
    let t = |from: &str, to: &str, q: i64, note: &str| json!({ "item_type": "RAW", "item_id": m, "lot_id": lot, "from_zone": from, "to_zone": to, "qty": q, "note": note });
    let (s, v) = c.post("/api/warehouse/transfers", { let mut b = t("RM_STORE", "BLOW_FLOOR", 10, ""); b["confirm"] = json!(true); b }).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(lot_qty(&pool, "RAW", m, "BLOW_FLOOR", lot).await, d("10"));
    let exp: Option<chrono::NaiveDate> = sqlx::query_scalar("SELECT expiry_date FROM stock_balances WHERE zone='BLOW_FLOOR' AND lot_id=$1").bind(lot).fetch_one(&pool).await.unwrap();
    assert!(exp.is_some(), "expiry travels with the lot");
    // scrap requires a reason
    let (s, _) = c.post("/api/warehouse/transfers", t("RM_STORE", "SCRAP_YARD", 1, "x")).await;
    assert_eq!(s, 422);
    let (s, _) = c.post("/api/warehouse/transfers", t("RM_STORE", "SCRAP_YARD", 1, "damaged in rain")).await;
    assert_eq!(s, 201);
    // not allowed: same zone, raw into FG_READY / jar zones, out of scrap, more than the balance
    for (from, to) in [("RM_STORE", "RM_STORE"), ("RM_STORE", "FG_READY"), ("RM_STORE", "JAR_CLEAN"), ("SCRAP_YARD", "RM_STORE")] {
        let (s, _) = c.post("/api/warehouse/transfers", t(from, to, 1, "some reason")).await;
        assert!(s == 400 || s == 422, "{from}->{to}: {s}");
    }
    let (s, e) = c.post("/api/warehouse/transfers", { let mut b = t("RM_STORE", "BLOW_FLOOR", 500, ""); b["confirm"] = json!(true); b }).await;
    assert_eq!((s, code(&e)), (409, "INSUFFICIENT_STOCK"), "{e}");
    let (s, _) = c.post("/api/warehouse/transfers", { let mut b = t("RM_STORE", "BLOW_FLOOR", 1, ""); b["lot_id"] = json!(Uuid::new_v4()); b }).await;
    assert_eq!(s, 422);
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn write_off_flow_confirm_dispose_dry_run_idempotency_and_fleet(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    let b = approved_batch(&pool, p.p500, 100).await;
    let body = json!({ "item_type": "PRODUCT", "item_id": p.p500, "zone": "FG_READY", "lot_id": b, "qty": 10, "reason_code": "BREAKAGE", "note": "forklift accident" });
    let (s, e) = c.post("/api/warehouse/write-offs", body.clone()).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"), "{e}");
    let (s, dry) = c.post("/api/warehouse/write-offs?dry_run=1", body.clone()).await;
    assert_eq!(s, 200, "{dry}");
    assert_eq!(dry["effects"].as_array().unwrap().len(), 2);
    assert_eq!(qty(&pool, "PRODUCT", p.p500, "FG_READY").await, d("100"));
    let mut conf = body.clone();
    conf["confirm"] = json!(true);
    let (s1, _, t1) = send(&c.app, "POST", "/api/warehouse/write-offs", Some(&c.key), Some(conf.clone()), &[("idempotency-key", "wo-idem-0001")]).await;
    let (s2, _, t2) = send(&c.app, "POST", "/api/warehouse/write-offs", Some(&c.key), Some(conf.clone()), &[("idempotency-key", "wo-idem-0001")]).await;
    assert_eq!((s1, s2), (201, 201));
    assert_eq!(t1, t2);
    assert_eq!(qty(&pool, "PRODUCT", p.p500, "FG_READY").await, d("90"));
    assert_eq!(lot_qty(&pool, "PRODUCT", p.p500, "SCRAP_YARD", b).await, d("10"));
    let wo: Value = serde_json::from_str(&t1).unwrap();
    assert!(wo["wo_no"].as_str().unwrap().starts_with("WO-"));
    // dispose: removes from the books (DISPOSE ledger rows)
    let mut d2 = conf.clone();
    d2["dispose"] = json!(true);
    d2["qty"] = json!(5);
    let (s, _) = c.post("/api/warehouse/write-offs", d2).await;
    assert_eq!(s, 201);
    assert_eq!(lot_qty(&pool, "PRODUCT", p.p500, "SCRAP_YARD", b).await, d("10"));
    let (s, _) = c.post("/api/warehouse/write-offs", json!({ "item_type": "PRODUCT", "item_id": p.p500, "zone": "SCRAP_YARD", "lot_id": b, "qty": 10, "reason_code": "OTHER", "note": "disposed at dump", "confirm": true })).await;
    assert_eq!(s, 201);
    assert_eq!(lot_qty(&pool, "PRODUCT", p.p500, "SCRAP_YARD", b).await, d("0"));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_ledger WHERE txn_type='DISPOSE'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 2);
    // validation
    for (patch, want) in [(json!({ "reason_code": "BORED" }), 422), (json!({ "note": "no" }), 422), (json!({ "qty": 0 }), 422), (json!({ "qty": 1000 }), 409),
                          (json!({ "zone": "QC_QUARANTINE" }), 400), (json!({ "lot_id": Uuid::new_v4() }), 422), (json!({ "bogus": 1 }), 400)] {
        let mut x = conf.clone();
        for (k, v) in patch.as_object().unwrap() { x[k] = v.clone(); }
        let (s, e) = c.post("/api/warehouse/write-offs", x).await;
        assert_eq!(s, want, "{patch} -> {e}");
    }
    // list / filter / detail / csv
    let (s, l) = c.get("/api/warehouse/write-offs?reason=BREAKAGE").await;
    assert_eq!((s, l["total"].clone()), (200, json!(2)));
    let (s, one) = c.get(&format!("/api/warehouse/write-offs/{}", wo["id"].as_str().unwrap())).await;
    assert_eq!((s, one["wo_no"].clone()), (200, wo["wo_no"].clone()));
    let (s, h, csv) = send(&c.app, "GET", "/api/warehouse/write-offs?format=csv", Some(&c.key), None, &[]).await;
    assert_eq!(s, 200);
    assert!(h.get("content-type").unwrap().to_str().unwrap().contains("csv") && csv.contains("forklift accident"));
    // returnable containers leave the fleet => a DESTROYED event is written
    let jar = product(&pool, "JAR20", "RETURNABLE", Some(20000), 1, None).await;
    put_stock(&pool, ItemType::Product, jar, "JAR_EMPTY", Uuid::nil(), "20", None).await;
    let (s, v) = c.post("/api/warehouse/write-offs", json!({ "item_type": "PRODUCT", "item_id": jar, "zone": "JAR_EMPTY", "qty": 3, "reason_code": "BREAKAGE", "note": "cracked jars", "confirm": true })).await;
    assert_eq!(s, 201, "{v}");
    let (ev, reason, loc): (String, String, String) = sqlx::query_as("SELECT event_type, reason_code, location FROM jar_fleet_events WHERE product_id=$1").bind(jar).fetch_one(&pool).await.unwrap();
    assert_eq!((ev.as_str(), reason.as_str(), loc.as_str()), ("DESTROYED", "CRACKED", "PLANT_EMPTY"));
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn reservation_rule_for_fg_ready(pool: PgPool) {
    let c = setup(pool.clone()).await;
    sqlx::query("UPDATE system_settings SET value='1000' WHERE key='entry.large_adjust_pct'").execute(&pool).await.unwrap();
    c.st.settings.reload(&pool).await.unwrap();
    let p = plant(&pool).await;
    let b = approved_batch(&pool, p.p500, 100).await;
    let trip = loading_trip(&pool).await;
    reserve(&pool, trip, p.p500, b, 80).await;
    let adj = |delta: i64| json!({ "item_type": "PRODUCT", "item_id": p.p500, "zone": "FG_READY", "lot_id": b, "delta": delta, "reason": "stock correction" });
    // 100 - 10 = 90 >= 80 reserved: fine
    let (s, _) = c.post("/api/warehouse/adjustments", adj(-10)).await;
    assert_eq!(s, 201);
    // 90 - 30 = 60 < 80: needs confirm and says why
    let (s, e) = c.post("/api/warehouse/adjustments", adj(-30)).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"), "{e}");
    assert!(e["error"]["details"]["warnings"][0].as_str().unwrap().contains("reserved"));
    // scrap transfer and write-off respect it too
    let (s, e) = c.post("/api/warehouse/transfers", json!({ "item_type": "PRODUCT", "item_id": p.p500, "lot_id": b, "from_zone": "FG_READY", "to_zone": "SCRAP_YARD", "qty": 30, "note": "water damage" })).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"), "{e}");
    let (s, e) = c.post("/api/warehouse/write-offs", json!({ "item_type": "PRODUCT", "item_id": p.p500, "zone": "FG_READY", "lot_id": b, "qty": 30, "reason_code": "BREAKAGE", "note": "water damage" })).await;
    assert_eq!(code(&e), "CONFIRM_REQUIRED", "{e}");
    assert!(e["error"]["details"]["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("reserved")));
    let mut ok = adj(-30);
    ok["confirm"] = json!(true);
    let (s, _) = c.post("/api/warehouse/adjustments", ok).await;
    assert_eq!(s, 201);
    // positive adjustments need an APPROVED batch lot
    let quarantined: Uuid = sqlx::query_scalar("INSERT INTO production_batches (batch_no, product_id, crates, expiry_date) VALUES ('QQ1', $1, 5, current_date + 100) RETURNING id").bind(p.p500).fetch_one(&pool).await.unwrap();
    let (s, e) = c.post("/api/warehouse/adjustments", json!({ "item_type": "PRODUCT", "item_id": p.p500, "zone": "FG_READY", "lot_id": quarantined, "delta": 5, "reason": "sneaky add", "confirm": true })).await;
    assert_eq!((s, code(&e)), (409, "QC_LOCKED"), "{e}");
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn reservation_race_write_off_vs_loader_never_both_win(pool: PgPool) {
    let c = setup(pool.clone()).await;
    sqlx::query("UPDATE system_settings SET value='1000' WHERE key='entry.large_adjust_pct'").execute(&pool).await.unwrap();
    c.st.settings.reload(&pool).await.unwrap();
    let p = plant(&pool).await;
    let trip = loading_trip(&pool).await;
    let mut both = 0;
    for round in 0..12 {
        let b = approved_batch(&pool, p.p500, 100).await;
        let (app, key, pool2) = (c.app.clone(), c.key.clone(), pool.clone());
        let adj = tokio::spawn(async move {
            call(&app, "POST", "/api/warehouse/adjustments", Some(&key),
                Some(json!({ "item_type": "PRODUCT", "item_id": p.p500, "zone": "FG_READY", "lot_id": b, "delta": -60, "reason": format!("race round {round}") }))).await
        });
        // the loader exactly as the loading module does it: lock the batch row, then check fg_free, then reserve
        let loader = tokio::spawn(async move {
            let mut tx = pool2.begin().await.unwrap();
            sqlx::query("SELECT id FROM production_batches WHERE id=$1 FOR UPDATE").bind(b).execute(&mut *tx).await.unwrap();
            let free = stock::fg_free(&mut tx, p.p500, b).await.unwrap();
            if free < Decimal::from(60) { return false; }
            sqlx::query("INSERT INTO trip_items (trip_id, batch_id, product_id, crates) VALUES ($1,$2,$3,60)").bind(trip).bind(b).bind(p.p500).execute(&mut *tx).await.unwrap();
            tx.commit().await.unwrap();
            true
        });
        let (s, v) = adj.await.unwrap();
        let loaded = loader.await.unwrap();
        let adjusted = s == 201;
        assert!(adjusted || code(&v) == "CONFIRM_REQUIRED", "{s} {v}");
        if adjusted && loaded { both += 1; }
        let fg = lot_qty(&pool, "PRODUCT", p.p500, "FG_READY", b).await;
        let reserved: Decimal = stock::fg_reserved(&mut pool.acquire().await.unwrap(), b).await.unwrap();
        assert!(reserved <= fg, "trip holds {reserved} but only {fg} in FG_READY (round {round})");
    }
    assert_eq!(both, 0, "write-off and loader must never both succeed silently");
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn physical_count_flow(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let m = material(&pool, "M1", "CAP", "1", "0", None).await;
    let n = material(&pool, "M2", "LABEL", "1", "0", None).await;
    let (l1, l2) = (Uuid::new_v4(), Uuid::new_v4());
    put_stock(&pool, ItemType::Raw, m, "RM_STORE", l1, "100", None).await;
    put_stock(&pool, ItemType::Raw, n, "RM_STORE", l2, "200", None).await;
    let (s, e) = c.post("/api/warehouse/counts", json!({ "zone": "JAR_CLEAN" })).await;
    assert_eq!((s, code(&e)), (400, "BAD_REQUEST"), "{e}");
    let (s, cnt) = c.post("/api/warehouse/counts", json!({ "zone": "RM_STORE", "note": "monthly" })).await;
    assert_eq!(s, 201, "{cnt}");
    assert!(cnt["cnt_no"].as_str().unwrap().starts_with("CNT-"));
    let id = cnt["id"].as_str().unwrap().to_string();
    let lines = cnt["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    let line = |name: &str| lines.iter().find(|l| l["name"] == name).unwrap()["line_id"].clone();
    // someone adjusts after the snapshot: the variance applies to the CURRENT balance
    let (s, _) = c.post("/api/warehouse/adjustments", json!({ "item_type": "RAW", "item_id": m, "zone": "RM_STORE", "lot_id": l1, "delta": 5, "reason": "late receipt" })).await;
    assert_eq!(s, 201);
    // large variance needs a note; foreign line id rejected
    let (s, _) = c.put(&format!("/api/warehouse/counts/{id}/lines"), json!({ "lines": [{ "line_id": Uuid::new_v4(), "counted_qty": 1 }] })).await;
    assert_eq!(s, 422);
    let (s, v) = c.put(&format!("/api/warehouse/counts/{id}/lines"), json!({ "lines": [{ "line_id": line("M1"), "counted_qty": 60 }, { "line_id": line("M2"), "counted_qty": 200 }] })).await;
    assert_eq!(s, 200, "{v}");
    let l = v["lines"].as_array().unwrap().iter().find(|l| l["name"] == "M1").unwrap().clone();
    assert_eq!((dv(&l["variance"]), dv(&l["variance_pct"])), (d("-40"), d("-40")));
    let (s, e) = c.post(&format!("/api/warehouse/counts/{id}/post"), json!({ "confirm": true })).await;
    assert_eq!((s, code(&e)), (422, "VALIDATION"), "{e}");
    let (s, _) = c.put(&format!("/api/warehouse/counts/{id}/lines"), json!({ "lines": [{ "line_id": line("M1"), "counted_qty": 60, "note": "pallet damaged by rain" }] })).await;
    assert_eq!(s, 200);
    // posting always needs confirm; dry run previews
    let (s, e) = c.post(&format!("/api/warehouse/counts/{id}/post"), json!({})).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"), "{e}");
    let (s, dry) = c.post(&format!("/api/warehouse/counts/{id}/post?dry_run=1"), json!({})).await;
    assert_eq!(s, 200, "{dry}");
    assert_eq!(dry["effects"].as_array().unwrap().len(), 1);
    assert_eq!(qty(&pool, "RAW", m, "RM_STORE").await, d("105"));
    let (s, p) = c.post(&format!("/api/warehouse/counts/{id}/post"), json!({ "confirm": true })).await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(p["status"], "POSTED");
    assert_eq!(qty(&pool, "RAW", m, "RM_STORE").await, d("60")); // counted - current(105) = -45 applied
    assert_eq!(qty(&pool, "RAW", n, "RM_STORE").await, d("200"));
    let note: String = sqlx::query_scalar("SELECT note FROM stock_ledger WHERE ref_type='COUNT' LIMIT 1").fetch_one(&pool).await.unwrap();
    assert!(note.contains("stock count CNT-"));
    // final
    let (s, e) = c.post(&format!("/api/warehouse/counts/{id}/post"), json!({ "confirm": true })).await;
    assert_eq!((s, code(&e)), (409, "INVALID_STATE"), "{e}");
    let (s, _) = c.put(&format!("/api/warehouse/counts/{id}/lines"), json!({ "lines": [{ "line_id": line("M1"), "counted_qty": 1 }] })).await;
    assert_eq!(s, 409);
    let (s, _) = c.post(&format!("/api/warehouse/counts/{id}/cancel"), json!({})).await;
    assert_eq!(s, 409);
    // cancel path
    let (_, c2) = c.post("/api/warehouse/counts", json!({ "zone": "RM_STORE" })).await;
    let (s, v) = c.post(&format!("/api/warehouse/counts/{}/cancel", c2["id"].as_str().unwrap()), json!({})).await;
    assert_eq!((s, v["status"].clone()), (200, json!("CANCELLED")));
    let (s, l) = c.get("/api/warehouse/counts?status=POSTED").await;
    assert_eq!((s, l["total"].clone()), (200, json!(1)));
    // negative / over-precise counts
    let (_, c3) = c.post("/api/warehouse/counts", json!({ "zone": "RM_STORE" })).await;
    let l3 = c3["lines"][0]["line_id"].clone();
    let (s, _) = c.put(&format!("/api/warehouse/counts/{}/lines", c3["id"].as_str().unwrap()), json!({ "lines": [{ "line_id": l3, "counted_qty": -1 }] })).await;
    assert_eq!(s, 422);
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn views_summary_pallets_ledger_filters_and_csv_injection(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    let b = approved_batch(&pool, p.p500, 130).await;
    let m = material(&pool, "=EVIL", "CAP", "1", "50", None).await;
    put_stock(&pool, ItemType::Raw, m, "RM_STORE", Uuid::nil(), "10", Some(future(10))).await;
    put_stock(&pool, ItemType::Raw, m, "RM_STORE", Uuid::new_v4(), "5", Some(future(3))).await;
    let (s, st) = c.get("/api/warehouse/stock?zone=FG_READY").await;
    assert_eq!(s, 200);
    assert_eq!((st[0]["pallets"].clone(), st[0]["loose_crates"].clone()), (json!(2), json!("10.0000")));
    let (_, low) = c.get("/api/warehouse/stock?low=1").await;
    assert!(low.as_array().unwrap().iter().any(|r| r["item_id"] == json!(m) && r["zone"] == "RM_STORE"));
    let (_, sm) = c.get("/api/warehouse/stock/summary").await;
    assert_eq!(sm["zones"].as_array().unwrap().len(), 7);
    let prod = sm["products"].as_array().unwrap().iter().find(|x| x["product_id"] == json!(p.p500)).unwrap().clone();
    assert_eq!((prod["fg_ready_pallets"].clone(), dv(&prod["fg_ready"])), (json!(2), d("130")));
    let (s, it) = c.get(&format!("/api/warehouse/stock/item?item_type=RAW&item_id={m}")).await;
    assert_eq!(s, 200, "{it}");
    assert_eq!(it["lots"].as_array().unwrap().len(), 2);
    assert_eq!(dv(&it["lots"][0]["qty"]), d("5")); // FEFO: earliest expiry first
    let (s, _) = c.get("/api/warehouse/stock/item?item_type=RAW").await;
    assert_eq!(s, 422);
    let (s, _) = c.get(&format!("/api/warehouse/stock/item?item_type=RAW&item_id={}", Uuid::new_v4())).await;
    assert_eq!(s, 404);
    let (s, ex) = c.get("/api/warehouse/expiring?days=5").await;
    assert_eq!((s, ex.as_array().unwrap().len()), (200, 1));
    // ledger: filters + paging + injection-safe params
    let (s, l) = c.get(&format!("/api/warehouse/ledger?item_type=RAW&item_id={m}&txn_type=GRN&limit=1")).await;
    assert_eq!((s, l["total"].clone(), l["items"].as_array().unwrap().len()), (200, json!(2), 1));
    let (s, _) = c.get("/api/warehouse/ledger?txn_type=GRN'%20OR%201=1").await;
    assert_eq!(s, 422);
    // a malicious free-text reason lands in the CSV neutralised
    let (s, _) = c.post("/api/warehouse/adjustments", json!({ "item_type": "RAW", "item_id": m, "zone": "RM_STORE", "lot_id": Uuid::nil(), "delta": -1, "reason": "=cmd|' /C calc'!A0 recount", "confirm": true })).await;
    assert_eq!(s, 201);
    let (s, _, csv) = send(&c.app, "GET", &format!("/api/warehouse/ledger?format=csv&item_id={m}&txn_type=ADJUST"), Some(&c.key), None, &[]).await;
    assert_eq!(s, 200);
    assert!(csv.contains("'=cmd|"), "{csv}");
    assert!(csv.contains("'-1.0000"), "negative deltas are prefixed too: {csv}");
    assert!(!csv.contains(",=cmd"));
    let (_, csv2) = { let (s, _, t) = send(&c.app, "GET", "/api/warehouse/stock?format=csv", Some(&c.key), None, &[]).await; (s, t) };
    assert!(csv2.contains("'=EVIL"), "{csv2}");
    // access
    let store = c.user("store", &["raw-materials"]).await;
    for uri in ["/api/warehouse/stock", "/api/warehouse/ledger", "/api/warehouse/write-offs", "/api/warehouse/counts", "/api/warehouse/expiring"] {
        let (s, _) = call(&c.app, "GET", uri, Some(&store), None).await;
        assert_eq!(s, 403, "{uri}");
    }
    let _ = b;
}

#[sqlx::test(migrations = "./migrations")]
async fn dashboard_hides_alerts_and_kpis_of_pages_the_caller_cannot_open(pool: PgPool) {
    let c = setup(pool.clone()).await;
    for (ev, key, title) in [("JAR_OVERDUE", "k1", "Customer Ravi Stores holds 12 jars"), ("LOW_STOCK", "k2", "Low stock: caps"), ("MAINTENANCE_DUE", "k3", "RO plant service")] {
        sqlx::query("INSERT INTO alerts (event_type, dedupe_key, title) VALUES ($1,$2,$3)").bind(ev).bind(key).bind(title).execute(&pool).await.unwrap();
    }
    let plain = c.user("plain", &["dashboard"]).await;
    let (s, v) = call(&c.app, "GET", "/api/dashboard", Some(&plain), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["recent_alerts"].as_array().unwrap().len(), 0, "{v}");
    assert!(!v.to_string().contains("Ravi"));
    for k in ["jars_overdue", "maintenance_due", "low_stock_count", "fg_ready_crates", "trips_on_road", "batches_in_quarantine", "water_log_overdue"] { assert!(v["kpis"][k].is_null(), "{k}"); }
    let jars = c.user("jarsuser", &["dashboard", "jars"]).await;
    let (_, v) = call(&c.app, "GET", "/api/dashboard", Some(&jars), None).await;
    let titles: Vec<&str> = v["recent_alerts"].as_array().unwrap().iter().map(|a| a["title"].as_str().unwrap()).collect();
    assert_eq!(titles, vec!["Customer Ravi Stores holds 12 jars"]);
    assert_eq!(v["kpis"]["jars_overdue"], 1);
    assert!(v["kpis"]["maintenance_due"].is_null());
    let (_, v) = c.get("/api/dashboard").await;
    assert_eq!(v["recent_alerts"].as_array().unwrap().len(), 3);
    assert_eq!(v["kpis"]["maintenance_due"], 1);
}
