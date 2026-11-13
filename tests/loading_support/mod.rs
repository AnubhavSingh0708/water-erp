//! Fixtures shared by the loading_* / qr_* integration tests (SQL fixtures + `water_erp::stock`, no dependency on other modules).
#![allow(dead_code)]
use axum::{http::StatusCode, Router};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType, Move};

#[path = "../common/mod.rs"]
pub mod common;

pub struct Ctx {
    pub pool: PgPool,
    pub app: Router,
    pub st: water_erp::state::AppState,
    /// user with the `loading` + `qr-labels` pages
    pub key: String,
    pub admin: String,
    /// user without any loading access
    pub nobody: String,
    pub uid: Uuid,
}

pub async fn ctx(pool: PgPool) -> Ctx {
    let (app, st) = common::app(pool.clone()).await;
    let uid = common::make_user(&pool, "loader", false, &["loading", "qr-labels", "dashboard"], &[]).await;
    let key = common::session_for(&pool, uid).await;
    let a = common::make_user(&pool, "boss", true, &[], &[]).await;
    let admin = common::session_for(&pool, a).await;
    let n = common::make_user(&pool, "nobody", false, &["dashboard", "qc"], &[]).await;
    let nobody = common::session_for(&pool, n).await;
    Ctx { pool, app, st, key, admin, nobody, uid }
}

impl Ctx {
    pub async fn get(&self, uri: &str) -> (StatusCode, Value) { common::call(&self.app, "GET", uri, Some(&self.key), None).await }
    pub async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) { common::call(&self.app, "POST", uri, Some(&self.key), Some(body)).await }
    pub async fn post_as(&self, key: &str, uri: &str, body: Value) -> (StatusCode, Value) { common::call(&self.app, "POST", uri, Some(key), Some(body)).await }
    pub async fn delete(&self, uri: &str) -> (StatusCode, Value) { common::call(&self.app, "DELETE", uri, Some(&self.key), None).await }
    pub async fn set(&self, key: &str, value: Value) {
        sqlx::query("UPDATE system_settings SET value=$2 WHERE key=$1").bind(key).bind(value).execute(&self.pool).await.unwrap();
        self.st.settings.reload(&self.pool).await.unwrap();
    }
    pub async fn product(&self, kind: &str, units: i32, cpp: Option<i32>) -> Uuid {
        sqlx::query_scalar("INSERT INTO products (code,name,kind,units_per_crate,crates_per_pallet) VALUES ('P'||floor(random()*1e9)::text,$1,$2,$3,$4) RETURNING id")
            .bind(format!("{kind} prod")).bind(kind).bind(units).bind(cpp).fetch_one(&self.pool).await.unwrap()
    }
    /// batch with `qty` crates on hand in the zone that matches its status
    pub async fn batch(&self, product: Uuid, qty: i64, qc: &str, expiry_days: i32) -> Uuid {
        let b: Uuid = sqlx::query_scalar(
            "INSERT INTO production_batches (batch_no,product_id,crates,qc_status,expiry_date) VALUES ('B'||floor(random()*1e9)::text,$1,$2,$3,current_date+$4) RETURNING id")
            .bind(product).bind(qty as i32).bind(qc).bind(expiry_days).fetch_one(&self.pool).await.unwrap();
        let zone = match qc { "APPROVED" => "FG_READY", "QUARANTINE" => "QC_QUARANTINE", _ => "SCRAP_YARD" };
        let exp = chrono::Utc::now().date_naive() + chrono::Duration::days(expiry_days as i64);
        if qty > 0 {
            let mut tx = self.pool.begin().await.unwrap();
            stock::post_all(&mut tx, vec![Move::new(ItemType::Product, product, zone, b, Decimal::from(qty), "BATCH_OUTPUT").expiry(Some(exp))]).await.unwrap();
            tx.commit().await.unwrap();
        }
        b
    }
    /// put `qty` crates of an APPROVED batch into a godown's FG_READY (the batch itself was produced at MAIN)
    pub async fn stock_at(&self, loc: Uuid, product: Uuid, batch: Uuid, qty: i64) {
        let mut tx = self.pool.begin().await.unwrap();
        stock::post_all(&mut tx, vec![Move::new(ItemType::Product, product, "FG_READY", batch, Decimal::from(qty), "ADJUST").at(loc)
            .expiry(Some(chrono::Utc::now().date_naive() + chrono::Duration::days(60)))]).await.unwrap();
        tx.commit().await.unwrap();
    }
    pub async fn godown(&self, code: &str) -> Uuid {
        sqlx::query_scalar("INSERT INTO locations (code,name,kind) VALUES ($1,$1,'GODOWN') RETURNING id").bind(code).fetch_one(&self.pool).await.unwrap()
    }
    pub async fn qty_at(&self, loc: Uuid, product: Uuid, zone: &str, lot: Uuid) -> Decimal {
        sqlx::query_scalar("SELECT COALESCE(SUM(qty),0) FROM stock_balances WHERE location_id=$1 AND item_type='PRODUCT' AND item_id=$2 AND zone=$3 AND lot_id=$4")
            .bind(loc).bind(product).bind(zone).bind(lot).fetch_one(&self.pool).await.unwrap()
    }
    pub async fn vehicle(&self, cap: i32) -> Uuid {
        sqlx::query_scalar("INSERT INTO vehicles (reg_no,capacity_crates,driver_name) VALUES ('V'||floor(random()*1e9)::text,$1,'Ravi') RETURNING id")
            .bind(cap).fetch_one(&self.pool).await.unwrap()
    }
    pub async fn customer(&self, name: &str) -> Uuid {
        sqlx::query_scalar("INSERT INTO customers (code,name) VALUES ('C'||floor(random()*1e9)::text,$1) RETURNING id").bind(name).fetch_one(&self.pool).await.unwrap()
    }
    pub async fn trip(&self, vehicle: Uuid) -> Uuid {
        let (s, v) = self.post("/api/loading/trips", json!({ "vehicle_id": vehicle })).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().parse().unwrap()
    }
    pub async fn qty(&self, product: Uuid, zone: &str, lot: Uuid) -> Decimal {
        sqlx::query_scalar("SELECT COALESCE(SUM(qty),0) FROM stock_balances WHERE item_type='PRODUCT' AND item_id=$1 AND zone=$2 AND lot_id=$3")
            .bind(product).bind(zone).bind(lot).fetch_one(&self.pool).await.unwrap()
    }
    pub async fn add(&self, trip: Uuid, body: Value) -> (StatusCode, Value) { self.post(&format!("/api/loading/trips/{trip}/items"), body).await }
    pub async fn gate(&self, trip: Uuid) -> (StatusCode, Value) { self.post(&format!("/api/loading/trips/{trip}/gate-pass"), json!({})).await }
    /// ledger invariant: every balance equals the sum of its ledger deltas
    pub async fn assert_ledger_consistent(&self) {
        let bad: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM stock_balances sb WHERE sb.qty <> COALESCE((SELECT SUM(qty_delta) FROM stock_ledger l
               WHERE l.location_id=sb.location_id AND l.item_type=sb.item_type AND l.item_id=sb.item_id AND l.zone=sb.zone AND l.lot_id=sb.lot_id),0)")
            .fetch_one(&self.pool).await.unwrap();
        assert_eq!(bad, 0, "stock_balances diverged from the ledger");
    }
}

pub fn uuid(v: &Value) -> Uuid { v.as_str().unwrap().parse().unwrap() }
pub fn code(v: &Value) -> &str { v["error"]["code"].as_str().unwrap_or("") }
