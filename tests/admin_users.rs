mod common;
mod admin_support;
use admin_support::{admin, plain_user, req};
use axum::{http::StatusCode, middleware, routing::get, Router};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

const PW: &str = "Another-Good-Pass-7";

#[sqlx::test(migrations = "./migrations")]
async fn every_admin_route_family_is_admin_only(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (_, user_key) = plain_user(&pool, "plain").await;
    let nil = Uuid::nil();
    let gets = ["settings", "pages", "notification-types", "users", "sessions", "audit", "suppliers", "raw-materials", "products", "customers",
                "routes", "vehicles", "equipment", "maintenance-schedules", "locations"];
    for g in gets {
        assert_eq!(req(&app, "GET", &format!("/api/admin/{g}"), Some(&user_key), None).await.0, StatusCode::FORBIDDEN, "GET {g}");
        assert_eq!(req(&app, "GET", &format!("/api/admin/{g}"), None, None).await.0, StatusCode::UNAUTHORIZED, "anon GET {g}");
    }
    let writes = [
        ("PUT", "settings".to_string()), ("POST", "users".into()), ("PUT", format!("users/{nil}")), ("DELETE", format!("users/{nil}")),
        ("POST", format!("users/{nil}/password")), ("POST", format!("users/{nil}/unlock")), ("POST", format!("users/{nil}/revoke-sessions")),
        ("DELETE", format!("sessions/{nil}")), ("POST", "sessions/revoke-all".into()),
        ("POST", "suppliers".into()), ("PUT", format!("suppliers/{nil}")), ("DELETE", format!("suppliers/{nil}")),
        ("POST", "raw-materials".into()), ("POST", "products".into()), ("PUT", format!("products/{nil}/bom")), ("GET", format!("products/{nil}/bom")),
        ("POST", "customers".into()), ("POST", "locations".into()), ("PUT", format!("locations/{nil}")), ("DELETE", format!("locations/{nil}")), ("POST", "routes".into()), ("POST", "vehicles".into()), ("POST", "equipment".into()), ("POST", "maintenance-schedules".into()),
    ];
    for (m, p) in writes {
        let (s, _) = req(&app, m, &format!("/api/admin/{p}"), Some(&user_key), Some(json!({}))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}");
    }
    // nothing leaked into the DB
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn allowed_pages_are_enforced_by_the_guard(pool: PgPool) {
    use water_erp::auth;
    let st = common::state(pool.clone()).await;
    let probe = Router::new()
        .route("/api/probe-qc", get(|| async { "qc" })).layer(middleware::from_fn(|req, next| auth::page_guard(&["qc"], req, next)))
        .layer(middleware::from_fn_with_state(st, auth::auth_middleware));
    let a = common::make_user(&pool, "a", false, &["qc"], &[]).await;
    let b = common::make_user(&pool, "b", false, &["warehouse"], &[]).await;
    let (ka, kb) = (common::session_for(&pool, a).await, common::session_for(&pool, b).await);
    assert_eq!(req(&probe, "GET", "/api/probe-qc", Some(&ka), None).await.0, StatusCode::OK);
    assert_eq!(req(&probe, "GET", "/api/probe-qc", Some(&kb), None).await.0, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "./migrations")]
async fn users_crud_validation_and_no_hash_leak(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (_, k) = admin(&pool, "root").await;
    let create = |v: Value| { let (app, k) = (app.clone(), k.clone()); async move { req(&app, "POST", "/api/admin/users", Some(&k), Some(v)).await } };
    let ok = json!({"username":"Maria","full_name":"Maria QC","password":PW,"is_admin":false,"allowed_pages":["qc","dashboard"],"subscribed_notifications":["QC_PENDING"]});
    let (s, u) = create(ok.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{u}");
    let uid = u["id"].as_str().unwrap().to_string();
    let txt = u.to_string();
    assert!(!txt.contains("password") && !txt.contains("argon2") && !txt.contains("key_hash"), "{txt}");
    assert_eq!(u["allowed_pages"], json!(["qc", "dashboard"]));
    // duplicates (case-insensitive)
    let mut dup = ok.clone(); dup["username"] = json!("MARIA");
    let (s, b) = create(dup).await; assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    // validation matrix
    for (field, val) in [("username", json!("ab")), ("username", json!("bad name")), ("username", json!("x".repeat(41))), ("username", json!("o'brien")),
                         ("password", json!("short")), ("full_name", json!("")), ("full_name", json!("a".repeat(121))),
                         ("full_name", json!("tab\u{0007}bell")), ("allowed_pages", json!(["nonsense"])), ("allowed_pages", json!(["admin"])),
                         ("subscribed_notifications", json!(["NOPE"]))] {
        let mut v = json!({"username":"newguy","full_name":"New","password":PW});
        v[field] = val.clone();
        let (s, b) = create(v).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{field}={val}: {b}");
        assert!(b["error"]["fields"][field].is_string(), "{field}: {b}");
    }
    let (s, _) = create(json!({"username":"abcdefghij","full_name":"Same","password":"ABCDEFGHIJ"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "password equal to username (case-insensitive)");
    // mass assignment / unknown fields are rejected, nothing created
    for extra in ["is_active", "id", "password_hash", "failed_login_count", "created_at"] {
        let mut v = json!({"username":"mass","full_name":"M","password":PW});
        v[extra] = json!("x");
        let (s, _) = create(v).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{extra}");
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE username IN ('mass','newguy')").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
    // get / list / search (injection-safe)
    let (s, g) = req(&app, "GET", &format!("/api/admin/users/{uid}"), Some(&k), None).await;
    assert_eq!((s, g["username"].as_str()), (StatusCode::OK, Some("Maria")));
    let (s, l) = req(&app, "GET", "/api/admin/users?q=mar", Some(&k), None).await;
    assert_eq!(s, StatusCode::OK); assert_eq!(l.as_array().unwrap().len(), 1);
    for q in ["%", "_", "' OR 1=1 --", "\\", "'; DROP TABLE users; --"] {
        let (s, l) = req(&app, "GET", &format!("/api/admin/users?q={}", urlenc(q)), Some(&k), None).await;
        assert_eq!(s, StatusCode::OK, "{q}");
        assert!(l.as_array().unwrap().len() <= 1, "{q} must not match everything: {l}");
    }
    let (s, _) = req(&app, "GET", &format!("/api/admin/users?q={}", "x".repeat(101)), Some(&k), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(req(&app, "GET", "/api/admin/users/not-a-uuid", Some(&k), None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(req(&app, "GET", &format!("/api/admin/users/{}", Uuid::new_v4()), Some(&k), None).await.0, StatusCode::NOT_FOUND);
    // update
    let (s, u2) = req(&app, "PUT", &format!("/api/admin/users/{uid}"), Some(&k), Some(json!({"full_name":"Maria Q.","allowed_pages":["qc"],"subscribed_notifications":[]}))).await;
    assert_eq!(s, StatusCode::OK, "{u2}");
    assert_eq!((u2["full_name"].as_str(), &u2["allowed_pages"], &u2["subscribed_notifications"]), (Some("Maria Q."), &json!(["qc"]), &json!([])));
    for body in [json!({"password_hash":"x"}), json!({"username":"other"}), json!({"allowed_pages":["admin"]}), json!({"allowed_pages":["x"]}), json!({"full_name":""})] {
        assert!(!req(&app, "PUT", &format!("/api/admin/users/{uid}"), Some(&k), Some(body.clone())).await.0.is_success(), "{body}");
    }
    // password reset revokes sessions, unlocks
    let ukey = common::session_for(&pool, uid.parse().unwrap()).await;
    sqlx::query("UPDATE users SET failed_login_count=3, locked_until=now()+interval '5 min' WHERE id=$1").bind(uid.parse::<Uuid>().unwrap()).execute(&pool).await.unwrap();
    assert_eq!(req(&app, "GET", "/api/me", Some(&ukey), None).await.0, StatusCode::OK);
    assert_eq!(req(&app, "POST", &format!("/api/admin/users/{uid}/password"), Some(&k), Some(json!({"new_password":"short"}))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(req(&app, "POST", &format!("/api/admin/users/{uid}/password"), Some(&k), Some(json!({"new_password":"maria"}))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(req(&app, "POST", &format!("/api/admin/users/{uid}/password"), Some(&k), Some(json!({"new_password":"Reset-Pass-12345"}))).await.0, StatusCode::NO_CONTENT);
    assert_eq!(req(&app, "GET", "/api/me", Some(&ukey), None).await.0, StatusCode::UNAUTHORIZED, "reset revokes sessions");
    let (s, uu) = req(&app, "GET", &format!("/api/admin/users/{uid}"), Some(&k), None).await;
    assert_eq!((s, uu["is_locked"].as_bool(), uu["failed_login_count"].as_i64()), (StatusCode::OK, Some(false), Some(0)));
    // unlock
    sqlx::query("UPDATE users SET failed_login_count=3, locked_until=now()+interval '5 min' WHERE id=$1").bind(uid.parse::<Uuid>().unwrap()).execute(&pool).await.unwrap();
    let (s, uu) = req(&app, "POST", &format!("/api/admin/users/{uid}/unlock"), Some(&k), None).await;
    assert_eq!((s, uu["is_locked"].as_bool()), (StatusCode::OK, Some(false)));
    // delete = deactivate + revoke; key dead; login blocked
    let ukey = common::session_for(&pool, uid.parse().unwrap()).await;
    assert_eq!(req(&app, "DELETE", &format!("/api/admin/users/{uid}"), Some(&k), None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(req(&app, "GET", "/api/me", Some(&ukey), None).await.0, StatusCode::UNAUTHORIZED);
    let (_, uu) = req(&app, "GET", &format!("/api/admin/users/{uid}"), Some(&k), None).await;
    assert_eq!(uu["is_active"], false);
    // audit rows exist for every mutation and carry the actor
    let acts: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_logs ORDER BY id").fetch_all(&pool).await.unwrap();
    for a in ["USER_CREATE", "USER_UPDATE", "USER_PASSWORD_RESET", "USER_UNLOCK", "USER_DEACTIVATE"] { assert!(acts.iter().any(|x| x == a), "missing audit {a}: {acts:?}"); }
    let who: Option<String> = sqlx::query_scalar("SELECT username FROM audit_logs WHERE action='USER_CREATE'").fetch_one(&pool).await.unwrap();
    assert_eq!(who.as_deref(), Some("root"));
    // audit details never contain secrets
    let all: String = sqlx::query_scalar("SELECT string_agg(details::text, ' ') FROM audit_logs").fetch_one(&pool).await.unwrap();
    assert!(!all.contains(PW) && !all.contains("Reset-Pass") && !all.contains("argon2"));
}

fn urlenc(s: &str) -> String { s.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect() }

#[sqlx::test(migrations = "./migrations")]
async fn last_admin_guard(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (id1, k1) = admin(&pool, "adm1").await;
    // only admin: demote / deactivate / delete all refused
    for (m, body) in [("PUT", Some(json!({"is_admin":false}))), ("PUT", Some(json!({"is_active":false}))), ("DELETE", None)] {
        let (s, b) = req(&app, m, &format!("/api/admin/users/{id1}"), Some(&k1), body).await;
        assert_eq!((s, b["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("LAST_ADMIN")), "{m}");
    }
    // harmless edit is fine
    assert_eq!(req(&app, "PUT", &format!("/api/admin/users/{id1}"), Some(&k1), Some(json!({"full_name":"Boss"}))).await.0, StatusCode::OK);
    // a non-admin does not count as a replacement
    let (_, _) = plain_user(&pool, "pleb").await;
    assert_eq!(req(&app, "PUT", &format!("/api/admin/users/{id1}"), Some(&k1), Some(json!({"is_admin":false}))).await.0, StatusCode::CONFLICT);
    // second admin: demoting the first now works
    let (id2, k2) = admin(&pool, "adm2").await;
    assert_eq!(req(&app, "PUT", &format!("/api/admin/users/{id1}"), Some(&k2), Some(json!({"is_admin":false}))).await.0, StatusCode::OK);
    // ... and the demoted user lost access immediately (live lookup)
    assert_eq!(req(&app, "GET", "/api/admin/users", Some(&k1), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(req(&app, "DELETE", &format!("/api/admin/users/{id2}"), Some(&k2), None).await.0, StatusCode::CONFLICT);
    // an inactive admin does not count either
    sqlx::query("UPDATE users SET is_admin=true, is_active=false WHERE id=$1").bind(id1).execute(&pool).await.unwrap();
    assert_eq!(req(&app, "PUT", &format!("/api/admin/users/{id2}"), Some(&k2), Some(json!({"is_admin":false}))).await.0, StatusCode::CONFLICT);
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_demotions_cannot_remove_every_admin(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (id1, k1) = admin(&pool, "adm1").await;
    let (id2, k2) = admin(&pool, "adm2").await;
    let (a, b) = (app.clone(), app.clone());
    let h1 = tokio::spawn(async move { req(&a, "PUT", &format!("/api/admin/users/{id2}"), Some(&k1), Some(json!({"is_admin":false}))).await.0 });
    let h2 = tokio::spawn(async move { req(&b, "PUT", &format!("/api/admin/users/{id1}"), Some(&k2), Some(json!({"is_admin":false}))).await.0 });
    let (s1, s2) = (h1.await.unwrap(), h2.await.unwrap());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE is_admin AND is_active").fetch_one(&pool).await.unwrap();
    assert!(n >= 1, "statuses {s1} {s2}: at least one admin must remain");
    assert!(s1 == StatusCode::OK || s2 == StatusCode::OK || s1 == StatusCode::CONFLICT);
}

#[sqlx::test(migrations = "./migrations")]
async fn sessions_list_and_revoke(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (_, k) = admin(&pool, "root").await;
    let (uid, uk) = plain_user(&pool, "worker").await;
    let uk2 = common::session_for(&pool, uid).await;
    let (s, l) = req(&app, "GET", "/api/admin/sessions", Some(&k), None).await;
    assert_eq!(s, StatusCode::OK);
    let l = l.as_array().unwrap();
    assert_eq!(l.len(), 3);
    assert_eq!(l.iter().filter(|x| x["is_current"] == true).count(), 1);
    assert!(l.iter().all(|x| x.get("key_hash").is_none() && x["username"].is_string()));
    let sid = l.iter().find(|x| x["username"] == "worker").unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(req(&app, "DELETE", &format!("/api/admin/sessions/{sid}"), Some(&k), None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(req(&app, "DELETE", &format!("/api/admin/sessions/{sid}"), Some(&k), None).await.0, StatusCode::NOT_FOUND);
    let alive = [req(&app, "GET", "/api/me", Some(&uk), None).await.0, req(&app, "GET", "/api/me", Some(&uk2), None).await.0];
    assert!(alive.contains(&StatusCode::UNAUTHORIZED) && alive.contains(&StatusCode::OK), "exactly one worker session revoked: {alive:?}");
    let (s, r) = req(&app, "POST", &format!("/api/admin/users/{uid}/revoke-sessions"), Some(&k), None).await;
    assert_eq!((s, r["revoked"].as_i64()), (StatusCode::OK, Some(1)));
    let _ = common::session_for(&pool, uid).await;
    let (s, r) = req(&app, "POST", "/api/admin/sessions/revoke-all", Some(&k), None).await;
    assert_eq!((s, r["revoked"].as_i64()), (StatusCode::OK, Some(1)));
    assert_eq!(req(&app, "GET", "/api/me", Some(&k), None).await.0, StatusCode::OK, "caller survives revoke-all");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action LIKE 'SESSION%'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 3);
}

#[sqlx::test(migrations = "./migrations")]
async fn settings_validation_is_all_or_nothing_and_live(pool: PgPool) {
    let (app, st) = common::app(pool.clone()).await;
    let (_, k) = admin(&pool, "root").await;
    let (s, l) = req(&app, "GET", "/api/admin/settings", Some(&k), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(l.as_array().unwrap().iter().any(|x| x["key"] == "plant.name" && x["description"].is_string()));
    // one valid + one invalid => nothing applied
    let (s, b) = req(&app, "PUT", "/api/admin/settings", Some(&k), Some(json!({"plant.name":"Changed Name","auth.max_failed_logins":0}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    assert!(b["error"]["fields"]["auth.max_failed_logins"].is_string());
    assert_eq!(st.settings.get().plant_name, "Aqua Pure Plant");
    let name: Value = sqlx::query_scalar("SELECT value FROM system_settings WHERE key='plant.name'").fetch_one(&pool).await.unwrap();
    assert_eq!(name, json!("Aqua Pure Plant"));
    // bad cases
    for (k_, v) in [("nope.key", json!(1)), ("auth.session_ttl_hours", json!("12")), ("auth.session_ttl_hours", json!(99999)), ("auth.lockout_minutes", json!(1.5)),
                    ("features.qr_labels", json!("yes")), ("plant.name", json!("")), ("plant.name", json!("x".repeat(121))), ("plant.code", json!("bad code!")),
                    ("plant.qr_base_url", json!("javascript:alert(1)")), ("plant.qr_base_url", json!("https://evil.example/\"><script>")),
                    ("plant.qr_base_url", json!("https://user:pw@evil.example")), ("lab.ph_max", json!(20)), ("lab.ph_min", json!(9)), ("lab.tds_min", json!(600)),
                    ("plant.utc_offset_minutes", json!(5000)), ("plant.name", json!("tab\u{0007}"))] {
        let (s, b) = req(&app, "PUT", "/api/admin/settings", Some(&k), Some(json!({ k_: v.clone() }))).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{k_}={v}: {b}");
    }
    assert_eq!(req(&app, "PUT", "/api/admin/settings", Some(&k), Some(json!({}))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(req(&app, "PUT", "/api/admin/settings", Some(&k), Some(json!(["x"]))).await.0, StatusCode::BAD_REQUEST);
    // valid multi-update is live immediately
    let (s, b) = req(&app, "PUT", "/api/admin/settings", Some(&k), Some(json!({
        "plant.name":"Crystal Springs","features.qr_labels":true,"auth.max_failed_logins":3,"lab.ph_min":6.0,"lab.ph_max":8.0,"plant.qr_base_url":"https://erp.example.com"}))).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let cur = st.settings.get();
    assert_eq!((cur.plant_name.as_str(), cur.qr_labels_enabled, cur.max_failed_logins), ("Crystal Springs", true, 3));
    let (_, d): (i64, Value) = sqlx::query_as("SELECT count(*), details FROM audit_logs WHERE action='SETTINGS_UPDATE' GROUP BY details").fetch_one(&pool).await.unwrap();
    assert_eq!(d["changes"]["plant.name"]["old"], "Aqua Pure Plant");
}

#[sqlx::test(migrations = "./migrations")]
async fn audit_query_filters_paging_and_immutability(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (_, k) = admin(&pool, "root").await;
    for i in 0..5 {
        let (s, _) = req(&app, "POST", "/api/admin/routes", Some(&k), Some(json!({"name": format!("Route {i}")}))).await;
        assert_eq!(s, StatusCode::CREATED);
    }
    let (s, a) = req(&app, "GET", "/api/admin/audit?action=ROUTE_CREATE&limit=2", Some(&k), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((a["total"].as_i64(), a["items"].as_array().unwrap().len()), (Some(5), 2));
    let row = &a["items"][0];
    for f in ["id", "at", "user_id", "username", "action", "entity", "entity_id", "details", "ip"] { assert!(row.get(f).is_some(), "missing {f}"); }
    let (_, a2) = req(&app, "GET", "/api/admin/audit?action=ROUTE_CREATE&limit=2&offset=4", Some(&k), None).await;
    assert_eq!(a2["items"].as_array().unwrap().len(), 1);
    // filters
    let (_, a) = req(&app, "GET", "/api/admin/audit?user=root&entity=route", Some(&k), None).await;
    assert_eq!(a["total"], 5);
    let (_, a) = req(&app, "GET", "/api/admin/audit?user=nobody", Some(&k), None).await;
    assert_eq!(a["total"], 0);
    let (_, a) = req(&app, "GET", "/api/admin/audit?from=2999-01-01", Some(&k), None).await;
    assert_eq!(a["total"], 0);
    let today = chrono::Utc::now().format("%Y-%m-%d");
    let (_, a) = req(&app, "GET", &format!("/api/admin/audit?from=2000-01-01&to={today}"), Some(&k), None).await;
    assert!(a["total"].as_i64().unwrap() >= 5);
    // hostile params
    for q in ["limit=999999999999", "limit=-5", "offset=-1", "offset=9223372036854775807", "action=%25", "action='%20OR%201=1--", "entity=%5C", "user=%27%3B%20DROP%20TABLE%20audit_logs%3B--"] {
        let (s, b) = req(&app, "GET", &format!("/api/admin/audit?{q}"), Some(&k), None).await;
        assert_eq!(s, StatusCode::OK, "{q}: {b}");
        assert!(b["items"].as_array().unwrap().len() <= 200);
    }
    for q in ["limit=abc", "from=not-a-date", "to=2020-13-45", "offset=1.5"] {
        assert_eq!(req(&app, "GET", &format!("/api/admin/audit?{q}"), Some(&k), None).await.0, StatusCode::BAD_REQUEST, "{q}");
    }
    // immutable
    assert!(sqlx::query("UPDATE audit_logs SET action='X'").execute(&pool).await.is_err());
    assert!(sqlx::query("DELETE FROM audit_logs").execute(&pool).await.is_err());
}
