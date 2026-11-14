//! Over-production cap: finished goods cannot exceed what the production batch produced (API_REFERENCE 0.2-H).
//! `OVER_PRODUCED` is a hard 409 (not overridable by `confirm`, also returned by dry runs) for positive adjustments and count surpluses.
mod common;
mod materials_fixtures;
use materials_fixtures::*;
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType};

async fn approved_batch(pool: &PgPool, product: Uuid, crates: i32, on_hand: i32) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO production_batches (batch_no, product_id, crates, qc_status, expiry_date) VALUES ('AB'||substr(gen_random_uuid()::text,1,8), $1, $2, 'APPROVED', current_date + 150) RETURNING id")
        .bind(product).bind(crates).fetch_one(pool).await.unwrap();
    if on_hand > 0 { put_stock(pool, ItemType::Product, product, "FG_READY", id, &on_hand.to_string(), Some(future(150))).await; }
    id
}

async fn big_adjust_limit(c: &Ctx) {
    sqlx::query("UPDATE system_settings SET value='100000' WHERE key='entry.large_adjust_pct'").execute(&c.pool).await.unwrap();
    c.st.settings.reload(&c.pool).await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn positive_adjustment_cannot_exceed_what_the_batch_produced(pool: PgPool) {
    let c = setup(pool.clone()).await;
    big_adjust_limit(&c).await;
    let p = plant(&pool).await;
    let b = approved_batch(&pool, p.p500, 100, 100).await;
    let adj = |delta: i64| json!({ "item_type": "PRODUCT", "item_id": p.p500, "zone": "FG_READY", "lot_id": b, "delta": delta, "reason": "found stock", "confirm": true });
    // full batch on hand: +1 is more than was ever produced - confirm does not override, dry runs show the same refusal
    let (s, e) = c.post("/api/warehouse/adjustments", adj(1)).await;
    assert_eq!((s, code(&e)), (409, "OVER_PRODUCED"), "{e}");
    assert!(e["error"]["message"].as_str().unwrap().contains("only produced 100 crates"), "{e}");
    assert!(e["error"]["message"].as_str().unwrap().contains("100 are already on record"), "{e}");
    assert!(e["error"]["hint"].is_string());
    assert_eq!(e["error"]["details"]["produced"], 100);
    let (s, e) = c.post("/api/warehouse/adjustments?dry_run=1", adj(1)).await;
    assert_eq!((s, code(&e)), (409, "OVER_PRODUCED"), "{e}");
    // a loss followed by the matching "found" is a legitimate correction
    assert_eq!(c.post("/api/warehouse/adjustments", adj(-10)).await.0, 201);
    let (s, e) = c.post("/api/warehouse/adjustments", adj(11)).await;
    assert_eq!((s, code(&e)), (409, "OVER_PRODUCED"), "{e}");
    assert_eq!(c.post("/api/warehouse/adjustments", adj(10)).await.0, 201);
    assert_eq!(lot_qty(&pool, "PRODUCT", p.p500, "FG_READY", b).await, d("100"));
    // stock of the same lot in another godown and in TRANSIT counts towards the total
    let g2 = godown(&pool, "GDN-CAP", false).await;
    assert_eq!(c.post("/api/warehouse/adjustments", adj(-30)).await.0, 201);
    put_stock_at(&pool, g2, ItemType::Product, p.p500, "FG_READY", b, "20", Some(future(150))).await;
    put_stock_at(&pool, stock::TRANSIT_LOCATION, ItemType::Product, p.p500, "FG_READY", b, "10", Some(future(150))).await;
    // 70 + 20 + 10 = 100 on record: nothing more can be added anywhere
    let (s, e) = c.post("/api/warehouse/adjustments", adj(1)).await;
    assert_eq!((s, code(&e)), (409, "OVER_PRODUCED"), "{e}");
    let mut other = adj(1);
    other["location_id"] = json!(g2);
    let (s, e) = c.post("/api/warehouse/adjustments", other).await;
    assert_eq!((s, code(&e)), (409, "OVER_PRODUCED"), "{e}");
    assert_eq!(dv(&e["error"]["details"]["on_record"]), d("100"), "{e}");
    // negative adjustments are not affected, and the freed room can be used again
    assert_eq!(c.post("/api/warehouse/adjustments", adj(-1)).await.0, 201);
    assert_eq!(c.post("/api/warehouse/adjustments", adj(1)).await.0, 201);
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn count_surplus_cannot_exceed_what_the_batch_produced(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let p = plant(&pool).await;
    let b = approved_batch(&pool, p.p500, 100, 100).await;
    let (s, cnt) = c.post("/api/warehouse/counts", json!({ "zone": "FG_READY" })).await;
    assert_eq!(s, 201, "{cnt}");
    let cid = cnt["id"].as_str().unwrap().to_string();
    let lid = cnt["lines"][0]["line_id"].clone();
    let (s, _) = c.put(&format!("/api/warehouse/counts/{cid}/lines"), json!({ "lines": [{ "line_id": lid, "counted_qty": 105, "note": "pallet found in the yard" }] })).await;
    assert_eq!(s, 200);
    let (s, e) = c.post(&format!("/api/warehouse/counts/{cid}/post"), json!({ "confirm": true })).await;
    assert_eq!((s, code(&e)), (409, "OVER_PRODUCED"), "{e}");
    let (s, e) = c.post(&format!("/api/warehouse/counts/{cid}/post?dry_run=1"), json!({ "confirm": true })).await;
    assert_eq!((s, code(&e)), (409, "OVER_PRODUCED"), "{e}");
    // nothing was posted and the count is still open
    assert_eq!(lot_qty(&pool, "PRODUCT", p.p500, "FG_READY", b).await, d("100"));
    let st: String = sqlx::query_scalar("SELECT status FROM stock_counts WHERE id=$1::uuid").bind(&cid).fetch_one(&pool).await.unwrap();
    assert_eq!(st, "DRAFT");
    // a count within what was produced still posts
    let (s, _) = c.put(&format!("/api/warehouse/counts/{cid}/lines"), json!({ "lines": [{ "line_id": lid, "counted_qty": 97, "note": "3 crates missing" }] })).await;
    assert_eq!(s, 200);
    let (s, e) = c.post(&format!("/api/warehouse/counts/{cid}/post"), json!({ "confirm": true })).await;
    assert_eq!(s, 200, "{e}");
    assert_eq!(lot_qty(&pool, "PRODUCT", p.p500, "FG_READY", b).await, d("97"));
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_positive_adjustments_never_exceed_the_produced_quantity(pool: PgPool) {
    let c = setup(pool.clone()).await;
    big_adjust_limit(&c).await;
    let p = plant(&pool).await;
    // 100 produced, 80 on hand (20 are on the road): room for exactly 4 x +5
    let b = approved_batch(&pool, p.p500, 100, 80).await;
    let mut hs = vec![];
    for _ in 0..12 {
        let (app, key, prod) = (c.app.clone(), c.key.clone(), p.p500);
        hs.push(tokio::spawn(async move {
            call(&app, "POST", "/api/warehouse/adjustments", Some(&key),
                Some(json!({ "item_type": "PRODUCT", "item_id": prod, "zone": "FG_READY", "lot_id": b, "delta": 5, "reason": "parallel found", "confirm": true }))).await
        }));
    }
    let mut ok = 0;
    for h in hs {
        let (s, v) = h.await.unwrap();
        match s { 201 => ok += 1, 409 => assert!(["OVER_PRODUCED", "RETRY"].contains(&code(&v)), "{v}"), x => panic!("{x} {v}") }
    }
    let have = lot_qty(&pool, "PRODUCT", p.p500, "FG_READY", b).await;
    assert!(have <= d("100"), "{have} crates of a 100-crate batch");
    assert_eq!(have, d("80") + d("5") * Decimal::from(ok));
    assert!(ok >= 1, "at least one of the writers must have succeeded");
    check_invariants(&pool).await;
}
