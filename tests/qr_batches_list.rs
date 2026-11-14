//! `GET /api/qr/batches` - batches eligible for QR labels, for label users without the `production` page.
mod loading_support;
use axum::http::StatusCode;
use loading_support::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test(migrations = "./migrations")]
async fn lists_label_eligible_batches_scoped_searchable_and_paged(pool: PgPool) {
    let c = ctx(pool).await;
    c.set("features.qr_labels", json!(true)).await;
    let p = c.product("BOTTLED", 12, Some(10)).await;
    let quarantined = c.batch(p, 100, "QUARANTINE", 90).await;
    let approved = c.batch(p, 40, "APPROVED", 120).await;
    let rejected = c.batch(p, 10, "REJECTED", 90).await;
    // 30 of the quarantined batch's crates already have labels
    let (s, v) = c.post(&format!("/api/qr/batches/{quarantined}/sets"), json!({ "mode": "CUSTOM", "sizes": [20, 10] })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");

    let (s, v) = c.get("/api/qr/batches").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 2, "{v}");
    let items = v["items"].as_array().unwrap();
    assert!(items.iter().all(|i| i["id"] != json!(rejected)), "REJECTED batches are never offered: {v}");
    let q = items.iter().find(|i| i["id"] == json!(quarantined)).unwrap();
    assert_eq!(q["crates"], 100);
    assert_eq!(q["allocated_crates"], 30);
    assert_eq!(q["free_crates"], 70);
    assert_eq!(q["qc_status"], "QUARANTINE");
    assert_eq!(q["product_name"], "BOTTLED prod");
    assert_eq!(q["location_id"], "00000000-0000-0000-0000-000000000001");
    assert!(q["location_name"].is_string() && q["batch_no"].is_string() && q["expiry_date"].is_string());
    let a = items.iter().find(|i| i["id"] == json!(approved)).unwrap();
    assert_eq!((a["allocated_crates"].as_i64(), a["free_crates"].as_i64()), (Some(0), Some(40)));

    // paging
    let (_, v) = c.get("/api/qr/batches?limit=1").await;
    assert_eq!((v["items"].as_array().unwrap().len(), v["total"].as_i64()), (1, Some(2)));
    let (_, v2) = c.get("/api/qr/batches?limit=1&offset=1").await;
    assert_ne!(v["items"][0]["id"], v2["items"][0]["id"]);

    // search: bound ILIKE on batch_no / product; wildcards are literal
    let bn: String = sqlx::query_scalar("SELECT batch_no FROM production_batches WHERE id=$1").bind(approved).fetch_one(&c.pool).await.unwrap();
    let (_, v) = c.get(&format!("/api/qr/batches?q={}", bn.to_lowercase())).await;
    assert_eq!(v["total"], 1, "{v}");
    assert_eq!(v["items"][0]["id"], json!(approved));
    let (_, v) = c.get("/api/qr/batches?q=bottled%20PROD").await;
    assert_eq!(v["total"], 2);
    for hostile in ["%25", "_", "'%3B%20DROP%20TABLE%20production_batches%3B--"] {
        let (s, v) = c.get(&format!("/api/qr/batches?q={hostile}")).await;
        assert_eq!((s, v["total"].as_i64()), (StatusCode::OK, Some(0)), "{hostile}: {v}");
    }
    let (s, _) = c.get(&format!("/api/qr/batches?q={}", "x".repeat(101))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // a user without the label / loading pages is refused
    let (s, _) = common::call(&c.app, "GET", "/api/qr/batches", Some(&c.nobody), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "./migrations")]
async fn batch_list_is_scoped_to_the_callers_godowns_and_needs_no_production_page(pool: PgPool) {
    let c = ctx(pool).await;
    c.set("features.qr_labels", json!(true)).await;
    let p = c.product("BOTTLED", 12, None).await;
    let main = c.batch(p, 10, "APPROVED", 90).await;
    let g2 = c.godown("GDN-Q2").await;
    let other: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO production_batches (batch_no,product_id,crates,qc_status,expiry_date,location_id) VALUES ('G2-1',$1,25,'QUARANTINE',current_date+90,$2) RETURNING id")
        .bind(p).bind(g2).fetch_one(&c.pool).await.unwrap();
    // a loader limited to godown 2 only sees that godown's batch (and has no `production` page)
    let u = common::make_user(&c.pool, "q2", false, &["qr-labels"], &[]).await;
    sqlx::query("UPDATE users SET allowed_location_ids = $2 WHERE id = $1").bind(u).bind(json!([g2])).execute(&c.pool).await.unwrap();
    let k = common::session_for(&c.pool, u).await;
    let (s, v) = common::call(&c.app, "GET", "/api/qr/batches", Some(&k), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 1, "{v}");
    assert_eq!(v["items"][0]["id"], json!(other));
    assert_eq!(v["items"][0]["location_id"], json!(g2));
    assert!(!v.to_string().contains(&main.to_string()), "foreign godown batch leaked: {v}");
    let (s, _) = common::call(&c.app, "GET", "/api/production/batches", Some(&k), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "premise: no production page");
    // unrestricted loader sees both
    let (_, v) = c.get("/api/qr/batches").await;
    assert_eq!(v["total"], 2);
}
