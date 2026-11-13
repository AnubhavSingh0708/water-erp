mod common;
mod materials_fixtures;
use materials_fixtures::*;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::ItemType;

const TRANSIT: Uuid = Uuid::from_u128(2);
const MAIN: Uuid = Uuid::from_u128(1);

fn line(item: Uuid, lot: Uuid, qty: i64) -> Value { json!({ "item_type": "RAW", "item_id": item, "lot_id": lot, "zone": "RM_STORE", "qty": qty }) }
fn tf(from: Uuid, to: Uuid, lines: Vec<Value>) -> Value { json!({ "from_location_id": from, "to_location_id": to, "lines": lines }) }
fn recv(t: &Value, rec: i64, dmg: i64, note: &str) -> Value {
    json!({ "lines": t["lines"].as_array().unwrap().iter().map(|l| json!({ "line_id": l["line_id"], "qty_received": rec, "qty_damaged": dmg, "note": note })).collect::<Vec<_>>() })
}

#[sqlx::test(migrations = "./migrations")]
async fn every_view_and_write_is_isolated_per_godown(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let sup = supplier(&pool, "S", 3).await;
    let cap = material(&pool, "CAP", "CAP", "1000", "0", Some(sup)).await;
    let (g1, g2) = (godown(&pool, "G1", false).await, godown(&pool, "G2", false).await);

    let (s, g) = c.post("/api/raw-materials/grns", json!({ "location_id": g1, "supplier_id": sup, "challan_no": "A1", "lines": [{ "raw_material_id": cap, "qty_purchase": 3 }] })).await;
    assert_eq!(s, 201, "{g}");
    assert_eq!(g["location_id"], json!(g1));
    let lot = uid(&g["lines"][0]["lot_id"]);
    assert_eq!((qty_at(&pool, g1, "RAW", cap, "RM_STORE").await, qty_at(&pool, MAIN, "RAW", cap, "RM_STORE").await, qty_at(&pool, g2, "RAW", cap, "RM_STORE").await), (d("3000"), d("0"), d("0")));

    // stock views
    let (_, st) = c.get(&format!("/api/warehouse/stock?location_id={g1}")).await;
    assert_eq!((st.as_array().unwrap().len(), st[0]["location_id"].clone()), (1, json!(g1)));
    assert_eq!(c.get(&format!("/api/warehouse/stock?location_id={g2}")).await.1.as_array().unwrap().len(), 0);
    assert_eq!(c.get("/api/warehouse/stock").await.1.as_array().unwrap().len(), 0, "default godown is MAIN");
    let (_, all) = c.get("/api/warehouse/stock?location_id=all").await;
    assert_eq!((all.as_array().unwrap().len(), all[0]["location_name"].clone()), (1, json!("G1")));
    let (_, m) = c.get(&format!("/api/raw-materials?location_id={g1}")).await;
    assert_eq!(dv(&m[0]["stock_rm_store"]), d("3000"));
    let (_, m) = c.get(&format!("/api/raw-materials?location_id={g2}")).await;
    assert_eq!(dv(&m[0]["stock_rm_store"]), d("0"));
    let (_, it) = c.get(&format!("/api/warehouse/stock/item?item_type=RAW&item_id={cap}&location_id={g2}")).await;
    assert_eq!(it["lots"].as_array().unwrap().len(), 0);
    let (_, it) = c.get(&format!("/api/warehouse/stock/item?item_type=RAW&item_id={cap}&location_id=all")).await;
    assert_eq!(it["lots"][0]["location_name"], "G1");
    // ledger
    let (_, l) = c.get(&format!("/api/warehouse/ledger?location_id={g1}")).await;
    assert!(l["total"].as_i64().unwrap() >= 1 && l["items"].as_array().unwrap().iter().all(|r| r["location_id"] == json!(g1)));
    assert_eq!(c.get("/api/warehouse/ledger").await.1["total"], 0);
    // summary by-location
    let (_, sm) = c.get("/api/warehouse/stock/summary?location_id=all").await;
    let row = sm["by_location"].as_array().unwrap().iter().find(|r| r["location_id"] == json!(g1)).unwrap().clone();
    assert_eq!(row["raw_items"], 1);

    // adjustment / write-off / count / supplier return act on one godown only
    let (s, _) = c.post("/api/warehouse/adjustments", json!({ "location_id": g2, "item_type": "RAW", "item_id": cap, "zone": "RM_STORE", "delta": 10, "reason": "opening stock G2", "confirm": true })).await;
    assert_eq!(s, 201);
    assert_eq!((qty_at(&pool, g1, "RAW", cap, "RM_STORE").await, qty_at(&pool, g2, "RAW", cap, "RM_STORE").await), (d("3000"), d("10")));
    let (s, w) = c.post("/api/warehouse/write-offs", json!({ "location_id": g1, "item_type": "RAW", "item_id": cap, "zone": "RM_STORE", "lot_id": lot, "qty": 100, "reason_code": "SPILLAGE", "note": "bag burst", "confirm": true })).await;
    assert_eq!(s, 201, "{w}");
    assert_eq!(w["location_id"], json!(g1));
    assert_eq!((qty_at(&pool, g1, "RAW", cap, "RM_STORE").await, qty_at(&pool, g1, "RAW", cap, "SCRAP_YARD").await, qty_at(&pool, g2, "RAW", cap, "SCRAP_YARD").await), (d("2900"), d("100"), d("0")));
    // the same lot id cannot be written off at a godown that does not hold it
    let (s, e) = c.post("/api/warehouse/write-offs", json!({ "location_id": g2, "item_type": "RAW", "item_id": cap, "zone": "RM_STORE", "lot_id": lot, "qty": 5, "reason_code": "SPILLAGE", "note": "wrong godown", "confirm": true })).await;
    assert_eq!((s, code(&e)), (409, "INSUFFICIENT_STOCK"), "{e}");
    assert_eq!(c.get(&format!("/api/warehouse/write-offs?location_id={g2}")).await.1["total"], 0);
    assert_eq!(c.get(&format!("/api/warehouse/write-offs?location_id={g1}")).await.1["total"], 1);
    let (s, cnt) = c.post("/api/warehouse/counts", json!({ "location_id": g2, "zone": "RM_STORE" })).await;
    assert_eq!(s, 201, "{cnt}");
    assert_eq!(cnt["lines"].as_array().unwrap().len(), 1);
    assert_eq!(cnt["location_id"], json!(g2));
    let (s, r) = c.post("/api/raw-materials/supplier-returns", json!({ "location_id": g1, "supplier_id": sup, "lines": [{ "raw_material_id": cap, "lot_id": lot, "qty_stock": 50, "reason": "damaged bags" }] })).await;
    assert_eq!(s, 201, "{r}");
    assert_eq!(qty_at(&pool, g1, "RAW", cap, "RM_STORE").await, d("2850"));
    // an in-godown zone transfer
    let (s, _) = c.post("/api/warehouse/transfers", json!({ "location_id": g1, "item_type": "RAW", "item_id": cap, "lot_id": lot, "from_zone": "RM_STORE", "to_zone": "BLOW_FLOOR", "qty": 50, "confirm": true })).await;
    assert_eq!(s, 201);
    assert_eq!((qty_at(&pool, g1, "RAW", cap, "BLOW_FLOOR").await, qty_at(&pool, g2, "RAW", cap, "BLOW_FLOOR").await), (d("50"), d("0")));
    // GRN list filter + detail
    assert_eq!(c.get(&format!("/api/raw-materials/grns?location_id={g2}")).await.1["total"], 0);
    assert_eq!(c.get(&format!("/api/raw-materials/grns?location_id={g1}")).await.1["total"], 1);
    // expiring
    put_stock_at(&pool, g2, ItemType::Raw, cap, "RM_STORE", Uuid::new_v4(), "5", Some(future(3))).await;
    assert_eq!(c.get(&format!("/api/warehouse/expiring?days=10&location_id={g1}")).await.1.as_array().unwrap().len(), 0);
    let (_, e) = c.get("/api/warehouse/expiring?days=10&location_id=all").await;
    assert_eq!((e.as_array().unwrap().len(), e[0]["location_name"].clone()), (1, json!("G2")));
    // ref data lists only selectable godowns
    let (_, locs) = c.get("/api/ref/locations").await;
    let ids: Vec<String> = locs.as_array().unwrap().iter().map(|l| l["id"].as_str().unwrap().to_string()).collect();
    assert!(ids.contains(&g1.to_string()) && ids.contains(&g2.to_string()) && ids.contains(&MAIN.to_string()) && !ids.contains(&TRANSIT.to_string()));
    // unknown / transit godown ids are validation errors
    assert_eq!(c.get(&format!("/api/warehouse/stock?location_id={}", Uuid::new_v4())).await.0, 422);
    assert_eq!(c.get(&format!("/api/warehouse/stock?location_id={TRANSIT}")).await.0, 422);
    assert_eq!(c.get("/api/warehouse/stock?location_id=nonsense").await.0, 422);
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn production_and_qc_run_at_production_sites_only(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    let (site, store) = (godown(&pool, "PLANT2", true).await, godown(&pool, "STORE", false).await);
    put_stock_at(&pool, site, ItemType::Raw, p.bottle, "BLOW_FLOOR", Uuid::nil(), "5000", None).await;
    put_stock_at(&pool, site, ItemType::Raw, p.cap, "RM_STORE", Uuid::new_v4(), "5000", None).await;
    put_stock_at(&pool, site, ItemType::Raw, p.mineral, "RM_STORE", Uuid::new_v4(), "50", None).await;
    put_stock_at(&pool, site, ItemType::Raw, p.preform, "RM_STORE", Uuid::new_v4(), "5000", None).await;
    fresh_water_log(&pool).await;
    let body = |loc: Option<Uuid>| { let mut b = json!({ "product_id": p.p500, "crates": 10, "shift": "DAY", "confirm": true }); if let Some(l) = loc { b["location_id"] = json!(l); } b };
    let (s, e) = c.post("/api/production/batches", body(Some(store))).await;
    assert_eq!((s, code(&e)), (422, "VALIDATION"), "{e}");
    // default godown MAIN has no stock => cannot produce there
    let (s, e) = c.post("/api/production/batches", body(None)).await;
    assert_eq!((s, code(&e)), (409, "INSUFFICIENT_STOCK"), "{e}");
    let (s, b) = c.post("/api/production/batches", body(Some(site))).await;
    assert_eq!(s, 201, "{b}");
    let bid = uid(&b["id"]);
    assert_eq!(b["location_id"], json!(site));
    assert_eq!(qty_at(&pool, site, "RAW", p.cap, "RM_STORE").await, d("4760"));
    assert_eq!(lot_qty(&pool, "PRODUCT", p.p500, "QC_QUARANTINE", bid).await, d("10"));
    assert_eq!(qty_at(&pool, MAIN, "PRODUCT", p.p500, "QC_QUARANTINE").await, d("0"));
    // blow run
    let blow = |loc: Uuid| json!({ "location_id": loc, "shift": "DAY", "preform_material_id": p.preform, "bottle_material_id": p.bottle, "preforms_issued": 100, "good_bottles": 99 });
    assert_eq!(c.post("/api/production/blow-runs", blow(store)).await.0, 422);
    let (s, r) = c.post("/api/production/blow-runs", blow(site)).await;
    assert_eq!(s, 201, "{r}");
    assert_eq!(qty_at(&pool, site, "RAW", p.preform, "RM_STORE").await, d("4900"));
    // QC: a user restricted to another godown cannot decide; the admin releases at the batch's godown
    let other = restricted(&c, "qcother", &["qc", "production"], &[store]).await;
    let url = format!("/api/qc/batches/{bid}");
    let (s, _) = call(&c.app, "POST", &format!("{url}/tests"), Some(&other), Some(json!({ "result": "PASS" }))).await;
    assert_eq!(s, 403);
    let (s, _) = call(&c.app, "GET", &url, Some(&other), None).await;
    assert_eq!(s, 403);
    assert_eq!(call(&c.app, "GET", "/api/qc/batches", Some(&other), None).await.1["total"], 0);
    let (s, _) = c.post(&format!("{url}/tests"), json!({ "result": "PASS" })).await;
    assert_eq!(s, 201);
    let (s, v) = c.post(&format!("{url}/approve"), json!({})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(lot_qty(&pool, "PRODUCT", p.p500, "FG_READY", bid).await, d("10"));
    assert_eq!(qty_at(&pool, site, "PRODUCT", p.p500, "FG_READY").await, d("10"));
    assert_eq!(qty_at(&pool, MAIN, "PRODUCT", p.p500, "FG_READY").await, d("0"));
    // production list is filtered by godown
    assert_eq!(c.get(&format!("/api/production/batches?location_id={site}")).await.1["total"], 1);
    assert_eq!(c.get(&format!("/api/production/batches?location_id={store}")).await.1["total"], 0);
    assert_eq!(call(&c.app, "GET", "/api/production/batches", Some(&other), None).await.1["total"], 0);
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn transfer_happy_path_validation_and_dry_run(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let cap = material(&pool, "CAP", "CAP", "1", "0", None).await;
    let (g1, g2) = (godown(&pool, "G1", false).await, godown(&pool, "G2", false).await);
    let lot = Uuid::new_v4();
    put_stock_at(&pool, g1, ItemType::Raw, cap, "RM_STORE", lot, "1000", Some(future(300))).await;
    let (s, dry) = c.post("/api/godowns/transfers?dry_run=1", tf(g1, g2, vec![line(cap, lot, 400)])).await;
    assert_eq!(s, 200, "{dry}");
    assert_eq!(dry["effects"].as_array().unwrap().len(), 2);
    assert_eq!(qty_at(&pool, g1, "RAW", cap, "RM_STORE").await, d("1000"));
    // validation
    for (body, want) in [
        (tf(g1, g1, vec![line(cap, lot, 1)]), 422),
        (tf(g1, TRANSIT, vec![line(cap, lot, 1)]), 422),
        (tf(g1, Uuid::new_v4(), vec![line(cap, lot, 1)]), 422),
        (tf(g1, g2, vec![]), 422),
        (tf(g1, g2, vec![line(cap, lot, 1), line(cap, lot, 2)]), 422),
        (tf(g1, g2, vec![line(cap, lot, 0)]), 422),
        (tf(g1, g2, vec![line(cap, lot, 5000)]), 409),
        (tf(g1, g2, vec![line(cap, Uuid::new_v4(), 1)]), 409),
        (tf(g1, g2, vec![line(Uuid::new_v4(), lot, 1)]), 422),
        (tf(g1, g2, vec![json!({ "item_type": "RAW", "item_id": cap, "lot_id": lot, "zone": "QC_QUARANTINE", "qty": 1 })]), 422),
        (tf(g1, g2, vec![json!({ "item_type": "RAW", "item_id": cap, "lot_id": lot, "zone": "SCRAP_YARD", "qty": 1 })]), 422),
        (json!({ "from_location_id": g1, "to_location_id": g2, "lines": [line(cap, lot, 1)], "status": "RECEIVED" }), 400),
    ] {
        let (s, e) = c.post("/api/godowns/transfers", body.clone()).await;
        assert_eq!(s, want, "{body} -> {e}");
    }
    assert_eq!(qty_at(&pool, g1, "RAW", cap, "RM_STORE").await, d("1000"));
    let (s, t) = c.post("/api/godowns/transfers", { let mut b = tf(g1, g2, vec![line(cap, lot, 400)]); b["driver_name"] = json!("Ramesh"); b["vehicle_no"] = json!("MH12AB1234"); b }).await;
    assert_eq!(s, 201, "{t}");
    assert_eq!(t["status"], "IN_TRANSIT");
    assert!(t["tf_no"].as_str().unwrap().starts_with("TF-"));
    assert_eq!((qty_at(&pool, g1, "RAW", cap, "RM_STORE").await, qty_at(&pool, TRANSIT, "RAW", cap, "RM_STORE").await, qty_at(&pool, g2, "RAW", cap, "RM_STORE").await), (d("600"), d("400"), d("0")));
    check_transit(&pool).await;
    check_invariants(&pool).await;
    let id = t["id"].as_str().unwrap();
    // receive: dry run first, then the real thing; expiry travels with the lot
    let (s, dry) = c.post(&format!("/api/godowns/transfers/{id}/receive?dry_run=1"), recv(&t, 400, 0, "")).await;
    assert_eq!(s, 200, "{dry}");
    assert_eq!(qty_at(&pool, TRANSIT, "RAW", cap, "RM_STORE").await, d("400"));
    let (s, r) = c.post(&format!("/api/godowns/transfers/{id}/receive"), recv(&t, 400, 0, "")).await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["status"], "RECEIVED");
    assert_eq!((qty_at(&pool, g2, "RAW", cap, "RM_STORE").await, qty_at(&pool, TRANSIT, "RAW", cap, "RM_STORE").await), (d("400"), d("0")));
    let exp: Option<chrono::NaiveDate> = sqlx::query_scalar("SELECT expiry_date FROM stock_balances WHERE location_id=$1 AND lot_id=$2").bind(g2).bind(lot).fetch_one(&pool).await.unwrap();
    assert!(exp.is_some());
    assert_eq!(qty_everywhere(&pool, "RAW", cap).await, d("1000"));
    check_transit(&pool).await;
    check_invariants(&pool).await;
    // list / detail / filters
    let (_, l) = c.get(&format!("/api/godowns/transfers?status=RECEIVED&location_id={g2}")).await;
    assert_eq!(l["total"], 1);
    assert_eq!(c.get("/api/godowns/transfers?status=IN_TRANSIT").await.1["total"], 0);
    assert_eq!(c.get("/api/godowns/transfers?status=BOGUS").await.0, 422);
    let (s, one) = c.get(&format!("/api/godowns/transfers/{id}")).await;
    assert_eq!((s, dv(&one["lines"][0]["qty_received"]), dv(&one["lines"][0]["qty_short"])), (200, d("400"), d("0")));
    // the ledger shows both legs
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_ledger WHERE ref_type='GODOWN_TRANSFER' AND ref_id=$1").bind(uid(&t["id"])).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 4);
}

#[sqlx::test(migrations = "./migrations")]
async fn transfer_damage_shortage_and_cancel(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let cap = material(&pool, "CAP", "CAP", "1", "0", None).await;
    let (g1, g2) = (godown(&pool, "G1", false).await, godown(&pool, "G2", false).await);
    let lot = Uuid::new_v4();
    put_stock_at(&pool, g1, ItemType::Raw, cap, "RM_STORE", lot, "1000", None).await;
    let (_, t) = c.post("/api/godowns/transfers", tf(g1, g2, vec![line(cap, lot, 100)])).await;
    let id = t["id"].as_str().unwrap();
    let url = format!("/api/godowns/transfers/{id}/receive");
    // over-delivery, incomplete line list, missing note, missing confirm
    assert_eq!(c.post(&url, recv(&t, 100, 10, "")).await.0, 422);
    assert_eq!(c.post(&url, json!({ "lines": [] })).await.0, 422);
    assert_eq!(c.post(&url, json!({ "lines": [{ "line_id": Uuid::new_v4(), "qty_received": 1 }] })).await.0, 422);
    assert_eq!(c.post(&url, recv(&t, 80, 10, "")).await.0, 422, "shortage needs a note");
    let (s, e) = c.post(&url, recv(&t, 80, 10, "10 bags missing at unloading")).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"), "{e}");
    assert_eq!(qty_at(&pool, TRANSIT, "RAW", cap, "RM_STORE").await, d("100"));
    let mut ok = recv(&t, 80, 10, "10 bags missing at unloading");
    ok["confirm"] = json!(true);
    let (s, r) = c.post(&url, ok.clone()).await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(dv(&r["lines"][0]["qty_short"]), d("10"));
    assert_eq!((qty_at(&pool, g2, "RAW", cap, "RM_STORE").await, qty_at(&pool, g2, "RAW", cap, "SCRAP_YARD").await, qty_at(&pool, TRANSIT, "RAW", cap, "RM_STORE").await), (d("80"), d("10"), d("0")));
    assert_eq!(qty_everywhere(&pool, "RAW", cap).await, d("990"), "only the documented shortage left the books");
    let short: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_ledger WHERE txn_type='TRANSFER_SHORT' AND qty_delta = -10").fetch_one(&pool).await.unwrap();
    assert_eq!(short, 1);
    let a: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='GODOWN_TRANSFER_RECEIVE'").fetch_one(&pool).await.unwrap();
    assert_eq!(a, 1);
    // final
    let (s, e) = c.post(&url, ok).await;
    assert_eq!((s, code(&e)), (409, "INVALID_STATE"));
    check_transit(&pool).await;
    check_invariants(&pool).await;

    // cancel path
    let (_, t2) = c.post("/api/godowns/transfers", tf(g1, g2, vec![line(cap, lot, 300)])).await;
    let cu = format!("/api/godowns/transfers/{}/cancel", t2["id"].as_str().unwrap());
    assert_eq!(c.post(&cu, json!({ "reason": "x", "confirm": true })).await.0, 422);
    let (s, e) = c.post(&cu, json!({ "reason": "vehicle broke down" })).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"), "{e}");
    assert_eq!(qty_at(&pool, g1, "RAW", cap, "RM_STORE").await, d("600"));
    let (s, v) = c.post(&cu, json!({ "reason": "vehicle broke down", "confirm": true })).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "CANCELLED");
    assert_eq!((qty_at(&pool, g1, "RAW", cap, "RM_STORE").await, qty_at(&pool, TRANSIT, "RAW", cap, "RM_STORE").await), (d("900"), d("0")));
    let (s, e) = c.post(&cu, json!({ "reason": "vehicle broke down", "confirm": true })).await;
    assert_eq!((s, code(&e)), (409, "INVALID_STATE"));
    let (s, e) = c.post(&format!("/api/godowns/transfers/{}/receive", t2["id"].as_str().unwrap()), recv(&t2, 300, 0, "")).await;
    assert_eq!((s, code(&e)), (409, "INVALID_STATE"), "{e}");
    check_transit(&pool).await;
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_receive_has_exactly_one_winner(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let cap = material(&pool, "CAP", "CAP", "1", "0", None).await;
    let (g1, g2) = (godown(&pool, "G1", false).await, godown(&pool, "G2", false).await);
    let lot = Uuid::new_v4();
    put_stock_at(&pool, g1, ItemType::Raw, cap, "RM_STORE", lot, "500", None).await;
    let (_, t) = c.post("/api/godowns/transfers", tf(g1, g2, vec![line(cap, lot, 500)])).await;
    let url = format!("/api/godowns/transfers/{}/receive", t["id"].as_str().unwrap());
    let mut hs = vec![];
    for i in 0..8 {
        let (app, key, u, body) = (c.app.clone(), c.key.clone(), url.clone(), recv(&t, 500, 0, ""));
        hs.push(tokio::spawn(async move {
            if i % 2 == 0 { call(&app, "POST", &u, Some(&key), Some(body)).await }
            else { call(&app, "POST", &u.replace("/receive", "/cancel"), Some(&key), Some(json!({ "reason": "cancelling it", "confirm": true }))).await }
        }));
    }
    let mut wins = 0;
    for h in hs {
        let (s, v) = h.await.unwrap();
        if s == 200 { wins += 1 } else { assert_eq!((s, code(&v)), (409, "INVALID_STATE"), "{v}") }
    }
    assert_eq!(wins, 1);
    assert_eq!(qty_everywhere(&pool, "RAW", cap).await, d("500"));
    assert_eq!(qty_at(&pool, TRANSIT, "RAW", cap, "RM_STORE").await, d("0"));
    check_transit(&pool).await;
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn qc_lots_and_reserved_crates_cannot_be_transferred(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    let (g1, g2) = (godown(&pool, "G1", false).await, godown(&pool, "G2", false).await);
    let prod = |batch: Uuid, qty: i64| json!({ "item_type": "PRODUCT", "item_id": p.p500, "lot_id": batch, "zone": "FG_READY", "qty": qty });
    let quarantined: Uuid = sqlx::query_scalar("INSERT INTO production_batches (batch_no, product_id, crates, expiry_date, location_id) VALUES ('QQ', $1, 50, current_date + 100, $2) RETURNING id").bind(p.p500).bind(g1).fetch_one(&pool).await.unwrap();
    put_stock_at(&pool, g1, ItemType::Product, p.p500, "QC_QUARANTINE", quarantined, "50", None).await;
    // quarantine zone, a quarantined batch lot in FG_READY (bad data), and a rejected batch are all refused
    let mut bad = tf(g1, g2, vec![json!({ "item_type": "PRODUCT", "item_id": p.p500, "lot_id": quarantined, "zone": "QC_QUARANTINE", "qty": 5 })]);
    assert_eq!(c.post("/api/godowns/transfers", bad.clone()).await.0, 422);
    put_stock_at(&pool, g1, ItemType::Product, p.p500, "FG_READY", quarantined, "5", None).await;
    bad = tf(g1, g2, vec![prod(quarantined, 5)]);
    let (s, e) = c.post("/api/godowns/transfers", bad).await;
    assert_eq!((s, code(&e)), (409, "QC_LOCKED"), "{e}");
    assert_eq!(qty_at(&pool, TRANSIT, "PRODUCT", p.p500, "FG_READY").await, d("0"));
    // approved batch with reservations
    let b: Uuid = sqlx::query_scalar("INSERT INTO production_batches (batch_no, product_id, crates, qc_status, expiry_date, location_id) VALUES ('AA', $1, 100, 'APPROVED', current_date + 100, $2) RETURNING id").bind(p.p500).bind(g1).fetch_one(&pool).await.unwrap();
    put_stock_at(&pool, g1, ItemType::Product, p.p500, "FG_READY", b, "100", Some(future(100))).await;
    let veh: Uuid = sqlx::query_scalar("INSERT INTO vehicles (reg_no, capacity_crates) VALUES ('V1', 500) RETURNING id").fetch_one(&pool).await.unwrap();
    let trip: Uuid = sqlx::query_scalar("INSERT INTO loading_trips (trip_no, vehicle_id, capacity_crates, location_id) VALUES ('T1', $1, 500, $2) RETURNING id").bind(veh).bind(g1).fetch_one(&pool).await.unwrap();
    sqlx::query("INSERT INTO trip_items (trip_id, batch_id, product_id, crates) VALUES ($1,$2,$3,80)").bind(trip).bind(b).bind(p.p500).execute(&pool).await.unwrap();
    let (s, e) = c.post("/api/godowns/transfers", tf(g1, g2, vec![prod(b, 30)])).await;
    assert_eq!((s, code(&e)), (409, "INSUFFICIENT_STOCK"), "{e}");
    assert!(e["error"]["message"].as_str().unwrap().contains("reserved"));
    let (s, t) = c.post("/api/godowns/transfers", tf(g1, g2, vec![prod(b, 20)])).await;
    assert_eq!(s, 201, "{t}");
    assert_eq!(qty_at(&pool, g1, "PRODUCT", p.p500, "FG_READY").await, d("85"));
    // fractional crates are refused
    assert_eq!(c.post("/api/godowns/transfers", tf(g1, g2, vec![json!({ "item_type": "PRODUCT", "item_id": p.p500, "lot_id": b, "zone": "FG_READY", "qty": "0.5" })])).await.0, 422);
    // receive the approved crates at the destination: lot keeps its identity
    let (s, _) = c.post(&format!("/api/godowns/transfers/{}/receive", t["id"].as_str().unwrap()), recv(&t, 20, 0, "")).await;
    assert_eq!(s, 200);
    assert_eq!(qty_at(&pool, g2, "PRODUCT", p.p500, "FG_READY").await, d("20"));
    // jar zones need a RETURNABLE product and no lot
    assert_eq!(c.post("/api/godowns/transfers", tf(g1, g2, vec![json!({ "item_type": "PRODUCT", "item_id": p.p500, "lot_id": Uuid::nil(), "zone": "JAR_CLEAN", "qty": 1 })])).await.0, 422);
    let jar = product(&pool, "JAR20", "RETURNABLE", Some(20000), 1, None).await;
    put_stock_at(&pool, g1, ItemType::Product, jar, "JAR_CLEAN", Uuid::nil(), "40", None).await;
    let (s, jt) = c.post("/api/godowns/transfers", tf(g1, g2, vec![json!({ "item_type": "PRODUCT", "item_id": jar, "zone": "JAR_CLEAN", "qty": 15 })])).await;
    assert_eq!(s, 201, "{jt}");
    check_transit(&pool).await;
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn restricted_users_see_and_touch_only_their_godowns(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let cap = material(&pool, "CAP", "CAP", "1", "0", None).await;
    let (g1, g2, g3) = (godown(&pool, "G1", false).await, godown(&pool, "G2", false).await, godown(&pool, "G3", false).await);
    let lot = Uuid::new_v4();
    for g in [g1, g2, g3] { put_stock_at(&pool, g, ItemType::Raw, cap, "RM_STORE", lot, "100", None).await; }
    let u1 = restricted(&c, "g1user", &["warehouse", "raw-materials", "dashboard"], &[g1]).await;
    let u2 = restricted(&c, "g2user", &["warehouse"], &[g2]).await;
    // reads: explicit foreign godown is 403, lists silently filter, single-godown users default to their godown
    assert_eq!(call(&c.app, "GET", &format!("/api/warehouse/stock?location_id={g2}"), Some(&u1), None).await.0, 403);
    let (_, st) = call(&c.app, "GET", "/api/warehouse/stock", Some(&u1), None).await;
    assert_eq!((st.as_array().unwrap().len(), st[0]["location_id"].clone()), (1, json!(g1)));
    let (_, st) = call(&c.app, "GET", "/api/warehouse/stock?location_id=all", Some(&u1), None).await;
    assert_eq!(st.as_array().unwrap().len(), 1);
    let (_, l) = call(&c.app, "GET", "/api/ref/locations", Some(&u1), None).await;
    assert_eq!(l.as_array().unwrap().len(), 1);
    let (_, sm) = call(&c.app, "GET", "/api/warehouse/stock/summary?location_id=all", Some(&u1), None).await;
    assert_eq!(sm["by_location"].as_array().unwrap().len(), 1);
    // writes
    for body in [json!({ "location_id": g2, "item_type": "RAW", "item_id": cap, "zone": "RM_STORE", "lot_id": lot, "delta": -1, "reason": "not my godown", "confirm": true })] {
        assert_eq!(call(&c.app, "POST", "/api/warehouse/adjustments", Some(&u1), Some(body)).await.0, 403);
    }
    assert_eq!(call(&c.app, "POST", "/api/warehouse/write-offs", Some(&u1), Some(json!({ "location_id": g3, "item_type": "RAW", "item_id": cap, "zone": "RM_STORE", "lot_id": lot, "qty": 1, "reason_code": "OTHER", "note": "not allowed", "confirm": true }))).await.0, 403);
    assert_eq!(call(&c.app, "POST", "/api/warehouse/counts", Some(&u1), Some(json!({ "location_id": g2, "zone": "RM_STORE" }))).await.0, 403);
    assert_eq!(call(&c.app, "POST", "/api/raw-materials/grns", Some(&u1), Some(json!({ "location_id": g2, "supplier_id": supplier(&pool, "S", 1).await, "lines": [{ "raw_material_id": cap, "qty_purchase": 1 }] }))).await.0, 403);
    assert_eq!(qty_at(&pool, g2, "RAW", cap, "RM_STORE").await, d("100"));
    // a count / write-off document of another godown is invisible and untouchable
    let (_, cnt) = c.post("/api/warehouse/counts", json!({ "location_id": g2, "zone": "RM_STORE" })).await;
    let cid = cnt["id"].as_str().unwrap();
    assert_eq!(call(&c.app, "GET", &format!("/api/warehouse/counts/{cid}"), Some(&u1), None).await.0, 403);
    assert_eq!(call(&c.app, "POST", &format!("/api/warehouse/counts/{cid}/cancel"), Some(&u1), Some(json!({}))).await.0, 403);
    assert_eq!(call(&c.app, "GET", "/api/warehouse/counts", Some(&u1), None).await.1["total"], 0);
    assert_eq!(call(&c.app, "GET", "/api/warehouse/counts", Some(&u2), None).await.1["total"], 1);
    // transfers: caller must be allowed at the SENDING godown to send, at the RECEIVING godown to receive
    assert_eq!(call(&c.app, "POST", "/api/godowns/transfers", Some(&u1), Some(tf(g2, g1, vec![line(cap, lot, 5)]))).await.0, 403);
    let (s, t) = call(&c.app, "POST", "/api/godowns/transfers", Some(&u1), Some(tf(g1, g2, vec![line(cap, lot, 30)]))).await;
    assert_eq!(s, 201, "{t}");
    let id = t["id"].as_str().unwrap();
    assert_eq!(call(&c.app, "POST", &format!("/api/godowns/transfers/{id}/receive"), Some(&u1), Some(recv(&t, 30, 0, ""))).await.0, 403, "sender cannot receive");
    assert_eq!(call(&c.app, "POST", &format!("/api/godowns/transfers/{id}/cancel"), Some(&u2), Some(json!({ "reason": "not the sender", "confirm": true }))).await.0, 403, "receiver cannot cancel");
    assert_eq!(call(&c.app, "GET", &format!("/api/godowns/transfers/{id}"), Some(&u2), None).await.0, 200, "receiver sees the incoming transfer");
    let u3 = restricted(&c, "g3user", &["warehouse"], &[g3]).await;
    assert_eq!(call(&c.app, "GET", &format!("/api/godowns/transfers/{id}"), Some(&u3), None).await.0, 403);
    assert_eq!(call(&c.app, "GET", "/api/godowns/transfers", Some(&u3), None).await.1["total"], 0);
    assert_eq!(call(&c.app, "GET", "/api/godowns/transfers", Some(&u1), None).await.1["total"], 1);
    let (s, _) = call(&c.app, "POST", &format!("/api/godowns/transfers/{id}/receive"), Some(&u2), Some(recv(&t, 30, 0, ""))).await;
    assert_eq!(s, 200);
    // no page, no access
    let nopage = restricted(&c, "nopage", &["qc"], &[g1]).await;
    assert_eq!(call(&c.app, "GET", "/api/godowns/transfers", Some(&nopage), None).await.0, 403);
    check_transit(&pool).await;
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn dashboard_in_transit_warning_and_reorder_totals_across_godowns(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let sup = supplier(&pool, "S", 3).await;
    let m = material(&pool, "CAP", "CAP", "100", "100", Some(sup)).await;
    let (g1, g2) = (godown(&pool, "G1", false).await, godown(&pool, "G2", false).await);
    let lot = Uuid::new_v4();
    put_stock_at(&pool, g1, ItemType::Raw, m, "RM_STORE", lot, "60", None).await;
    put_stock_at(&pool, g2, ItemType::Raw, m, "RM_STORE", Uuid::new_v4(), "60", None).await;
    // 60 + 60 across godowns >= minimum 100: nothing to reorder
    water_erp::workers::reorder::run_once(&c.st).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM reorder_recommendations WHERE status='OPEN'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
    // goods in TRANSIT are not in any store: total 80 < 100 => recommendation, which lists where stock is
    let (_, t) = c.post("/api/godowns/transfers", tf(g1, g2, vec![line(m, lot, 40)])).await;
    water_erp::workers::reorder::run_once(&c.st).await.unwrap();
    let (_, recs) = c.get("/api/raw-materials/reorder-recommendations").await;
    assert_eq!(recs.as_array().unwrap().len(), 1, "{recs}");
    assert_eq!(recs[0]["stock_by_location"].as_array().unwrap().len(), 2);
    // dashboard: nothing overdue yet, then 3 days in transit
    let (_, dash) = c.get("/api/dashboard").await;
    assert_eq!(dash["in_transit_warnings"].as_array().unwrap().len(), 0);
    assert_eq!(dash["kpis"]["in_transit_transfers"], 1);
    sqlx::query("UPDATE godown_transfers SET created_at = now() - interval '3 days'").execute(&pool).await.unwrap();
    let (_, dash) = c.get("/api/dashboard").await;
    assert_eq!(dash["in_transit_warnings"].as_array().unwrap().len(), 1);
    assert_eq!(dash["in_transit_warnings"][0]["tf_no"], t["tf_no"]);
    assert_eq!(dash["by_location"].as_array().unwrap().len(), 3);
    let (_, one) = c.get(&format!("/api/dashboard?location_id={g1}")).await;
    assert_eq!(one["by_location"].as_array().unwrap().len(), 1);
    let (_, l) = c.get("/api/godowns/transfers?status=IN_TRANSIT").await;
    assert_eq!(l["items"][0]["overdue"], true);
    check_transit(&pool).await;
}
