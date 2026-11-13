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
    ]
}

#[sqlx::test(migrations = "./migrations")]
async fn seed_is_idempotent_and_consistent(pool: PgPool) {
    water_erp::seed::run(&pool).await.unwrap();
    let first = snapshot(&pool).await;
    assert!(first.iter().all(|n| *n > 0), "{first:?}");
    water_erp::seed::run(&pool).await.unwrap();
    assert_eq!(first, snapshot(&pool).await, "second run must not change anything (incl. stock)");

    // ledger invariant: balance == sum of deltas for every (item, zone, lot)
    let bad: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stock_balances b WHERE b.qty <> COALESCE((SELECT sum(l.qty_delta) FROM stock_ledger l WHERE l.item_type=b.item_type AND l.item_id=b.item_id AND l.zone=b.zone AND l.lot_id=b.lot_id),0)")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(bad, 0);
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
    for (u, admin, pages) in [("admin", true, 9usize), ("manager", false, 9), ("loader", false, 4), ("qc", false, 2), ("gate", false, 2), ("store", false, 3), ("operator", false, 2), ("jars", false, 3)] {
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
