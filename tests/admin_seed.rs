mod common;
mod admin_support;
use admin_support::req;
use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;

async fn snapshot(pool: &PgPool) -> Vec<i64> {
    let q = |sql: &'static str| { let pool = pool.clone(); async move { sqlx::query_scalar::<_, i64>(sql).fetch_one(&pool).await.unwrap() } };
    vec![
        q("SELECT count(*) FROM users").await, q("SELECT count(*) FROM suppliers").await, q("SELECT count(*) FROM raw_materials").await, q("SELECT count(*) FROM products").await,
        q("SELECT count(*) FROM bom_items").await, q("SELECT count(*) FROM customers").await, q("SELECT count(*) FROM vehicles").await, q("SELECT count(*) FROM equipment").await,
        q("SELECT count(*) FROM maintenance_schedules").await, q("SELECT count(*) FROM stock_ledger").await, q("SELECT count(*) FROM stock_balances").await,
        q("SELECT COALESCE(sum(qty),0)::bigint FROM stock_balances").await, q("SELECT count(*) FROM production_batches").await, q("SELECT count(*) FROM jar_ledger").await,
        q("SELECT count(*) FROM jar_fleet_events").await, q("SELECT count(*) FROM grns").await, q("SELECT count(*) FROM qr_crate_sets").await, q("SELECT count(*) FROM water_treatment_logs").await,
        q("SELECT count(*) FROM blow_molding_runs").await, q("SELECT count(*) FROM routes").await,
        q("SELECT count(*) FROM locations").await, q("SELECT count(*) FROM godown_transfers").await, q("SELECT count(*) FROM godown_transfer_lines").await,
        q("SELECT count(*) FROM stock_balances WHERE location_id <> '00000000-0000-0000-0000-000000000001'").await,
        q("SELECT COALESCE(sum(qty),0)::bigint FROM stock_balances WHERE location_id <> '00000000-0000-0000-0000-000000000001'").await,
    ]
}

