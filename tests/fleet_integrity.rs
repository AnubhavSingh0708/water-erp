//! Fleet integrity: stock adjustments / count posts that remove containers from a jar zone write matching `jar_fleet_events`
//! in the same transaction (DESTROYED / OTHER); surpluses write nothing (a surplus is not a purchase); `/api/jars/fleet` total is
//! derived from stock balances + customer holdings, so it always agrees with stock.
mod common;
mod materials_fixtures;
use materials_fixtures::*;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::ItemType;

async fn events(pool: &PgPool, product: Uuid) -> Vec<(String, i32, Option<String>, Option<String>, String)> {
    sqlx::query_as("SELECT event_type, qty, reason_code, location, note FROM jar_fleet_events WHERE product_id=$1 ORDER BY created_at, id").bind(product).fetch_all(pool).await.unwrap()
}

async fn jar_product(pool: &PgPool) -> Uuid { product(pool, "J20", "RETURNABLE", Some(20000), 1, None).await }

#[sqlx::test(migrations = "./migrations")]
async fn adjustments_in_jar_zones_write_fleet_events(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let j = jar_product(&pool).await;
    put_stock(&pool, ItemType::Product, j, "JAR_EMPTY", Uuid::nil(), "50", None).await;
    put_stock(&pool, ItemType::Product, j, "JAR_CLEAN", Uuid::nil(), "40", None).await;
    let cust: Uuid = sqlx::query_scalar("INSERT INTO customers (code,name) VALUES ('C1','Holder') RETURNING id").fetch_one(&pool).await.unwrap();
    sqlx::query("INSERT INTO jar_ledger (customer_id, product_id, txn_type, delta, txn_date) VALUES ($1,$2,'OPENING',7,current_date)").bind(cust).bind(j).execute(&pool).await.unwrap();
    let adj = |zone: &str, delta: i64| json!({ "item_type": "PRODUCT", "item_id": j, "zone": zone, "delta": delta, "reason": "jars found cracked", "confirm": true });

    // dry run and failed attempts leave nothing behind
    let (s, _) = c.post("/api/warehouse/adjustments?dry_run=1", adj("JAR_EMPTY", -3)).await;
    assert_eq!(s, 200);
    let (s, e) = c.post("/api/warehouse/adjustments", adj("JAR_EMPTY", -500)).await;
    assert_eq!((s, code(&e)), (409, "INSUFFICIENT_STOCK"));
    assert!(events(&pool, j).await.is_empty());

    // negative: DESTROYED / OTHER at the matching plant location, same quantity
    assert_eq!(c.post("/api/warehouse/adjustments", adj("JAR_EMPTY", -3)).await.0, 201);
    assert_eq!(c.post("/api/warehouse/adjustments", adj("JAR_CLEAN", -2)).await.0, 201);
    // positive: stock rises, no event is invented (not a purchase)
    assert_eq!(c.post("/api/warehouse/adjustments", adj("JAR_CLEAN", 4)).await.0, 201);
    let ev = events(&pool, j).await;
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0], ("DESTROYED".into(), 3, Some("OTHER".into()), Some("PLANT_EMPTY".into()), "adjustment".into()));
    assert_eq!(ev[1], ("DESTROYED".into(), 2, Some("OTHER".into()), Some("PLANT_CLEAN".into()), "adjustment".into()));
    let loc: Uuid = sqlx::query_scalar("SELECT location_id FROM jar_fleet_events WHERE product_id=$1 LIMIT 1").bind(j).fetch_one(&pool).await.unwrap();
    assert_eq!(loc, water_erp::stock::MAIN_LOCATION);

    // the fleet total is derived from stock + customer holdings (47 + 42 + 7), and the period shows the destroyed containers
    let (s, f) = c.get(&format!("/api/jars/fleet?product_id={j}")).await;
    assert_eq!(s, 200, "{f}");
    assert_eq!(f["fleet"]["plant_empty"], 47);
    assert_eq!(f["fleet"]["plant_clean"], 42);
    assert_eq!(f["fleet"]["with_customers"], 7);
    assert_eq!(f["fleet"]["total"], 96);
    assert_eq!(f["period"]["destroyed_total"], 5);
    assert_eq!(f["period"]["destroyed_by_reason"]["OTHER"], 5);
    assert_eq!(f["period"]["new_purchased"], 0);

    // a non-returnable item never writes fleet events; finished-goods zone adjustments are not jar zones
    let raw = material(&pool, "CAP-9", "CAP", "1", "0", None).await;
    put_stock(&pool, ItemType::Raw, raw, "RM_STORE", Uuid::nil(), "10", None).await;
    assert_eq!(c.post("/api/warehouse/adjustments", json!({ "item_type": "RAW", "item_id": raw, "zone": "RM_STORE", "delta": -2, "reason": "spillage found", "confirm": true })).await.0, 201);
    assert_eq!(events(&pool, j).await.len(), 2);
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn count_post_in_jar_zones_writes_fleet_events_for_losses_only(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let j = jar_product(&pool).await;
    let j2 = product(&pool, "J10", "RETURNABLE", Some(10000), 1, None).await;
    put_stock(&pool, ItemType::Product, j, "JAR_CLEAN", Uuid::nil(), "40", None).await;
    put_stock(&pool, ItemType::Product, j2, "JAR_CLEAN", Uuid::nil(), "10", None).await;
    let (s, cnt) = c.post("/api/warehouse/counts", json!({ "zone": "JAR_CLEAN" })).await;
    assert_eq!(s, 201, "{cnt}");
    let cid = cnt["id"].as_str().unwrap().to_string();
    let cnt_no = cnt["cnt_no"].as_str().unwrap().to_string();
    let line_of = |item: Uuid| cnt["lines"].as_array().unwrap().iter().find(|l| l["item_id"] == json!(item)).unwrap()["line_id"].clone();
    // J: 40 expected, 35 counted (loss of 5); J10: 10 expected, 12 counted (surplus, no event)
    let (s, _) = c.put(&format!("/api/warehouse/counts/{cid}/lines"), json!({ "lines": [
        { "line_id": line_of(j), "counted_qty": 35, "note": "five jars crushed by forklift" },
        { "line_id": line_of(j2), "counted_qty": 12, "note": "two found behind rack" }] })).await;
    assert_eq!(s, 200);
    // dry run: no events
    let (s, v) = c.post(&format!("/api/warehouse/counts/{cid}/post?dry_run=1"), json!({})).await;
    assert_eq!(s, 200, "{v}");
    assert!(events(&pool, j).await.is_empty());
    // unconfirmed: nothing
    let (s, e) = c.post(&format!("/api/warehouse/counts/{cid}/post"), json!({})).await;
    assert_eq!((s, code(&e)), (409, "CONFIRM_REQUIRED"));
    assert!(events(&pool, j).await.is_empty());
    let (s, v) = c.post(&format!("/api/warehouse/counts/{cid}/post"), json!({ "confirm": true })).await;
    assert_eq!(s, 200, "{v}");
    let ev = events(&pool, j).await;
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0], ("DESTROYED".into(), 5, Some("OTHER".into()), Some("PLANT_CLEAN".into()), format!("stock count {cnt_no}")));
    assert!(events(&pool, j2).await.is_empty(), "a surplus is not a purchase");
    assert_eq!(qty(&pool, "PRODUCT", j, "JAR_CLEAN").await, d("35"));
    assert_eq!(qty(&pool, "PRODUCT", j2, "JAR_CLEAN").await, d("12"));
    // posting twice is refused and writes no second event
    let (s, _) = c.post(&format!("/api/warehouse/counts/{cid}/post"), json!({ "confirm": true })).await;
    assert_eq!(s, 409);
    assert_eq!(events(&pool, j).await.len(), 1);
    // empties zone: same rule, PLANT_EMPTY
    put_stock(&pool, ItemType::Product, j, "JAR_EMPTY", Uuid::nil(), "20", None).await;
    let (_, cnt2) = c.post("/api/warehouse/counts", json!({ "zone": "JAR_EMPTY" })).await;
    let cid2 = cnt2["id"].as_str().unwrap().to_string();
    let (s, _) = c.put(&format!("/api/warehouse/counts/{cid2}/lines"), json!({ "lines": [{ "line_id": cnt2["lines"][0]["line_id"], "counted_qty": 19, "note": "one missing" }] })).await;
    assert_eq!(s, 200);
    assert_eq!(c.post(&format!("/api/warehouse/counts/{cid2}/post"), json!({ "confirm": true })).await.0, 200);
    let ev = events(&pool, j).await;
    assert_eq!(ev.len(), 2);
    assert_eq!((ev[1].1, ev[1].3.as_deref()), (1, Some("PLANT_EMPTY")));
    let (_, f) = c.get(&format!("/api/jars/fleet?product_id={j}")).await;
    assert_eq!(f["period"]["destroyed_total"], 6);
    assert_eq!(f["fleet"]["total"], 35 + 19);
    check_invariants(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn jars_damaged_or_lost_between_godowns_write_fleet_events(pool: PgPool) {
    let c = setup(pool.clone()).await;
    let j = jar_product(&pool).await;
    let g2 = godown(&pool, "GDN-J2", false).await;
    put_stock(&pool, ItemType::Product, j, "JAR_CLEAN", Uuid::nil(), "30", None).await;
    let (s, tf) = c.post("/api/godowns/transfers", json!({ "from_location_id": water_erp::stock::MAIN_LOCATION, "to_location_id": g2,
        "lines": [{ "item_type": "PRODUCT", "item_id": j, "lot_id": Uuid::nil(), "zone": "JAR_CLEAN", "qty": 10 }] })).await;
    assert_eq!(s, 201, "{tf}");
    let tid = tf["id"].as_str().unwrap().to_string();
    let line = tf["lines"][0]["line_id"].clone();
    let recv = json!({ "lines": [{ "line_id": line, "qty_received": 6, "qty_damaged": 2, "note": "two jars lost on the road" }] });
    let (s, _) = c.post(&format!("/api/godowns/transfers/{tid}/receive?dry_run=1"), { let mut r = recv.clone(); r["confirm"] = json!(true); r }).await;
    assert_eq!(s, 200);
    assert!(events(&pool, j).await.is_empty(), "dry run leaves no event");
    let mut r = recv.clone();
    r["confirm"] = json!(true);
    let (s, v) = c.post(&format!("/api/godowns/transfers/{tid}/receive"), r).await;
    assert_eq!(s, 200, "{v}");
    let ev = events(&pool, j).await;
    assert_eq!(ev.len(), 2, "{ev:?}");
    let by = |reason: &str| ev.iter().find(|e| e.2.as_deref() == Some(reason)).unwrap().clone();
    assert_eq!((by("DAMAGED_RETURN").1, by("DAMAGED_RETURN").3.unwrap()), (2, "PLANT_CLEAN".to_string()));
    assert_eq!(by("LOST").1, 2);
    // the fleet agrees with stock: 20 left behind + 6 received
    let (_, f) = c.get(&format!("/api/jars/fleet?product_id={j}&location_id=all")).await;
    assert_eq!(f["fleet"]["total"], 26, "{f}");
    check_invariants(&pool).await;
    check_transit(&pool).await;
}
