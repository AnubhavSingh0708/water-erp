//! `GET /api/production/preform-stock` - RM_STORE quantity of one PREFORM material at the resolved godown, for operators who
//! have the `production` page but not `raw-materials`.
mod common;
mod materials_fixtures;
use materials_fixtures::*;
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::ItemType;

#[sqlx::test(migrations = "./migrations")]
async fn preform_stock_is_visible_to_operators_without_the_raw_materials_page(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    put_stock(&pool, ItemType::Raw, p.preform, "RM_STORE", Uuid::new_v4(), "1200", Some(future(200))).await;
    put_stock(&pool, ItemType::Raw, p.preform, "RM_STORE", Uuid::new_v4(), "300", Some(future(100))).await;
    put_stock(&pool, ItemType::Raw, p.preform, "SCRAP_YARD", Uuid::nil(), "50", None).await; // not usable stock
    let operator = c.user("operator", &["production"]).await;
    let uri = format!("/api/production/preform-stock?preform_material_id={}", p.preform);
    let (s, v) = call(&c.app, "GET", &uri, Some(&operator), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(uid(&v["material_id"]), p.preform);
    assert_eq!(dv(&v["available"]), d("1500"));
    assert_eq!(dv(&v["min_stock"]), d("500"));
    assert_eq!(v["unit"], "pcs");
    assert_eq!(v["location_id"], "00000000-0000-0000-0000-000000000001");
    assert!(v["name"].is_string() && v["location_name"].is_string());
    // the same operator has no business on the raw-materials page
    assert_eq!(call(&c.app, "GET", "/api/raw-materials", Some(&operator), None).await.0, 403);
    // without the production page: 403
    let other = c.user("other", &["raw-materials"]).await;
    assert_eq!(call(&c.app, "GET", &uri, Some(&other), None).await.0, 403);
    // missing / unknown / wrong category / inactive
    assert_eq!(call(&c.app, "GET", "/api/production/preform-stock", Some(&operator), None).await.0, 422);
    for bad in [Uuid::new_v4(), p.bottle, p.cap] {
        let (s, v) = call(&c.app, "GET", &format!("/api/production/preform-stock?preform_material_id={bad}"), Some(&operator), None).await;
        assert_eq!(s, 422, "{v}");
    }
    sqlx::query("UPDATE raw_materials SET is_active=false WHERE id=$1").bind(p.preform).execute(&pool).await.unwrap();
    assert_eq!(call(&c.app, "GET", &uri, Some(&operator), None).await.0, 422);
    assert_eq!(call(&c.app, "GET", "/api/production/preform-stock?preform_material_id=nope", Some(&operator), None).await.0, 400);
}

#[sqlx::test(migrations = "./migrations")]
async fn preform_stock_is_per_godown_and_respects_godown_permissions(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    let site2 = godown(&pool, "PLANT-2", true).await;
    let store = godown(&pool, "STORE-X", false).await;
    put_stock(&pool, ItemType::Raw, p.preform, "RM_STORE", Uuid::new_v4(), "100", None).await;
    put_stock_at(&pool, site2, ItemType::Raw, p.preform, "RM_STORE", Uuid::new_v4(), "777", None).await;
    let restricted_key = restricted(&c, "op2", &["production"], &[site2]).await;
    // omitted location_id => the restricted user's only godown
    let (s, v) = call(&c.app, "GET", &format!("/api/production/preform-stock?preform_material_id={}", p.preform), Some(&restricted_key), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!((dv(&v["available"]), uid(&v["location_id"])), (d("777"), site2));
    // explicit foreign godown: 403, never the number
    let (s, v) = call(&c.app, "GET", &format!("/api/production/preform-stock?preform_material_id={}&location_id={}", p.preform, water_erp::stock::MAIN_LOCATION), Some(&restricted_key), None).await;
    assert_eq!(s, 403, "{v}");
    assert!(!v.to_string().contains("100"), "{v}");
    // admin picks a godown explicitly; MAIN by default
    let (_, v) = c.get(&format!("/api/production/preform-stock?preform_material_id={}", p.preform)).await;
    assert_eq!(dv(&v["available"]), d("100"));
    let (_, v) = c.get(&format!("/api/production/preform-stock?preform_material_id={}&location_id={site2}", p.preform)).await;
    assert_eq!(dv(&v["available"]), d("777"));
    // a godown that is not a production site is refused (blow runs cannot happen there)
    assert_eq!(c.get(&format!("/api/production/preform-stock?preform_material_id={}&location_id={store}", p.preform)).await.0, 422);
    // never stock in a godown that has none: zero, not an error
    let site3 = godown(&pool, "PLANT-3", true).await;
    let (s, v) = c.get(&format!("/api/production/preform-stock?preform_material_id={}&location_id={site3}", p.preform)).await;
    assert_eq!((s, dv(&v["available"])), (200, d("0")));
}
