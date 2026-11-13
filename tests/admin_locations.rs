mod common;
mod admin_support;
use admin_support::{admin, plain_user, req};
use axum::http::StatusCode;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;
use water_erp::stock::{self, ItemType, Move, MAIN_LOCATION, TRANSIT_LOCATION};

const PW: &str = "Another-Good-Pass-7";
fn code_of(b: &Value) -> &str { b["error"]["code"].as_str().unwrap_or("") }

#[sqlx::test(migrations = "./migrations")]
async fn location_routes_are_admin_only(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (_, user_key) = plain_user(&pool, "plain").await;
    let nil = Uuid::nil();
    for (m, p) in [("GET", "locations".to_string()), ("POST", "locations".into()), ("GET", format!("locations/{nil}")), ("PUT", format!("locations/{nil}")), ("DELETE", format!("locations/{nil}"))] {
        assert_eq!(req(&app, m, &format!("/api/admin/{p}"), Some(&user_key), Some(json!({}))).await.0, StatusCode::FORBIDDEN, "{m} {p}");
        assert_eq!(req(&app, m, &format!("/api/admin/{p}"), None, None).await.0, StatusCode::UNAUTHORIZED, "anon {m} {p}");
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn location_crud_validation_and_audit(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (_, k) = admin(&pool, "root").await;
    let call = |m: &'static str, p: String, b: Option<Value>| { let (app, k) = (app.clone(), k.clone()); async move { req(&app, m, &format!("/api/admin/locations{p}"), Some(&k), b).await } };

    // the list shows MAIN only (TRANSIT is a system location and never listed)
    let (s, l) = call("GET", "".into(), None).await;
    assert_eq!(s, StatusCode::OK);
    let codes: Vec<&str> = l.as_array().unwrap().iter().map(|x| x["code"].as_str().unwrap()).collect();
    assert_eq!(codes, vec!["MAIN"]);

    let (s, g) = call("POST", "".into(), Some(json!({"code":"gdn-east","name":"East Godown","city":"Pune","phone":"+91 20 5550 1234"}))).await;
    assert_eq!(s, StatusCode::CREATED, "{g}");
    assert_eq!((g["code"].as_str(), g["kind"].as_str(), g["is_production_site"].as_bool(), g["is_active"].as_bool()), (Some("GDN-EAST"), Some("GODOWN"), Some(false), Some(true)));
    let id = g["id"].as_str().unwrap().to_string();

    // validation
    let bad = [
        (json!({"code":"X1","name":"x","kind":"TRANSIT"}), "kind TRANSIT"),
        (json!({"code":"X1","name":"x","kind":"WAREHOUSE"}), "kind unknown"),
        (json!({"code":"X1"}), "name missing"),
        (json!({"code":"","name":"x"}), "code empty"),
        (json!({"code":"has space","name":"x"}), "code chars"),
        (json!({"code":"X1","name":"   "}), "blank name"),
        (json!({"code":"X1","name":"x","phone":"call me"}), "phone"),
        (json!({"code":"X1","name":"x\u{0}y"}), "control char"),
    ];
    for (b, what) in bad {
        let (s, r) = call("POST", "".into(), Some(b)).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{what}: {r}");
    }
    let (s, _) = call("POST", "".into(), Some(json!({"code":"X1","name":"x","id":Uuid::new_v4()}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "unknown field rejected");
    let (s, r) = call("POST", "".into(), Some(json!({"code":"GDN-EAST","name":"Other"}))).await;
    assert_eq!((s, code_of(&r)), (StatusCode::CONFLICT, "DUPLICATE"), "{r}");
    let (s, r) = call("POST", "".into(), Some(json!({"code":"transit","name":"Fake"}))).await;
    assert_eq!((s, code_of(&r)), (StatusCode::CONFLICT, "DUPLICATE"), "the TRANSIT code is taken: {r}");
    let (s, _) = call("GET", "/not-a-uuid".into(), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // partial update: absent = unchanged; kind PLANT allowed; kind TRANSIT never
    let (s, u) = call("PUT", format!("/{id}"), Some(json!({"name":"East Godown 2","city":"Mumbai"}))).await;
    assert_eq!(s, StatusCode::OK, "{u}");
    assert_eq!((u["name"].as_str(), u["city"].as_str(), u["code"].as_str(), u["phone"].as_str()), (Some("East Godown 2"), Some("Mumbai"), Some("GDN-EAST"), Some("+91 20 5550 1234")));
    assert_eq!(call("PUT", format!("/{id}"), Some(json!({"kind":"TRANSIT"}))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(call("PUT", format!("/{id}"), Some(json!({"kind":"PLANT","is_production_site":true}))).await.0, StatusCode::OK);
    assert_eq!(call("PUT", format!("/{id}"), Some(json!({"kind":"GODOWN","is_production_site":false}))).await.0, StatusCode::OK);

    // TRANSIT is not addressable at all
    let t = TRANSIT_LOCATION;
    assert_eq!(call("GET", format!("/{t}"), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call("PUT", format!("/{t}"), Some(json!({"name":"hack"}))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call("DELETE", format!("/{t}"), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call("GET", format!("/{}", Uuid::new_v4()), None).await.0, StatusCode::NOT_FOUND);

    // soft delete: row stays, filters work, idempotent, can be reactivated
    assert_eq!(call("DELETE", format!("/{id}"), None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(call("DELETE", format!("/{id}"), None).await.0, StatusCode::NO_CONTENT);
    let (_, l) = call("GET", "?active=true".into(), None).await;
    assert_eq!(l.as_array().unwrap().len(), 1);
    let (_, l) = call("GET", "?q=east".into(), None).await;
    assert_eq!(l.as_array().unwrap().len(), 1);
    assert_eq!(l[0]["is_active"], json!(false));
    let (s, u) = call("PUT", format!("/{id}"), Some(json!({"is_active":true}))).await;
    assert_eq!((s, u["is_active"].clone()), (StatusCode::OK, json!(true)));

    let acts: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_logs WHERE entity='location' ORDER BY id").fetch_all(&pool).await.unwrap();
    assert_eq!(acts.iter().filter(|a| *a == "LOCATION_CREATE").count(), 1);
    assert!(acts.iter().any(|a| a == "LOCATION_UPDATE") && acts.iter().any(|a| a == "LOCATION_DEACTIVATE"), "{acts:?}");
    let failed_changes: i64 = sqlx::query_scalar("SELECT count(*) FROM locations WHERE code IN ('X1','TRANSIT2','FAKE')").fetch_one(&pool).await.unwrap();
    assert_eq!(failed_changes, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn main_location_is_protected(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (_, k) = admin(&pool, "root").await;
    let call = |m: &'static str, b: Option<Value>| { let (app, k) = (app.clone(), k.clone()); async move { req(&app, m, &format!("/api/admin/locations/{MAIN_LOCATION}"), Some(&k), b).await } };
    let (s, r) = call("DELETE", None).await;
    assert_eq!((s, code_of(&r)), (StatusCode::CONFLICT, "PROTECTED"), "{r}");
    for b in [json!({"is_active":false}), json!({"kind":"GODOWN"}), json!({"is_production_site":false}), json!({"code":"HQ"})] {
        let (s, r) = call("PUT", Some(b.clone())).await;
        assert_eq!((s, code_of(&r)), (StatusCode::CONFLICT, "PROTECTED"), "{b}: {r}");
    }
    // harmless edits still work, and nothing changed otherwise
    let (s, r) = call("PUT", Some(json!({"name":"Main Plant (Pune)","city":"Pune"}))).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (code, kind, prod, active): (String, String, bool, bool) = sqlx::query_as("SELECT code, kind, is_production_site, is_active FROM locations WHERE id=$1").bind(MAIN_LOCATION).fetch_one(&pool).await.unwrap();
    assert_eq!((code.as_str(), kind.as_str(), prod, active), ("MAIN", "PLANT", true, true));
}

#[sqlx::test(migrations = "./migrations")]
async fn cannot_deactivate_godown_with_stock_or_transit(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (_, k) = admin(&pool, "root").await;
    let (s, g) = req(&app, "POST", "/api/admin/locations", Some(&k), Some(json!({"code":"G-STK","name":"Stock Godown"}))).await;
    assert_eq!(s, StatusCode::CREATED);
    let gid: Uuid = g["id"].as_str().unwrap().parse().unwrap();
    let item = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Raw, item, "RM_STORE", Uuid::nil(), dec!(5), "GRN").at(gid)]).await.unwrap();
    tx.commit().await.unwrap();
    for (m, b) in [("DELETE", None), ("PUT", Some(json!({"is_active":false})))] {
        let (s, r) = req(&app, m, &format!("/api/admin/locations/{gid}"), Some(&k), b).await;
        assert_eq!((s, code_of(&r)), (StatusCode::CONFLICT, "IN_USE"), "{m}: {r}");
    }
    let active: bool = sqlx::query_scalar("SELECT is_active FROM locations WHERE id=$1").bind(gid).fetch_one(&pool).await.unwrap();
    assert!(active);
    // empty it: zero balance rows do not block
    let mut tx = pool.begin().await.unwrap();
    stock::post_all(&mut tx, vec![Move::new(ItemType::Raw, item, "RM_STORE", Uuid::nil(), dec!(-5), "ADJUST").at(gid)]).await.unwrap();
    tx.commit().await.unwrap();
    // ... but an IN_TRANSIT transfer to or from the godown does
    let tf: Uuid = sqlx::query_scalar("INSERT INTO godown_transfers (tf_no, from_location_id, to_location_id) VALUES ('TF-T-1',$1,$2) RETURNING id").bind(MAIN_LOCATION).bind(gid).fetch_one(&pool).await.unwrap();
    let (s, r) = req(&app, "DELETE", &format!("/api/admin/locations/{gid}"), Some(&k), None).await;
    assert_eq!((s, code_of(&r), r["error"]["details"]["transfers_in_transit"].clone()), (StatusCode::CONFLICT, "IN_USE", json!(1)), "{r}");
    sqlx::query("UPDATE godown_transfers SET status='RECEIVED' WHERE id=$1").bind(tf).execute(&pool).await.unwrap();
    let (s, _) = req(&app, "DELETE", &format!("/api/admin/locations/{gid}"), Some(&k), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='LOCATION_DEACTIVATE' AND entity_id=$1").bind(gid.to_string()).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1, "refused attempts leave no audit row");
}

#[sqlx::test(migrations = "./migrations")]
async fn user_godown_restriction_round_trip(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (_, k) = admin(&pool, "root").await;
    let mk = |code: &'static str| { let (app, k) = (app.clone(), k.clone()); async move {
        let (s, g) = req(&app, "POST", "/api/admin/locations", Some(&k), Some(json!({"code":code,"name":code}))).await;
        assert_eq!(s, StatusCode::CREATED, "{g}");
        g["id"].as_str().unwrap().to_string()
    } };
    let (g1, g2, g3) = (mk("G1").await, mk("G2").await, mk("G3").await);
    assert_eq!(req(&app, "DELETE", &format!("/api/admin/locations/{g3}"), Some(&k), None).await.0, StatusCode::NO_CONTENT);

    // default: no restriction
    let (s, u) = req(&app, "POST", "/api/admin/users", Some(&k), Some(json!({"username":"keeper","full_name":"Keeper","password":PW,"allowed_pages":["warehouse"]}))).await;
    assert_eq!(s, StatusCode::CREATED, "{u}");
    assert_eq!(u["allowed_location_ids"], json!([]));
    let uid = u["id"].as_str().unwrap().to_string();

    // invalid assignments, nothing written
    let transit = TRANSIT_LOCATION.to_string();
    let too_many: Vec<String> = (0..51).map(|_| Uuid::new_v4().to_string()).collect();
    for (ids, what) in [(json!([transit]), "TRANSIT"), (json!([Uuid::new_v4()]), "unknown"), (json!([g3]), "inactive"), (json!(too_many), ">50"), (json!([g1, "nope"]), "not a uuid")] {
        let (s, r) = req(&app, "PUT", &format!("/api/admin/users/{uid}"), Some(&k), Some(json!({"allowed_location_ids": ids}))).await;
        assert!(s == StatusCode::UNPROCESSABLE_ENTITY || s == StatusCode::BAD_REQUEST, "{what}: {s} {r}");
        let (s, r) = req(&app, "POST", "/api/admin/users", Some(&k), Some(json!({"username":"bad","full_name":"Bad","password":PW,"allowed_location_ids": ids}))).await;
        assert!(s == StatusCode::UNPROCESSABLE_ENTITY || s == StatusCode::BAD_REQUEST, "create {what}: {s} {r}");
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE username='bad'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
    let (_, u) = req(&app, "GET", &format!("/api/admin/users/{uid}"), Some(&k), None).await;
    assert_eq!(u["allowed_location_ids"], json!([]));

    // restrict to two godowns (duplicates collapse), read back via get and list
    let (s, u) = req(&app, "PUT", &format!("/api/admin/users/{uid}"), Some(&k), Some(json!({"allowed_location_ids":[g1, g2, g1]}))).await;
    assert_eq!((s, u["allowed_location_ids"].clone()), (StatusCode::OK, json!([g1, g2])), "{u}");
    let (_, list) = req(&app, "GET", "/api/admin/users?q=keeper", Some(&k), None).await;
    assert_eq!(list[0]["allowed_location_ids"], json!([g1, g2]));
    // unrelated update leaves the restriction alone
    let (_, u) = req(&app, "PUT", &format!("/api/admin/users/{uid}"), Some(&k), Some(json!({"full_name":"Keeper Two"}))).await;
    assert_eq!(u["allowed_location_ids"], json!([g1, g2]));

    // Me (login response and /api/me) and the auth extractor see it
    let (s, login) = req(&app, "POST", "/api/auth/login", None, Some(json!({"username":"keeper","password":PW}))).await;
    assert_eq!(s, StatusCode::OK, "{login}");
    assert_eq!(login["user"]["allowed_location_ids"], json!([g1, g2]));
    let key = login["key"].as_str().unwrap().to_string();
    let (_, me) = req(&app, "GET", "/api/me", Some(&key), None).await;
    assert_eq!(me["allowed_location_ids"], json!([g1, g2]));
    let (s, ses) = req(&app, "GET", &format!("/api/admin/sessions?user_id={uid}"), Some(&k), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ses[0]["allowed_location_ids"], json!([g1, g2]), "sessions list shows the restriction");
    let st = common::state(pool.clone()).await;
    let au = water_erp::auth::authenticate(&st, &key).await.unwrap();
    assert!(au.can_use_location(g1.parse().unwrap()) && au.can_use_location(g2.parse().unwrap()) && !au.can_use_location(MAIN_LOCATION));

    // admins' Me carries the (empty) list too; [] lifts the restriction again
    let (_, admin_me) = req(&app, "GET", "/api/me", Some(&k), None).await;
    assert_eq!(admin_me["allowed_location_ids"], json!([]));
    let (s, u) = req(&app, "PUT", &format!("/api/admin/users/{uid}"), Some(&k), Some(json!({"allowed_location_ids":[]}))).await;
    assert_eq!((s, u["allowed_location_ids"].clone()), (StatusCode::OK, json!([])));
    let (_, me) = req(&app, "GET", "/api/me", Some(&key), None).await;
    assert_eq!(me["allowed_location_ids"], json!([]));

    // created restricted from the start
    let (s, u2) = req(&app, "POST", "/api/admin/users", Some(&k), Some(json!({"username":"north","full_name":"North","password":PW,"allowed_location_ids":[g2]}))).await;
    assert_eq!((s, u2["allowed_location_ids"].clone()), (StatusCode::CREATED, json!([g2])), "{u2}");

    // audited with before/after
    let d: Value = sqlx::query_scalar("SELECT details FROM audit_logs WHERE action='USER_UPDATE' AND entity_id=$1 AND details #> '{after,allowed_location_ids}' = $2::jsonb ORDER BY id LIMIT 1")
        .bind(&uid).bind(json!([g1, g2])).fetch_one(&pool).await.unwrap();
    assert_eq!(d["before"]["allowed_location_ids"], json!([]));
    let c: Value = sqlx::query_scalar("SELECT details FROM audit_logs WHERE action='USER_CREATE' AND details->>'username'='north'").fetch_one(&pool).await.unwrap();
    assert_eq!(c["allowed_location_ids"], json!([g2]));
}
