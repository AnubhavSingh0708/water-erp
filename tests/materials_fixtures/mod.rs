//! Shared fixtures for Owner-B integration tests (materials_*, production_*, qc_*, warehouse_*).
#![allow(dead_code)]
use axum::{body::Body, http::{HeaderMap, Request}, Router};
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::PgPool;
use std::str::FromStr;
use tower::ServiceExt;
use uuid::Uuid;
use water_erp::{state::AppState, stock::{self, ItemType, Move}};

pub struct Ctx { pub app: Router, pub st: AppState, pub pool: PgPool, pub key: String, pub admin: Uuid }

pub async fn setup(pool: PgPool) -> Ctx {
    let (app, st) = super::common::app(pool.clone()).await;
    let admin = super::common::make_user(&pool, "admin", true, &[], &[]).await;
    let key = super::common::session_for(&pool, admin).await;
    Ctx { app, st, pool, key, admin }
}

impl Ctx {
    pub async fn user(&self, name: &str, pages: &[&str]) -> String {
        let u = super::common::make_user(&self.pool, name, false, pages, &[]).await;
        super::common::session_for(&self.pool, u).await
    }
    pub async fn get(&self, uri: &str) -> (u16, Value) { call(&self.app, "GET", uri, Some(&self.key), None).await }
    pub async fn post(&self, uri: &str, body: Value) -> (u16, Value) { call(&self.app, "POST", uri, Some(&self.key), Some(body)).await }
    pub async fn put(&self, uri: &str, body: Value) -> (u16, Value) { call(&self.app, "PUT", uri, Some(&self.key), Some(body)).await }
}

pub async fn send(app: &Router, method: &str, uri: &str, key: Option<&str>, body: Option<Value>, headers: &[(&str, &str)]) -> (u16, HeaderMap, String) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(k) = key { b = b.header("x-auth-key", k); }
    for (k, v) in headers { b = b.header(*k, *v); }
    let req = match body { Some(j) => b.header("content-type", "application/json").body(Body::from(j.to_string())).unwrap(), None => b.body(Body::empty()).unwrap() };
    let resp = app.clone().oneshot(req).await.unwrap();
    let st = resp.status();
    let h = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (st.as_u16(), h, String::from_utf8_lossy(&bytes).into_owned())
}

pub async fn call(app: &Router, method: &str, uri: &str, key: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let (s, _, t) = send(app, method, uri, key, body, &[]).await;
    (s, serde_json::from_str(&t).unwrap_or(Value::String(t)))
}

pub fn d(s: &str) -> Decimal { Decimal::from_str(s).unwrap() }
pub fn dv(v: &Value) -> Decimal { Decimal::from_str(v.as_str().unwrap_or_else(|| panic!("not a string: {v}"))).unwrap() }
pub fn uid(v: &Value) -> Uuid { Uuid::parse_str(v.as_str().unwrap_or_else(|| panic!("not a uuid: {v}"))).unwrap() }
pub fn code(v: &Value) -> &str { v["error"]["code"].as_str().unwrap_or("") }

pub async fn supplier(pool: &PgPool, name: &str, lead: i32) -> Uuid {
    sqlx::query_scalar("INSERT INTO suppliers (name, lead_time_days) VALUES ($1,$2) RETURNING id").bind(name).bind(lead).fetch_one(pool).await.unwrap()
}

/// Raw material: purchase unit = `spp` stock units.
pub async fn material(pool: &PgPool, code: &str, category: &str, spp: &str, min: &str, supplier: Option<Uuid>) -> Uuid {
    sqlx::query_scalar("INSERT INTO raw_materials (code, name, category, stock_unit, purchase_unit, stock_per_purchase_unit, min_stock, default_supplier_id, unit_weight_g)
                        VALUES ($1,$1,$2,'pcs','bag',$3::numeric,$4::numeric,$5, 20) RETURNING id")
        .bind(code).bind(category).bind(spp).bind(min).bind(supplier).fetch_one(pool).await.unwrap()
}

pub async fn product(pool: &PgPool, code: &str, kind: &str, size_ml: Option<i32>, upc: i32, cpp: Option<i32>) -> Uuid {
    sqlx::query_scalar("INSERT INTO products (code, name, kind, size_ml, units_per_crate, crates_per_pallet, shelf_life_days) VALUES ($1,$1,$2,$3,$4,$5,180) RETURNING id")
        .bind(code).bind(kind).bind(size_ml).bind(upc).bind(cpp).fetch_one(pool).await.unwrap()
}

pub async fn bom(pool: &PgPool, product: Uuid, material: Uuid, qty: &str, zone: &str, basis: &str) {
    sqlx::query("INSERT INTO bom_items (product_id, raw_material_id, qty_per_crate, source_zone, basis) VALUES ($1,$2,$3::numeric,$4,$5)")
        .bind(product).bind(material).bind(qty).bind(zone).bind(basis).execute(pool).await.unwrap();
}

pub async fn put_stock(pool: &PgPool, it: ItemType, item: Uuid, zone: &'static str, lot: Uuid, qty: &str, expiry: Option<chrono::NaiveDate>) {
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(it, item, zone, lot, d(qty), "GRN").expiry(expiry)]).await.unwrap();
    tx.commit().await.unwrap();
}