#[sqlx::test(migrations = "./migrations")]
async fn seed_is_idempotent_and_consistent(pool: PgPool) {
    water_erp::seed::run(&pool).await.unwrap();
    let first = snapshot(&pool).await;
    assert!(first.iter().all(|n| *n > 0), "{first:?}");
    water_erp::seed::run(&pool).await.unwrap();
    assert_eq!(first, snapshot(&pool).await, "second run must not change anything (incl. stock)");

    // ledger invariant: balance == sum of deltas for every (location, item, zone, lot), and no ledger row without a balance row
    let bad: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stock_balances b WHERE b.qty <> COALESCE((SELECT sum(l.qty_delta) FROM stock_ledger l WHERE l.location_id=b.location_id AND l.item_type=b.item_type AND l.item_id=b.item_id AND l.zone=b.zone AND l.lot_id=b.lot_id),0)")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(bad, 0);
    let orphan: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT DISTINCT location_id, item_type, item_id, zone, lot_id FROM stock_ledger) l
         WHERE NOT EXISTS (SELECT 1 FROM stock_balances b WHERE b.location_id=l.location_id AND b.item_type=l.item_type AND b.item_id=l.item_id AND b.zone=l.zone AND b.lot_id=l.lot_id)")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(orphan, 0);
    // all ledger rows carry the seed marker (nothing anonymous)
    let unmarked: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_ledger WHERE note NOT LIKE 'seed-demo%'").fetch_one(&pool).await.unwrap();
    assert_eq!(unmarked, 0);

    // approved batches with FG_READY stock and different expiries; a QUARANTINE batch sitting in QC_QUARANTINE
    let approved: Vec<(String, i64)> = sqlx::query_as(
        "SELECT p.code, count(DISTINCT b.expiry_date) FROM production_batches b JOIN products p ON p.id=b.product_id
         JOIN stock_balances s ON s.item_type='PRODUCT' AND s.item_id=b.product_id AND s.zone='FG_READY' AND s.lot_id=b.id AND s.qty>0
         WHERE b.qc_status='APPROVED' GROUP BY p.code ORDER BY p.code").fetch_all(&pool).await.unwrap();
    assert_eq!(approved.len(), 3, "{approved:?}");
    let multi: i64 = sqlx::query_scalar("SELECT count(DISTINCT b.expiry_date) FROM production_batches b JOIN products p ON p.id=b.product_id WHERE p.code='MW-500' AND b.qc_status='APPROVED'").fetch_one(&pool).await.unwrap();
    assert_eq!(multi, 2, "FEFO visible: two approved 500 ml batches with different expiries");
    let quarantine: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM production_batches b JOIN stock_balances s ON s.lot_id=b.id AND s.zone='QC_QUARANTINE' AND s.qty>0 WHERE b.qc_status='QUARANTINE'").fetch_one(&pool).await.unwrap();
    assert!(quarantine >= 1);
    let fg_in_quarantine_batch: i64 = sqlx::query_scalar("SELECT count(*) FROM production_batches b JOIN stock_balances s ON s.lot_id=b.id AND s.zone='FG_READY' AND s.qty>0 WHERE b.qc_status<>'APPROVED'").fetch_one(&pool).await.unwrap();
    assert_eq!(fg_in_quarantine_batch, 0, "only APPROVED goods may be in FG_READY");

    // plant-specific model
    let kinds: Vec<(String, String, Option<i32>, Option<i32>, bool, bool, Option<i32>)> = sqlx::query_as(
        "SELECT code, kind, size_ml, crates_per_pallet, preform_material_id IS NOT NULL, bottle_material_id IS NOT NULL, max_cycles FROM products ORDER BY code").fetch_all(&pool).await.unwrap();
    assert_eq!(kinds, vec![
        ("MW-1000".into(), "BOTTLED".into(), Some(1000), Some(80), true, true, None),
        ("MW-20L".into(), "RETURNABLE".into(), Some(20000), None, false, false, Some(50)),
        ("MW-500".into(), "BOTTLED".into(), Some(500), Some(60), true, true, None)]);
    let bad_bom: i64 = sqlx::query_scalar("SELECT count(*) FROM bom_items b JOIN products p ON p.id=b.product_id JOIN raw_materials m ON m.id=b.raw_material_id
        WHERE (p.kind='RETURNABLE' AND m.category IN ('PREFORM','EMPTY_BOTTLE')) OR (m.category='EMPTY_BOTTLE' AND b.source_zone<>'BLOW_FLOOR') OR (m.category<>'EMPTY_BOTTLE' AND b.source_zone<>'RM_STORE')").fetch_one(&pool).await.unwrap();
    assert_eq!(bad_bom, 0);
    let per_l: i64 = sqlx::query_scalar("SELECT count(*) FROM bom_items WHERE basis='PER_1000L'").fetch_one(&pool).await.unwrap();
    assert!(per_l >= 6);
    let cats: i64 = sqlx::query_scalar("SELECT count(DISTINCT category) FROM raw_materials").fetch_one(&pool).await.unwrap();
    assert_eq!(cats, 9);
    // expiry lots in RM_STORE, empty bottles on the blow floor
    let lots: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_balances WHERE zone='RM_STORE' AND expiry_date IS NOT NULL AND qty>0").fetch_one(&pool).await.unwrap();
    assert!(lots >= 6);
    let bottles: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_balances s JOIN raw_materials m ON m.id=s.item_id WHERE s.zone='BLOW_FLOOR' AND m.category='EMPTY_BOTTLE' AND s.qty>0").fetch_one(&pool).await.unwrap();
    assert_eq!(bottles, 2);
    // fleet: stock in both jar zones, customers holding jars, events incl. DESTROYED with reasons
    let (e, c): (i64, i64) = (
        sqlx::query_scalar("SELECT COALESCE(sum(qty),0)::bigint FROM stock_balances WHERE zone='JAR_EMPTY'").fetch_one(&pool).await.unwrap(),
        sqlx::query_scalar("SELECT COALESCE(sum(qty),0)::bigint FROM stock_balances WHERE zone='JAR_CLEAN'").fetch_one(&pool).await.unwrap());
    assert!(e > 0 && c > 0);
    let holders: i64 = sqlx::query_scalar("SELECT count(*) FROM (SELECT customer_id FROM jar_ledger GROUP BY customer_id HAVING sum(delta) > 0) x").fetch_one(&pool).await.unwrap();
    assert!(holders >= 3);
    let neg: i64 = sqlx::query_scalar("SELECT count(*) FROM (SELECT customer_id FROM jar_ledger GROUP BY customer_id, product_id HAVING sum(delta) < 0) x").fetch_one(&pool).await.unwrap();
    assert_eq!(neg, 0);
    let ev: Vec<String> = sqlx::query_scalar("SELECT DISTINCT event_type FROM jar_fleet_events ORDER BY 1").fetch_all(&pool).await.unwrap();
    assert_eq!(ev, vec!["DESTROYED", "NEW_PURCHASED", "WASHED"]);
    // vehicles: one with expired PUC; schedules: one overdue
    let expired: i64 = sqlx::query_scalar("SELECT count(*) FROM vehicles WHERE puc_expiry < CURRENT_DATE").fetch_one(&pool).await.unwrap();
    assert_eq!(expired, 1);
    let overdue: i64 = sqlx::query_scalar("SELECT count(*) FROM maintenance_schedules WHERE next_due_on < CURRENT_DATE").fetch_one(&pool).await.unwrap();
    assert!(overdue >= 1);
    // QR sets exist but feature stays off
    let sets: i64 = sqlx::query_scalar("SELECT count(*) FROM qr_crate_sets").fetch_one(&pool).await.unwrap();
    assert!(sets >= 6);
    let flag: serde_json::Value = sqlx::query_scalar("SELECT value FROM system_settings WHERE key='features.qr_labels'").fetch_one(&pool).await.unwrap();
    assert_eq!(flag, json!(false));
    let over: i64 = sqlx::query_scalar("SELECT count(*) FROM (SELECT b.id FROM production_batches b JOIN qr_crate_sets s ON s.batch_id=b.id AND s.parent_set_id IS NULL GROUP BY b.id, b.crates HAVING sum(s.original_crates) > b.crates) x").fetch_one(&pool).await.unwrap();
    assert_eq!(over, 0);

    // demo accounts: right pages, the documented password works through the real login
    let (app, _) = common::app(pool.clone()).await;
    for (u, admin, pages) in [("admin", true, 9usize), ("manager", false, 9), ("loader", false, 4), ("qc", false, 2), ("gate", false, 2), ("store", false, 3), ("store_north", false, 3), ("operator", false, 2), ("jars", false, 3)] {
        let (s, b) = req(&app, "POST", "/api/auth/login", None, Some(json!({"username": u, "password": "Demo@12345!"}))).await;
        assert_eq!(s, StatusCode::OK, "{u}: {b}");
        assert_eq!(b["user"]["is_admin"], admin, "{u}");
        if !admin { assert_eq!(b["user"]["allowed_pages"].as_array().unwrap().len(), pages, "{u}"); }
    }
    // the admin can read everything through the admin API
    let (_, b) = req(&app, "POST", "/api/auth/login", None, Some(json!({"username":"admin","password":"Demo@12345!"}))).await;
    let k = b["key"].as_str().unwrap();
    for (p, n) in [("products", 3), ("suppliers", 3), ("customers", 5), ("vehicles", 3)] {
        let (s, l) = req(&app, "GET", &format!("/api/admin/{p}"), Some(k), None).await;
        assert_eq!((s, l.as_array().unwrap().len()), (StatusCode::OK, n), "{p}");
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn seed_never_overwrites_existing_users(pool: PgPool) {
    sqlx::query("INSERT INTO users (username, full_name, password_hash, is_admin) VALUES ('admin','Real Admin','$argon2id$custom',true)").execute(&pool).await.unwrap();
    water_erp::seed::run(&pool).await.unwrap();
    let (h, n): (String, String) = sqlx::query_as("SELECT password_hash, full_name FROM users WHERE username='admin'").fetch_one(&pool).await.unwrap();
    assert_eq!((h.as_str(), n.as_str()), ("$argon2id$custom", "Real Admin"));
}

#[sqlx::test(migrations = "./migrations")]
async fn seed_godowns_transfers_and_restricted_keeper(pool: PgPool) {
    use water_erp::stock::{MAIN_LOCATION, TRANSIT_LOCATION};
    water_erp::seed::run(&pool).await.unwrap();
    water_erp::seed::run(&pool).await.unwrap();   // idempotent (counts compared in the other test; here: no duplicate godowns/users)
    let gd: Vec<(String, String, bool)> = sqlx::query_as("SELECT code, kind, is_production_site FROM locations WHERE kind <> 'TRANSIT' ORDER BY code").fetch_all(&pool).await.unwrap();
    assert_eq!(gd, vec![("GDN-CITY".into(), "GODOWN".into(), false), ("GDN-NORTH".into(), "GODOWN".into(), false), ("MAIN".into(), "PLANT".into(), true)]);
    let north: uuid::Uuid = sqlx::query_scalar("SELECT id FROM locations WHERE code='GDN-NORTH'").fetch_one(&pool).await.unwrap();
    let city: uuid::Uuid = sqlx::query_scalar("SELECT id FROM locations WHERE code='GDN-CITY'").fetch_one(&pool).await.unwrap();

    // each godown has its own RM_STORE, FG_READY and JAR_EMPTY stock, with different lots / expiries
    for (loc, name) in [(north, "north"), (city, "city")] {
        for zone in ["RM_STORE", "FG_READY", "JAR_EMPTY"] {
            let q: i64 = sqlx::query_scalar("SELECT COALESCE(sum(qty),0)::bigint FROM stock_balances WHERE location_id=$1 AND zone=$2").bind(loc).bind(zone).fetch_one(&pool).await.unwrap();
            assert!(q > 0, "{name} {zone}");
        }
    }
    let lots = |loc: uuid::Uuid, zone: &'static str| { let pool = pool.clone(); async move {
        sqlx::query_scalar::<_, uuid::Uuid>("SELECT lot_id FROM stock_balances WHERE location_id=$1 AND zone=$2 AND qty>0 ORDER BY lot_id").bind(loc).bind(zone).fetch_all(&pool).await.unwrap() } };
    let (nf, cf) = (lots(north, "FG_READY").await, lots(city, "FG_READY").await);
    assert!(nf.iter().any(|l| !cf.contains(l)) && cf.iter().any(|l| !nf.contains(l)), "godowns hold different FG lots");
    let exp: Vec<(uuid::Uuid, Option<chrono::NaiveDate>)> = sqlx::query_as(
        "SELECT a.item_id, a.expiry_date FROM stock_balances a JOIN stock_balances b ON b.item_id=a.item_id AND b.zone='RM_STORE' AND b.location_id=$2 AND b.qty>0 AND b.expiry_date IS DISTINCT FROM a.expiry_date
         WHERE a.zone='RM_STORE' AND a.location_id=$1 AND a.qty>0").bind(north).bind(city).fetch_all(&pool).await.unwrap();
    assert!(!exp.is_empty(), "same material, different expiry in the two godowns");

    // transfers: exactly one IN_TRANSIT, two RECEIVED; TRANSIT holds exactly what is still on the road
    let st: Vec<(String, i64)> = sqlx::query_as("SELECT status, count(*) FROM godown_transfers GROUP BY status ORDER BY status").fetch_all(&pool).await.unwrap();
    assert_eq!(st, vec![("IN_TRANSIT".to_string(), 1), ("RECEIVED".to_string(), 2)]);
    let on_road: rust_decimal::Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(l.qty_sent - COALESCE(l.qty_received,0) - COALESCE(l.qty_damaged,0)),0) FROM godown_transfer_lines l JOIN godown_transfers t ON t.id=l.transfer_id WHERE t.status='IN_TRANSIT'").fetch_one(&pool).await.unwrap();
    let in_transit_balance: rust_decimal::Decimal = sqlx::query_scalar("SELECT COALESCE(sum(qty),0) FROM stock_balances WHERE location_id=$1").bind(TRANSIT_LOCATION).fetch_one(&pool).await.unwrap();
    assert!(on_road > rust_decimal::Decimal::ZERO);
    assert_eq!(in_transit_balance, on_road);
    let bad_lines: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM godown_transfer_lines l JOIN godown_transfers t ON t.id=l.transfer_id WHERE t.status='RECEIVED' AND l.qty_received + l.qty_damaged <> l.qty_sent").fetch_one(&pool).await.unwrap();
    assert_eq!(bad_lines, 0);
    // one damaged crate sits in the City godown's scrap yard (TRANSIT_DAMAGE)
    let dmg: rust_decimal::Decimal = sqlx::query_scalar("SELECT COALESCE(sum(qty_delta),0) FROM stock_ledger WHERE txn_type='TRANSIT_DAMAGE' AND location_id=$1 AND zone='SCRAP_YARD'").bind(city).fetch_one(&pool).await.unwrap();
    assert_eq!(dmg, rust_decimal::Decimal::ONE);

    // conservation: every transferred item has the same total over all locations as the ledger says it should (nothing vanished)
    let leak: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT item_type, item_id, lot_id, sum(qty) q FROM stock_balances GROUP BY 1,2,3) b
         JOIN (SELECT item_type, item_id, lot_id, sum(qty_delta) d FROM stock_ledger GROUP BY 1,2,3) l USING (item_type, item_id, lot_id) WHERE b.q <> l.d").fetch_one(&pool).await.unwrap();
    assert_eq!(leak, 0);
    // GODOWN_TRANSFER_OUT / IN ledger rows net to zero per transfer except for what is still in transit
    let net: i64 = sqlx::query_scalar("SELECT COALESCE(sum(qty_delta),0)::bigint FROM stock_ledger WHERE txn_type IN ('GODOWN_TRANSFER_OUT','GODOWN_TRANSFER_IN','TRANSIT_DAMAGE')").fetch_one(&pool).await.unwrap();
    assert_eq!(net, 0, "every move has two legs");
    // MAIN lost exactly what the godowns and TRANSIT gained from transfers
    let main_out: rust_decimal::Decimal = sqlx::query_scalar("SELECT COALESCE(-sum(qty_delta),0) FROM stock_ledger WHERE txn_type='GODOWN_TRANSFER_OUT' AND location_id=$1").bind(MAIN_LOCATION).fetch_one(&pool).await.unwrap();
    let recv: rust_decimal::Decimal = sqlx::query_scalar("SELECT COALESCE(sum(qty_delta),0) FROM stock_ledger WHERE txn_type IN ('GODOWN_TRANSFER_IN','TRANSIT_DAMAGE') AND location_id IN ($1,$2)").bind(north).bind(city).fetch_one(&pool).await.unwrap();
    assert_eq!(main_out, recv + on_road);

    // the restricted demo keeper: only North, three pages, sees it in Me and cannot use the other godowns
    let (app, st) = common::app(pool.clone()).await;
    let (s, b) = req(&app, "POST", "/api/auth/login", None, Some(json!({"username":"store_north","password":"Demo@12345!"}))).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["user"]["allowed_location_ids"], json!([north]));
    let mut pages: Vec<String> = b["user"]["allowed_pages"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
    pages.sort();
    assert_eq!(pages, vec!["dashboard", "raw-materials", "warehouse"]);
    let au = water_erp::auth::authenticate(&st, b["key"].as_str().unwrap()).await.unwrap();
    assert!(au.can_use_location(north) && !au.can_use_location(city) && !au.can_use_location(MAIN_LOCATION));
    let others: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE username <> 'store_north' AND allowed_location_ids <> '[]'::jsonb").fetch_one(&pool).await.unwrap();
    assert_eq!(others, 0, "only the north keeper is restricted");
    let tf_receiver: Option<uuid::Uuid> = sqlx::query_scalar("SELECT received_by FROM godown_transfers WHERE to_location_id=$1 AND status='RECEIVED'").bind(north).fetch_one(&pool).await.unwrap();
    assert_eq!(tf_receiver.unwrap(), au.id);
}
