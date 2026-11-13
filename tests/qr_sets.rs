mod loading_support;
use axum::http::StatusCode;
use loading_support::*;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use sqlx::PgPool;
use water_erp::routes::qr::label_for;

async fn qr_ctx(pool: PgPool) -> Ctx {
    let c = ctx(pool).await;
    c.set("features.qr_labels", json!(true)).await;
    c
}
fn labels(v: &Value) -> Vec<String> { v.as_array().unwrap().iter().map(|s| s["set_label"].as_str().unwrap().to_string()).collect() }
fn sizes(v: &Value) -> Vec<i64> { v.as_array().unwrap().iter().map(|s| s["original_crates"].as_i64().unwrap()).collect() }

#[test]
fn label_sequence_is_x_y_z_then_a_to_w_then_double_letters() {
    let l: Vec<String> = (0..30).map(label_for).collect();
    assert_eq!(&l[..5], ["X", "Y", "Z", "A", "B"]);
    assert_eq!(l[25], "W");
    assert_eq!(&l[26..30], ["AA", "AB", "AC", "AD"]);
    assert_eq!(label_for(26 + 25), "AZ");
    assert_eq!(label_for(26 + 26), "BA");
    assert_eq!(label_for(26 + 676), "AAA");
}

#[sqlx::test(migrations = "./migrations")]
async fn generation_modes_keep_the_partition_invariant(pool: PgPool) {
    let c = qr_ctx(pool).await;
    let p = c.product("BOTTLED", 12, Some(10)).await;
    let b = c.batch(p, 100, "QUARANTINE", 90).await;
    let uri = format!("/api/qr/batches/{b}/sets");
    // uniform defaults to crates_per_pallet (10) -> 4 sets of 10 from the first 40? no: custom first
    let (s, v) = c.post(&uri, json!({ "mode": "CUSTOM", "sizes": [30, 20] })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(labels(&v), ["X", "Y"]);
    assert_eq!(v[0]["status"], "QUARANTINE");
    assert!(v[0]["svg"].as_str().unwrap().starts_with("<svg"));
    assert!(v[0]["qr_url"].as_str().unwrap().ends_with(&format!("/loading.html?qr={}", v[0]["code"].as_str().unwrap())));
    assert_eq!(v[0]["code"].as_str().unwrap().len(), 10);
    // more than what is left -> rejected, nothing created
    let (s, _) = c.post(&uri, json!({ "mode": "CUSTOM", "sizes": [30, 21] })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // uniform fills the remaining 50 with sets of 20 (20,20,10), labels continue
    let (s, v) = c.post(&uri, json!({ "mode": "UNIFORM", "set_size": 20 })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(labels(&v), ["Z", "A", "B"]);
    assert_eq!(sizes(&v), [20, 20, 10]);
    // everything is labelled now
    let (s, _) = c.post(&uri, json!({ "mode": "COUNT", "count": 2 })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let total: i64 = sqlx::query_scalar("SELECT SUM(original_crates)::bigint FROM qr_crate_sets WHERE batch_id=$1 AND parent_set_id IS NULL").bind(b).fetch_one(&c.pool).await.unwrap();
    assert_eq!(total, 100);
    // COUNT splits near-equal
    let b2 = c.batch(p, 10, "APPROVED", 90).await;
    let (s, v) = c.post(&format!("/api/qr/batches/{b2}/sets"), json!({ "mode": "COUNT", "count": 3 })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(sizes(&v), [4, 3, 3]);
    assert_eq!(v[0]["status"], "AVAILABLE");
    assert_eq!(v[0]["loadable"], true);
    // validation
    for bad in [json!({ "mode": "NOPE" }), json!({ "mode": "COUNT" }), json!({ "mode": "COUNT", "count": 0 }), json!({ "mode": "CUSTOM", "sizes": [] }),
                json!({ "mode": "CUSTOM", "sizes": [0, 5] }), json!({ "mode": "CUSTOM", "sizes": [-1] }), json!({ "mode": "UNIFORM", "set_size": 0 }),
                json!({ "mode": "UNIFORM", "set_size": 1, "status": "AVAILABLE" })] {
        let (s, _) = c.post(&format!("/api/qr/batches/{b2}/sets"), bad.clone()).await;
        assert!(s == StatusCode::BAD_REQUEST || s == StatusCode::UNPROCESSABLE_ENTITY, "{bad} -> {s}");
    }
    // rejected batch: no labels
    let br = c.batch(p, 10, "REJECTED", 90).await;
    let (s, v) = c.post(&format!("/api/qr/batches/{br}/sets"), json!({ "mode": "COUNT", "count": 1 })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "QC_LOCKED"));
    // a loading-only user may read labels but not create them
    let lo = common::make_user(&c.pool, "loader2", false, &["loading"], &[]).await;
    let k = common::session_for(&c.pool, lo).await;
    assert_eq!(c.post_as(&k, &format!("/api/qr/batches/{b2}/sets"), json!({ "mode": "COUNT", "count": 1 })).await.0, StatusCode::FORBIDDEN);
    assert_eq!(common::call(&c.app, "GET", &format!("/api/qr/batches/{b2}/sets"), Some(&k), None).await.0, StatusCode::OK);
}

#[sqlx::test(migrations = "./migrations")]
async fn lookup_search_sheet_and_injection(pool: PgPool) {
    let c = qr_ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 20, "APPROVED", 90).await;
    let (_, sets) = c.post(&format!("/api/qr/batches/{b}/sets"), json!({ "mode": "COUNT", "count": 2 })).await;
    let code0 = sets[0]["code"].as_str().unwrap().to_string();
    let id0 = sets[0]["id"].as_str().unwrap().to_string();
    let (s, v) = c.get(&format!("/api/qr/sets/lookup/{}", code0.to_lowercase())).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["id"], json!(id0));
    assert_eq!(c.get("/api/qr/sets/lookup/ZZZZZZZZZZ").await.0, StatusCode::NOT_FOUND);
    assert_eq!(c.get("/api/qr/sets/lookup/x%27%20OR%201%3D1--").await.0, StatusCode::NOT_FOUND);
    let (s, v) = c.get(&format!("/api/qr/sets/{id0}/qr")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["code"], json!(code0));
    assert!(v["svg"].as_str().unwrap().starts_with("<svg") && !v["svg"].as_str().unwrap().contains("<script"));
    let (_, l) = c.get(&format!("/api/qr/sets?batch_id={b}&status=AVAILABLE")).await;
    assert_eq!(l["total"], 2);
    let (_, l) = c.get(&format!("/api/qr/sets?q={}", &code0[..4])).await;
    assert!(l["total"].as_i64().unwrap() >= 1);
    for q in ["%27%20OR%201%3D1%20--", "%25", "x%27%3B%20DROP%20TABLE%20qr_crate_sets%3B--"] {
        let (s, l) = c.get(&format!("/api/qr/sets?q={q}")).await;
        assert_eq!((s, l["total"].as_i64()), (StatusCode::OK, Some(0)), "{q}");
    }
    assert_eq!(c.get("/api/qr/sets?status=BOGUS").await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, sh) = c.get(&format!("/api/qr/batches/{b}/sheet")).await;
    assert_eq!(s, StatusCode::OK, "{sh}");
    assert_eq!(sh["sets"].as_array().unwrap().len(), 2);
    assert!(sh["sets"][0]["svg"].is_string());
    assert!(sh["batch"]["batch_no"].is_string() && sh["product"]["name"].is_string());
}

#[sqlx::test(migrations = "./migrations")]
async fn delete_only_unused_root_sets(pool: PgPool) {
    let c = qr_ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 20, "APPROVED", 90).await;
    let (_, sets) = c.post(&format!("/api/qr/batches/{b}/sets"), json!({ "mode": "COUNT", "count": 2 })).await;
    let (s0, s1) = (sets[0]["code"].as_str().unwrap().to_string(), sets[1]["id"].as_str().unwrap().to_string());
    let t = c.trip(c.vehicle(100).await).await;
    assert_eq!(c.add(t, json!({ "qr": s0 })).await.0, StatusCode::CREATED);
    let used = sets[0]["id"].as_str().unwrap();
    let (s, v) = c.delete(&format!("/api/qr/sets/{used}")).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INVALID_STATE"), "{v}");
    assert_eq!(c.delete(&format!("/api/qr/sets/{s1}")).await.0, StatusCode::NO_CONTENT);
    assert_eq!(c.get(&format!("/api/qr/sets/{s1}/qr")).await.0, StatusCode::NOT_FOUND);
    // the freed allocation can be labelled again
    let (s, v) = c.post(&format!("/api/qr/batches/{b}/sets"), json!({ "mode": "UNIFORM", "set_size": 10 })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(sizes(&v), [10]);
}

#[sqlx::test(migrations = "./migrations")]
async fn qr_lifecycle_partial_split_gate_pass_and_settlement(pool: PgPool) {
    let c = qr_ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let b = c.batch(p, 30, "APPROVED", 90).await;
    let (_, sets) = c.post(&format!("/api/qr/batches/{b}/sets"), json!({ "mode": "CUSTOM", "sizes": [10, 10, 10] })).await;
    let code = |i: usize| sets[i]["code"].as_str().unwrap().to_string();
    let t = c.trip(c.vehicle(100).await).await;
    // whole set
    let (s, v) = c.add(t, json!({ "qr": code(0) })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["item"]["crates"], 10);
    assert_eq!(v["item"]["set_label"], "X");
    assert!(v["residual_set"].is_null());
    // same label again
    let (s, v) = c.add(t, json!({ "qr": code(0) })).await;
    assert_eq!((s, code_of(&v)), (StatusCode::CONFLICT, "INVALID_STATE"), "{v}");
    // partial: 4 of 10 -> the scanned label keeps 4, a residual child with 6 is created
    let (s, v) = c.add(t, json!({ "qr": code(1).to_lowercase(), "crates": 4 })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["item"]["crates"], 4);
    let r = &v["residual_set"];
    assert_eq!((r["crates"].as_i64(), r["set_label"].as_str(), r["status"].as_str()), (Some(6), Some("Y-R1"), Some("AVAILABLE")));
    assert!(r["svg"].as_str().unwrap().starts_with("<svg"));
    assert_ne!(r["code"], json!(code(1)));
    assert_eq!(r["parent_set_id"], sets[1]["id"]);
    // crates are conserved and the root partition is untouched
    let (kept, resid): (i32, i32) = (
        sqlx::query_scalar("SELECT crates FROM qr_crate_sets WHERE id=$1").bind(uuid(&sets[1]["id"])).fetch_one(&c.pool).await.unwrap(),
        sqlx::query_scalar("SELECT crates FROM qr_crate_sets WHERE id=$1").bind(uuid(&r["id"])).fetch_one(&c.pool).await.unwrap());
    assert_eq!(kept + resid, 10);
    let roots: i64 = sqlx::query_scalar("SELECT SUM(original_crates)::bigint FROM qr_crate_sets WHERE batch_id=$1 AND parent_set_id IS NULL").bind(b).fetch_one(&c.pool).await.unwrap();
    assert_eq!(roots, 30);
    // bad crates
    assert_eq!(c.add(t, json!({ "qr": code(2), "crates": 11 })).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(c.add(t, json!({ "qr": code(2), "crates": 0 })).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(c.add(t, json!({ "qr": "NOSUCHCODE" })).await.0, StatusCode::NOT_FOUND);
    // reservations are shared with bulk loading: 14 reserved, 16 free
    let (_, av) = c.get(&format!("/api/loading/availability?product_id={p}")).await;
    assert_eq!((av[0]["reserved"].as_i64(), av[0]["available"].as_i64()), (Some(14), Some(16)));
    // removing the partial item frees the label (it keeps its 4 crates; residual is not merged)
    let (_, trip) = c.get(&format!("/api/loading/trips/{t}")).await;
    let partial_item = trip["items"].as_array().unwrap().iter().find(|i| i["crates"] == 4).unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(c.delete(&format!("/api/loading/trips/{t}/items/{partial_item}")).await.0, StatusCode::OK);
    let st: (String, i32) = sqlx::query_as("SELECT status, crates FROM qr_crate_sets WHERE id=$1").bind(uuid(&sets[1]["id"])).fetch_one(&c.pool).await.unwrap();
    assert_eq!(st, ("AVAILABLE".to_string(), 4));
    // add it again plus the bulk path in the same trip, then gate pass
    assert_eq!(c.add(t, json!({ "qr": code(1) })).await.0, StatusCode::CREATED);
    assert_eq!(c.add(t, json!({ "batch_id": b, "crates": 3 })).await.0, StatusCode::CREATED);
    let (s, v) = c.gate(t).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(13));
    let loaded: i64 = sqlx::query_scalar("SELECT count(*) FROM qr_crate_sets WHERE trip_id=$1 AND status='LOADED'").bind(t).fetch_one(&c.pool).await.unwrap();
    assert_eq!(loaded, 2);
    // settle: set X delivered completely, set Y (4) returns 3, bulk item (3) delivered
    let items = v["items"].as_array().unwrap().clone();
    let pick = |label: Option<&str>| items.iter().find(|i| i["set_label"].as_str() == label).unwrap()["id"].clone();
    let body = json!({ "items": [
        { "item_id": pick(Some("X")), "delivered": 10, "returned": 0, "damaged": 0 },
        { "item_id": pick(Some("Y")), "delivered": 1, "returned": 3, "damaged": 0 },
        { "item_id": pick(None), "delivered": 3, "returned": 0, "damaged": 0 } ] });
    let (s, v) = c.post(&format!("/api/loading/trips/{t}/settle"), body).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let status_of = |id: Value| { let pool = c.pool.clone(); async move { sqlx::query_as::<_, (String, i32)>("SELECT status, crates FROM qr_crate_sets WHERE id=$1").bind(uuid(&id)).fetch_one(&pool).await.unwrap() } };
    assert_eq!(status_of(sets[0]["id"].clone()).await, ("DELIVERED".to_string(), 10));
    assert_eq!(status_of(sets[1]["id"].clone()).await, ("AVAILABLE".to_string(), 3));
    assert_eq!(c.qty(p, "FG_READY", b).await, dec!(16));
    c.assert_ledger_consistent().await;
}

fn code_of(v: &Value) -> &str { code(v) }

#[sqlx::test(migrations = "./migrations")]
async fn qr_qc_lock_cancel_and_void_transitions(pool: PgPool) {
    let c = qr_ctx(pool).await;
    let p = c.product("BOTTLED", 12, None).await;
    let q = c.batch(p, 10, "QUARANTINE", 90).await;
    let (_, qs) = c.post(&format!("/api/qr/batches/{q}/sets"), json!({ "mode": "COUNT", "count": 1 })).await;
    let t = c.trip(c.vehicle(100).await).await;
    let (s, v) = c.add(t, json!({ "qr": qs[0]["code"] })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "QC_LOCKED"), "{v}");
    assert_eq!(qs[0]["loadable"], false);
    // non-strict: loadable, then cancelling returns the set to QUARANTINE
    c.set("loading.strict_qc_lock", json!(false)).await;
    assert_eq!(c.add(t, json!({ "qr": qs[0]["code"] })).await.0, StatusCode::CREATED);
    let st: String = sqlx::query_scalar("SELECT status FROM qr_crate_sets WHERE id=$1").bind(uuid(&qs[0]["id"])).fetch_one(&c.pool).await.unwrap();
    assert_eq!(st, "RESERVED");
    assert_eq!(c.post(&format!("/api/loading/trips/{t}/cancel"), json!({})).await.0, StatusCode::OK);
    let st: (String, Option<uuid::Uuid>) = sqlx::query_as("SELECT status, trip_id FROM qr_crate_sets WHERE id=$1").bind(uuid(&qs[0]["id"])).fetch_one(&c.pool).await.unwrap();
    assert_eq!(st, ("QUARANTINE".to_string(), None));
    // approved batch: gate pass -> LOADED -> void -> AVAILABLE
    let a = c.batch(p, 10, "APPROVED", 90).await;
    let (_, as_) = c.post(&format!("/api/qr/batches/{a}/sets"), json!({ "mode": "COUNT", "count": 1 })).await;
    let t2 = c.trip(c.vehicle(100).await).await;
    assert_eq!(c.add(t2, json!({ "qr": as_[0]["code"] })).await.0, StatusCode::CREATED);
    // a second trip cannot take the reserved label
    let t3 = c.trip(c.vehicle(100).await).await;
    let (s, v) = c.add(t3, json!({ "qr": as_[0]["code"] })).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "INVALID_STATE"), "{v}");
    assert_eq!(c.gate(t2).await.0, StatusCode::OK);
    assert_eq!(c.qty(p, "FG_READY", a).await, dec!(0));
    let (s, v) = c.post(&format!("/api/loading/trips/{t2}/void"), json!({ "reason": "wrong truck loaded", "confirm": true })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let st: (String, Option<uuid::Uuid>) = sqlx::query_as("SELECT status, trip_id FROM qr_crate_sets WHERE id=$1").bind(uuid(&as_[0]["id"])).fetch_one(&c.pool).await.unwrap();
    assert_eq!(st, ("AVAILABLE".to_string(), None));
    assert_eq!(c.qty(p, "FG_READY", a).await, dec!(10));
    c.assert_ledger_consistent().await;
}