pub async fn qty(pool: &PgPool, it: &str, item: Uuid, zone: &str) -> Decimal {
    sqlx::query_scalar("SELECT COALESCE(SUM(qty),0) FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone=$3").bind(it).bind(item).bind(zone).fetch_one(pool).await.unwrap()
}

pub async fn lot_qty(pool: &PgPool, it: &str, item: Uuid, zone: &str, lot: Uuid) -> Decimal {
    sqlx::query_scalar("SELECT COALESCE(SUM(qty),0) FROM stock_balances WHERE item_type=$1 AND item_id=$2 AND zone=$3 AND lot_id=$4").bind(it).bind(item).bind(zone).bind(lot).fetch_one(pool).await.unwrap()
}

/// stock_balances.qty == SUM(ledger deltas) per (item, zone, lot) and nothing negative.
pub async fn check_invariants(pool: &PgPool) {
    let bad: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stock_balances b WHERE b.qty <> COALESCE((SELECT SUM(l.qty_delta) FROM stock_ledger l
           WHERE l.item_type=b.item_type AND l.item_id=b.item_id AND l.zone=b.zone AND l.lot_id=b.lot_id), 0) OR b.qty < 0").fetch_one(pool).await.unwrap();
    assert_eq!(bad, 0, "stock_balances disagree with the ledger");
}

pub fn future(days: i64) -> chrono::NaiveDate { chrono::Utc::now().date_naive() + chrono::Duration::days(days) }

/// A simple bottled product with a full BOM and stock. Returns (product, [preform, bottle, cap, mineral]).
pub struct Plant { pub sup: Uuid, pub preform: Uuid, pub bottle: Uuid, pub cap: Uuid, pub mineral: Uuid, pub p500: Uuid }

pub async fn plant(pool: &PgPool) -> Plant {
    let sup = supplier(pool, "Acme Plastics", 5).await;
    let preform = material(pool, "PF-500", "PREFORM", "1000", "500", Some(sup)).await;
    let bottle = material(pool, "EB-500", "EMPTY_BOTTLE", "1", "0", None).await;
    let cap = material(pool, "CAP-1", "CAP", "1000", "100", Some(sup)).await;
    let mineral = material(pool, "MIN-1", "MINERAL", "25", "5", Some(sup)).await;
    let p500 = product(pool, "W500", "BOTTLED", Some(500), 24, Some(60)).await;
    bom(pool, p500, bottle, "24", "BLOW_FLOOR", "PER_CRATE").await;
    bom(pool, p500, cap, "24", "RM_STORE", "PER_CRATE").await;
    bom(pool, p500, mineral, "2.5", "RM_STORE", "PER_1000L").await;
    Plant { sup, preform, bottle, cap, mineral, p500 }
}

pub async fn stock_plant(pool: &PgPool, p: &Plant) {
    put_stock(pool, ItemType::Raw, p.bottle, "BLOW_FLOOR", Uuid::nil(), "100000", None).await;
    put_stock(pool, ItemType::Raw, p.cap, "RM_STORE", Uuid::new_v4(), "100000", Some(future(300))).await;
    put_stock(pool, ItemType::Raw, p.mineral, "RM_STORE", Uuid::new_v4(), "1000", Some(future(300))).await;
}

pub async fn fresh_water_log(pool: &PgPool) {
    sqlx::query("INSERT INTO water_treatment_logs (tds_ppm, ph, uv_ok, out_of_range) VALUES (150, 7.2, true, false)").execute(pool).await.unwrap();
}
