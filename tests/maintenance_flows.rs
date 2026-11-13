mod common;
use axum::{http::StatusCode, Router};
use chrono::{Duration, NaiveDate};
use common::{app, call, make_user, session_for};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::{
    state::AppState,
    stock::{self, ItemType, Move, NIL_LOT},
    workers::maintenance::run_once,
};

struct Fx { app: Router, st: AppState, key: String, today: NaiveDate }

async fn fx(pool: PgPool) -> Fx {
    let (app, st) = app(pool.clone()).await;
    let u = make_user(&pool, "maint", false, &["maintenance"], &[]).await;
    let key = session_for(&pool, u).await;
    let mut c = pool.acquire().await.unwrap();
    let today = water_erp::util::plant_today(&mut c).await.unwrap();
    Fx { app, st, key, today }
}
async fn equipment(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO equipment (code,name,kind) VALUES ('E'||floor(random()*1e9)::text,$1,'RO') RETURNING id").bind(name).fetch_one(pool).await.unwrap()
}
async fn vehicle(pool: &PgPool, reg: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO vehicles (reg_no,capacity_crates) VALUES ($1,100) RETURNING id").bind(reg).fetch_one(pool).await.unwrap()
}
async fn sched(pool: &PgPool, eq: Option<Uuid>, veh: Option<Uuid>, task: &str, interval: Option<i32>, next: NaiveDate) -> Uuid {
    sqlx::query_scalar("INSERT INTO maintenance_schedules (subject_type,equipment_id,vehicle_id,task,interval_days,next_due_on) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id")
        .bind(if eq.is_some() { "EQUIPMENT" } else { "VEHICLE" }).bind(eq).bind(veh).bind(task).bind(interval).bind(next).fetch_one(pool).await.unwrap()
}
async fn open_alerts(pool: &PgPool, ty: &str) -> Vec<(String, String)> {
    sqlx::query_as("SELECT dedupe_key, severity FROM alerts WHERE event_type=$1 AND resolved_at IS NULL ORDER BY dedupe_key").bind(ty).fetch_all(pool).await.unwrap()
}
async fn all_alerts(pool: &PgPool, ty: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM alerts WHERE event_type=$1").bind(ty).fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn schedule_and_document_statuses(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let d = |n: i64| f.today + Duration::days(n);
    let eq = equipment(&pool, "RO Plant").await;
    let v = vehicle(&pool, "MH-01-AA-1").await;
    let s_over = sched(&pool, Some(eq), None, "Replace filters", Some(90), d(-1)).await;
    let s_today = sched(&pool, Some(eq), None, "Check pressure", None, d(0)).await;
    let s_soon = sched(&pool, None, Some(v), "Service", Some(180), d(30)).await;
    let s_ok = sched(&pool, None, Some(v), "Tyres", Some(180), d(31)).await;
    let inactive = sched(&pool, Some(eq), None, "Retired task", None, d(-50)).await;
    sqlx::query("UPDATE maintenance_schedules SET is_active=false WHERE id=$1").bind(inactive).execute(&pool).await.unwrap();

    let (s, rows) = call(&f.app, "GET", "/api/maintenance/schedules", Some(&f.key), None).await;
    assert_eq!(s, StatusCode::OK, "{rows}");
    let by = |id: Uuid| rows.as_array().unwrap().iter().find(|r| r["id"] == id.to_string()).cloned();
    assert_eq!(by(s_over).unwrap()["status"], "OVERDUE");
    assert_eq!(by(s_over).unwrap()["days_left"], -1);
    assert_eq!(by(s_over).unwrap()["subject_name"], "RO Plant");
    assert_eq!(by(s_today).unwrap()["status"], "DUE_SOON");
    assert_eq!(by(s_soon).unwrap()["status"], "DUE_SOON");
    assert_eq!(by(s_soon).unwrap()["subject_name"], "MH-01-AA-1");
    assert_eq!(by(s_ok).unwrap()["status"], "OK");
    assert!(by(inactive).is_none());
    assert_eq!(rows[0]["id"], s_over.to_string(), "soonest first");
    let (_, only) = call(&f.app, "GET", "/api/maintenance/schedules?status=OVERDUE", Some(&f.key), None).await;
    assert_eq!(only.as_array().unwrap().len(), 1);
    let (_, only) = call(&f.app, "GET", "/api/maintenance/schedules?subject_type=VEHICLE", Some(&f.key), None).await;
    assert_eq!(only.as_array().unwrap().len(), 2);
    for bad in ["status=LATE", "subject_type=BOAT", "status=OK%27%20OR%201=1"] {
        let (s, _) = call(&f.app, "GET", &format!("/api/maintenance/schedules?{bad}"), Some(&f.key), None).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    // the lead time is a live setting
    sqlx::query("UPDATE system_settings SET value='5' WHERE key='alerts.expiry_lead_days'").execute(&pool).await.unwrap();
    f.st.settings.reload(&pool).await.unwrap();
    let (_, rows) = call(&f.app, "GET", "/api/maintenance/schedules?status=DUE_SOON", Some(&f.key), None).await;
    assert_eq!(rows.as_array().unwrap().len(), 1);

    // vehicle documents
    sqlx::query("UPDATE vehicles SET insurance_expiry=$2, fitness_expiry=$3, puc_expiry=$4 WHERE id=$1").bind(v).bind(d(-3)).bind(d(3)).bind(d(200)).execute(&pool).await.unwrap();
    let inactive_v = vehicle(&pool, "OLD-1").await;
    sqlx::query("UPDATE vehicles SET puc_expiry=$2, is_active=false WHERE id=$1").bind(inactive_v).bind(d(-9)).execute(&pool).await.unwrap();
    let (s, docs) = call(&f.app, "GET", "/api/maintenance/vehicle-docs", Some(&f.key), None).await;
    assert_eq!(s, StatusCode::OK, "{docs}");
    let docs = docs.as_array().unwrap();
    assert_eq!(docs.len(), 3, "permit has no date, inactive vehicle hidden");
    assert_eq!((docs[0]["doc"].clone(), docs[0]["status"].clone(), docs[0]["expired"].clone()), (json!("INSURANCE"), json!("OVERDUE"), json!(true)));
    assert_eq!((docs[1]["doc"].clone(), docs[1]["status"].clone(), docs[1]["days_left"].clone()), (json!("FITNESS"), json!("DUE_SOON"), json!(3)));
    assert_eq!(docs[2]["status"], "OK");
    let (_, f_only) = call(&f.app, "GET", &format!("/api/maintenance/vehicle-docs?vehicle_id={v}&status=OVERDUE"), Some(&f.key), None).await;
    assert_eq!(f_only.as_array().unwrap().len(), 1);
    let (s, _) = call(&f.app, "GET", "/api/maintenance/vehicle-docs?vehicle_id=zzz", Some(&f.key), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrations = "./migrations")]
async fn page_guard(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let u = make_user(&pool, "jarsonly", false, &["jars"], &[]).await;
    let k = session_for(&pool, u).await;
    let eq = equipment(&pool, "X").await;
    let id = sched(&pool, Some(eq), None, "t", Some(10), f.today).await;
    for (m, p) in [("GET", "/api/maintenance/schedules".to_string()), ("GET", "/api/maintenance/vehicle-docs".into()), ("POST", format!("/api/maintenance/schedules/{id}/complete")), ("GET", format!("/api/maintenance/schedules/{id}/logs"))] {
        let (s, _) = call(&f.app, m, &p, Some(&k), if m == "POST" { Some(json!({})) } else { None }).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{p}");
    }
    let (s, _) = call(&f.app, "GET", "/api/maintenance/schedules", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn completing_a_schedule(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let d = |n: i64| f.today + Duration::days(n);
    let eq = equipment(&pool, "UV lamp").await;
    let s1 = sched(&pool, Some(eq), None, "Lamp swap", Some(60), d(-5)).await;
    let s2 = sched(&pool, Some(eq), None, "One-off check", None, d(2)).await;
    run_once(&f.st).await.unwrap();
    assert_eq!(open_alerts(&pool, "MAINTENANCE_DUE").await.len(), 2);

    let url = format!("/api/maintenance/schedules/{s1}/complete");
    let (s, v) = call(&f.app, "POST", &url, Some(&f.key), Some(json!({"notes": "done by Raju"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["next_due_on"], d(60).to_string(), "done_on (today) + interval");
    assert_eq!(v["last_done_on"], f.today.to_string());
    assert_eq!(v["status"], "OK");
    // its alert was resolved in the same transaction
    let open = open_alerts(&pool, "MAINTENANCE_DUE").await;
    assert_eq!(open.len(), 1);
    assert!(open[0].0.starts_with(&format!("schedule:{s2}:")));
    // a double submit does not create a second log
    let (s, v) = call(&f.app, "POST", &url, Some(&f.key), Some(json!({}))).await;
    assert_eq!((s, v["error"]["code"].clone()), (StatusCode::CONFLICT, json!("INVALID_STATE")));
    let (_, logs) = call(&f.app, "GET", &format!("/api/maintenance/schedules/{s1}/logs"), Some(&f.key), None).await;
    assert_eq!(logs.as_array().unwrap().len(), 1);
    assert_eq!(logs[0]["notes"], "done by Raju");
    assert_eq!(logs[0]["done_by_name"], "maint");
    let audit: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE action='maintenance.complete'").fetch_one(&pool).await.unwrap();
    assert_eq!(audit, 1);

    // no interval: next_due_on is mandatory and must lie after the done date
    let url2 = format!("/api/maintenance/schedules/{s2}/complete");
    let (s, _) = call(&f.app, "POST", &url2, Some(&f.key), Some(json!({}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = call(&f.app, "POST", &url2, Some(&f.key), Some(json!({"next_due_on": f.today.to_string()}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = call(&f.app, "POST", &url2, Some(&f.key), Some(json!({"done_on": d(1).to_string(), "next_due_on": d(60).to_string()}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "done_on in the future");
    let (s, _) = call(&f.app, "POST", &url2, Some(&f.key), Some(json!({"next_due_on": d(60).to_string(), "extra": 1}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown field");
    let (s, v) = call(&f.app, "POST", &url2, Some(&f.key), Some(json!({"done_on": d(-1).to_string(), "next_due_on": d(60).to_string()}))).await;
    assert_eq!((s, v["days_left"].clone()), (StatusCode::OK, json!(60)));
    let (s, _) = call(&f.app, "POST", &format!("/api/maintenance/schedules/{}/complete", Uuid::new_v4()), Some(&f.key), Some(json!({}))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // backdating before the last completion is refused
    let (s, _) = call(&f.app, "POST", &url2, Some(&f.key), Some(json!({"done_on": d(-3).to_string(), "next_due_on": d(60).to_string()}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    // deactivated schedule
    sqlx::query("UPDATE maintenance_schedules SET is_active=false WHERE id=$1").bind(s1).execute(&pool).await.unwrap();
    let (s, _) = call(&f.app, "POST", &url, Some(&f.key), Some(json!({"done_on": d(-0).to_string()}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[sqlx::test(migrations = "./migrations")]
async fn worker_raises_dedupes_and_resolves_maintenance_and_documents(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let d = |n: i64| f.today + Duration::days(n);
    let eq = equipment(&pool, "Ozonator").await;
    let v = vehicle(&pool, "KA-01-1").await;
    let soon = sched(&pool, Some(eq), None, "Descale", Some(60), d(10)).await;
    let _ok = sched(&pool, Some(eq), None, "Far away", Some(60), d(100)).await;
    sqlx::query("UPDATE vehicles SET puc_expiry=$2, insurance_expiry=$3 WHERE id=$1").bind(v).bind(d(-2)).bind(d(5)).execute(&pool).await.unwrap();

    let s1 = run_once(&f.st).await.unwrap();
    assert_eq!(s1.raised, 3);
    let m = open_alerts(&pool, "MAINTENANCE_DUE").await;
    assert_eq!(m, vec![(format!("schedule:{soon}:{}", d(10)), "WARNING".to_string())]);
    let docs = open_alerts(&pool, "VEHICLE_DOC_EXPIRY").await;
    assert_eq!(docs.len(), 2);
    assert!(docs.contains(&(format!("vehicle:{v}:PUC:{}", d(-2)), "CRITICAL".to_string())));
    assert!(docs.contains(&(format!("vehicle:{v}:INSURANCE:{}", d(5)), "WARNING".to_string())));
    // idempotent: no duplicates on the next pass
    let s2 = run_once(&f.st).await.unwrap();
    assert_eq!((s2.raised, s2.resolved), (0, 0));
    assert_eq!(all_alerts(&pool, "MAINTENANCE_DUE").await, 1);
    assert_eq!(all_alerts(&pool, "VEHICLE_DOC_EXPIRY").await, 2);

    // the schedule slips past its date: same key, severity escalates, no second alert
    sqlx::query("UPDATE maintenance_schedules SET next_due_on=$2 WHERE id=$1").bind(soon).bind(d(10)).execute(&pool).await.unwrap();
    sqlx::query("UPDATE alerts SET severity='INFO' WHERE event_type='MAINTENANCE_DUE'").execute(&pool).await.unwrap();
    run_once(&f.st).await.unwrap();
    assert_eq!(open_alerts(&pool, "MAINTENANCE_DUE").await[0].1, "WARNING", "text/severity refreshed in place");

    // renew the PUC: the old key disappears; the date moved so a new key would only appear if still within the lead time
    sqlx::query("UPDATE vehicles SET puc_expiry=$2 WHERE id=$1").bind(v).bind(d(365)).execute(&pool).await.unwrap();
    let s3 = run_once(&f.st).await.unwrap();
    assert_eq!((s3.raised, s3.resolved), (0, 1));
    assert_eq!(open_alerts(&pool, "VEHICLE_DOC_EXPIRY").await.len(), 1);
    // deactivating the vehicle / schedule resolves the rest
    sqlx::query("UPDATE vehicles SET is_active=false WHERE id=$1").bind(v).execute(&pool).await.unwrap();
    sqlx::query("UPDATE maintenance_schedules SET is_active=false WHERE id=$1").bind(soon).execute(&pool).await.unwrap();
    let s4 = run_once(&f.st).await.unwrap();
    assert_eq!(s4.resolved, 2);
    assert!(open_alerts(&pool, "MAINTENANCE_DUE").await.is_empty());
    assert!(open_alerts(&pool, "VEHICLE_DOC_EXPIRY").await.is_empty());
    // resolved alerts can be raised again later (new episode)
    sqlx::query("UPDATE maintenance_schedules SET is_active=true WHERE id=$1").bind(soon).execute(&pool).await.unwrap();
    assert_eq!(run_once(&f.st).await.unwrap().raised, 1);
    assert_eq!(all_alerts(&pool, "MAINTENANCE_DUE").await, 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn worker_jar_overdue_and_fleet_alerts(pool: PgPool) {
    let f = fx(pool.clone()).await;
    let p: Uuid = sqlx::query_scalar("INSERT INTO products (code,name,kind,units_per_crate,max_cycles) VALUES ('J1','20L Jar','RETURNABLE',1,10) RETURNING id").fetch_one(&pool).await.unwrap();
    let c1: Uuid = sqlx::query_scalar("INSERT INTO customers (code,name) VALUES ('C1','Late Payer') RETURNING id").fetch_one(&pool).await.unwrap();
    let c2: Uuid = sqlx::query_scalar("INSERT INTO customers (code,name) VALUES ('C2','On Time') RETURNING id").fetch_one(&pool).await.unwrap();
    let led = |c: Uuid, t: &'static str, delta: i32, days_ago: i64| {
        let pool = pool.clone();
        let date = f.today - Duration::days(days_ago);
        async move { sqlx::query("INSERT INTO jar_ledger (customer_id,product_id,txn_type,delta,txn_date) VALUES ($1,$2,$3,$4,$5)").bind(c).bind(p).bind(t).bind(delta).bind(date).execute(&pool).await.unwrap(); }
    };
    led(c1, "ISSUE_FILLED", 5, 45).await;
    led(c2, "ISSUE_FILLED", 5, 3).await;
    run_once(&f.st).await.unwrap();
    assert_eq!(open_alerts(&pool, "JAR_OVERDUE").await, vec![(format!("{c1}:{p}"), "WARNING".to_string())]);
    run_once(&f.st).await.unwrap();
    assert_eq!(all_alerts(&pool, "JAR_OVERDUE").await, 1, "no duplicate");
    // everything returned -> resolved
    led(c1, "RETURN_EMPTY", -5, 0).await;
    let s = run_once(&f.st).await.unwrap();
    assert_eq!(s.resolved, 1);
    assert!(open_alerts(&pool, "JAR_OVERDUE").await.is_empty());

    // fleet alert: 100 fills, 5 destroyed in 30 days (> 3 %)
    sqlx::query("INSERT INTO production_batches (batch_no,product_id,crates,qc_status,produced_on,expiry_date) VALUES ('BX',$1,100,'APPROVED',$2,current_date+100)").bind(p).bind(f.today - Duration::days(2)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO jar_fleet_events (product_id,event_type,qty,reason_code,location) VALUES ($1,'DESTROYED',5,'CRACKED','PLANT_EMPTY')").bind(p).execute(&pool).await.unwrap();
    // fleet of 40 containers in the plant: 100 fills / (40 + 5) = 2.2 cycles of max 10 => 22 % retirement
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Product, p, "JAR_CLEAN", NIL_LOT, Decimal::from(40), "ADJUST")]).await.unwrap();
    tx.commit().await.unwrap();
    run_once(&f.st).await.unwrap();
    assert_eq!(open_alerts(&pool, "JAR_FLEET_ALERT").await, vec![(format!("{p}:HIGH_DESTROY_RATE"), "WARNING".to_string())]);
    run_once(&f.st).await.unwrap();
    assert_eq!(all_alerts(&pool, "JAR_FLEET_ALERT").await, 1);
    // raising the limit clears it; a small max_cycles raises the retirement alert instead
    sqlx::query("UPDATE system_settings SET value='10' WHERE key='jars.destroy_warn_pct'").execute(&pool).await.unwrap();
    f.st.settings.reload(&pool).await.unwrap();
    sqlx::query("UPDATE products SET max_cycles=2 WHERE id=$1").bind(p).execute(&pool).await.unwrap();
    let s = run_once(&f.st).await.unwrap();
    assert_eq!((s.raised, s.resolved), (1, 1));
    assert_eq!(open_alerts(&pool, "JAR_FLEET_ALERT").await.iter().map(|a| a.0.clone()).collect::<Vec<_>>(), vec![format!("{p}:NEAR_RETIREMENT")]);
    // all clear
    sqlx::query("UPDATE products SET max_cycles=500 WHERE id=$1").bind(p).execute(&pool).await.unwrap();
    run_once(&f.st).await.unwrap();
    assert!(open_alerts(&pool, "JAR_FLEET_ALERT").await.is_empty());
}
