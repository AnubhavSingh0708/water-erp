mod common;
use axum::{body::Body, extract::ConnectInfo, http::{Request, StatusCode}, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::net::SocketAddr;
use tower::ServiceExt;
use uuid::Uuid;
use water_erp::routes::admin::common::hash_password_blocking;

const PW: &str = "Correct-Horse-9!";

/// Full-control request helper: optional key, body, client address (ConnectInfo) and extra headers.
async fn req(app: &Router, method: &str, uri: &str, key: Option<&str>, body: Option<Value>, ip: Option<&str>, headers: &[(&str, &str)])
    -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(k) = key { b = b.header("x-auth-key", k); }
    for (k, v) in headers { b = b.header(*k, *v); }
    let mut r = if let Some(j) = body { b.header("content-type", "application/json").body(Body::from(j.to_string())).unwrap() } else { b.body(Body::empty()).unwrap() };
    if let Some(ip) = ip { r.extensions_mut().insert(ConnectInfo::<SocketAddr>(format!("{ip}:5555").parse().unwrap())); }
    let resp = app.clone().oneshot(r).await.unwrap();
    let (st, h) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into())), h)
}

async fn user_with_pw(pool: &PgPool, name: &str, admin: bool, pages: &[&str]) -> Uuid {
    let id = common::make_user(pool, name, admin, pages, &["QC_PENDING"]).await;
    let h = hash_password_blocking(PW).unwrap();
    sqlx::query("UPDATE users SET password_hash=$2 WHERE id=$1").bind(id).bind(h).execute(pool).await.unwrap();
    id
}

async fn login(app: &Router, user: &str, pw: &str, ip: &str) -> (StatusCode, Value, axum::http::HeaderMap) {
    req(app, "POST", "/api/auth/login", None, Some(json!({"username": user, "password": pw})), Some(ip), &[]).await
}

#[sqlx::test(migrations = "./migrations")]
async fn login_success_sets_cookie_and_hashes_key(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    user_with_pw(&pool, "alice", false, &["qc", "dashboard"]).await;
    let t = std::time::Instant::now();
    let (st, body, h) = login(&app, "ALICE", PW, "10.1.0.1").await; // case-insensitive username
    eprintln!("login took {:?}", t.elapsed());
    assert_eq!(st, StatusCode::OK, "{body}");
    let key = body["key"].as_str().unwrap();
    assert_eq!(key.len(), 64);
    assert_eq!(body["user"]["username"], "alice");
    assert_eq!(body["user"]["allowed_pages"], json!(["qc", "dashboard"]));
    assert!(body["user"].get("password_hash").is_none());
    assert!(body.to_string().find("argon2").is_none());
    assert_eq!(h.get("set-cookie").unwrap().to_str().unwrap(), format!("erp_key={key}; Secure; SameSite=Strict; Path=/; Max-Age=2592000"));
    // only the hash is stored
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM user_sessions WHERE key_hash = $1").bind(water_erp::auth::hash_key(key)).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM user_sessions WHERE key_hash = $1").bind(key).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
    // /api/me works with and without trailing slash semantics (exact path used by the frontend)
    let (st, me, _) = req(&app, "GET", "/api/me", Some(key), None, None, &[]).await;
    assert_eq!(st, StatusCode::OK, "{me}");
    assert_eq!(me["username"], "alice");
    assert_eq!(me["subscribed_notifications"], json!(["QC_PENDING"]));
    // Bearer works too
    let (st, _, _) = req(&app, "GET", "/api/me", None, None, None, &[("authorization", &format!("Bearer {key}"))]).await;
    assert_eq!(st, StatusCode::OK);
    // audit row, ip recorded
    let ip: Option<String> = sqlx::query_scalar("SELECT ip FROM audit_logs WHERE action='LOGIN'").fetch_one(&pool).await.unwrap();
    assert_eq!(ip.as_deref(), Some("10.1.0.1"));
}

#[sqlx::test(migrations = "./migrations")]
async fn admin_me_lists_all_pages(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    user_with_pw(&pool, "boss", true, &[]).await;
    let (_, body, _) = login(&app, "boss", PW, "10.1.0.2").await;
    assert_eq!(body["user"]["allowed_pages"].as_array().unwrap().len(), water_erp::auth::PAGES.len());
}

#[sqlx::test(migrations = "./migrations")]
async fn no_username_enumeration_including_lockout(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    user_with_pw(&pool, "realuser", false, &[]).await;
    let (s1, b1, _) = login(&app, "realuser", "wrong-password-1", "10.2.0.1").await;
    let (s2, b2, _) = login(&app, "ghostuser-zz", "wrong-password-1", "10.2.0.1").await;
    assert_eq!(s1, StatusCode::UNAUTHORIZED);
    assert_eq!((s1, &b1), (s2, &b2), "same status AND body for known/unknown user");
    assert_eq!(b1["error"]["message"], "Invalid username or password");
    // drive both to lockout: the answers stay identical (429 for both)
    let mut last = (StatusCode::OK, Value::Null, Value::Null);
    for _ in 0..4 {
        let (a, ba, _) = login(&app, "realuser", "wrong-password-1", "10.2.0.1").await;
        let (b, bb, _) = login(&app, "ghostuser-zz", "wrong-password-1", "10.2.0.1").await;
        last = (a, ba, bb.clone());
        assert_eq!(a, b);
        assert_eq!(last.1, bb);
    }
    assert_eq!(last.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(last.1["error"]["code"], "LOCKED");
    // even the correct password is refused while locked, and the known user stays indistinguishable
    let (s, b, _) = login(&app, "realuser", PW, "10.2.0.1").await;
    let (g, gb, _) = login(&app, "ghostuser-zz", PW, "10.2.0.1").await;
    assert_eq!((s, &b), (g, &gb));
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test(migrations = "./migrations")]
async fn lockout_and_unlock(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let id = user_with_pw(&pool, "bob", false, &[]).await;
    for i in 0..5 {
        let (s, _, _) = login(&app, "bob", "nope-nope-nope", "10.3.0.1").await;
        assert_eq!(s, if i < 4 { StatusCode::UNAUTHORIZED } else { StatusCode::TOO_MANY_REQUESTS }, "attempt {i}");
    }
    assert_eq!(login(&app, "bob", PW, "10.3.0.1").await.0, StatusCode::TOO_MANY_REQUESTS);
    let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='LOGIN_LOCKED'").fetch_one(&pool).await.unwrap();
    assert_eq!(audit, 1);
    // lock expires => login works, counter reset
    sqlx::query("UPDATE users SET locked_until = now() - interval '1 minute' WHERE id=$1").bind(id).execute(&pool).await.unwrap();
    assert_eq!(login(&app, "bob", PW, "10.3.0.1").await.0, StatusCode::OK);
    let c: i32 = sqlx::query_scalar("SELECT failed_login_count FROM users WHERE id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(c, 0);
    // success resets the counter (4 fails + success + 4 fails is not a lockout)
    for _ in 0..4 { login(&app, "bob", "nope-nope-nope", "10.3.0.1").await; }
    assert_eq!(login(&app, "bob", PW, "10.3.0.1").await.0, StatusCode::OK);
    for _ in 0..4 { assert_eq!(login(&app, "bob", "nope-nope-nope", "10.3.0.1").await.0, StatusCode::UNAUTHORIZED); }
}

#[sqlx::test(migrations = "./migrations")]
async fn inactive_user_and_bad_hash_get_generic_401(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let id = user_with_pw(&pool, "carol", false, &[]).await;
    sqlx::query("UPDATE users SET is_active=false WHERE id=$1").bind(id).execute(&pool).await.unwrap();
    let (s, b, _) = login(&app, "carol", PW, "10.4.0.1").await;
    assert_eq!((s, b["error"]["message"].as_str()), (StatusCode::UNAUTHORIZED, Some("Invalid username or password")));
    common::make_user(&pool, "dave", false, &[], &[]).await; // password_hash = 'x' (unparseable)
    let (s, _, _) = login(&app, "dave", "whatever-whatever", "10.4.0.1").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn malformed_and_hostile_login_input(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (s, b, _) = req(&app, "POST", "/api/auth/login", None, Some(json!({"username":"a","password":"b","is_admin":true})), Some("10.5.0.1"), &[]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["error"]["code"], "BAD_REQUEST");
    let (s, b, _) = req(&app, "POST", "/api/auth/login", None, Some(json!({"username":"a"})), Some("10.5.0.1"), &[]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST); assert!(b["error"]["code"].is_string());
    // wrong content type / malformed json
    let r = Request::builder().method("POST").uri("/api/auth/login").header("content-type", "text/plain").body(Body::from("{}")).unwrap();
    let resp = app.clone().oneshot(r).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let r = Request::builder().method("POST").uri("/api/auth/login").header("content-type", "application/json").body(Body::from("{not json")).unwrap();
    assert_eq!(app.clone().oneshot(r).await.unwrap().status(), StatusCode::BAD_REQUEST);
    // SQL-injection style + huge + unicode/control input: generic 401, never 500
    for u in ["' OR '1'='1", "admin'--", "a\u{0000}b", "💥💥💥", &"x".repeat(5000)] {
        let (s, _, _) = login(&app, u, "' OR 1=1 --", "10.5.0.2").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "input {u:?}");
    }
    let (s, _, _) = login(&app, "someone", &"p".repeat(100_000), "10.5.0.2").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // empty
    let (s, _, _) = login(&app, "", "x", "10.5.0.2").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrations = "./migrations")]
async fn per_ip_limiter(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let mut codes = vec![];
    for i in 0..32 {
        // unique usernames so the per-account lockout is not what triggers
        let (s, _, _) = req(&app, "POST", "/api/auth/login", None, Some(json!({"username": format!("nobody-{i}"), "password": "x"})), Some("10.6.6.6"), &[]).await;
        codes.push(s);
    }
    assert!(codes[..30].iter().all(|c| *c == StatusCode::UNAUTHORIZED));
    assert_eq!(codes[30], StatusCode::TOO_MANY_REQUESTS);
    // another address is unaffected; X-Forwarded-For is not trusted
    let (s, _, _) = req(&app, "POST", "/api/auth/login", None, Some(json!({"username": "nobody-x", "password": "x"})), Some("10.6.6.7"), &[("x-forwarded-for", "10.6.6.6")]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = req(&app, "POST", "/api/auth/login", None, Some(json!({"username": "nobody-y", "password": "x"})), Some("10.6.6.6"), &[("x-forwarded-for", "9.9.9.9")]).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test(migrations = "./migrations")]
async fn max_ten_sessions_and_truncated_user_agent(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let id = user_with_pw(&pool, "eve", false, &[]).await;
    let ua = format!("Mozilla/{}\u{00e9}\u{1F4A5}", "x".repeat(400));
    let mut keys = vec![];
    for _ in 0..12 {
        let (s, b, _) = req(&app, "POST", "/api/auth/login", None, Some(json!({"username":"eve","password":PW})), Some("10.7.0.1"), &[("user-agent", &ua)]).await;
        assert_eq!(s, StatusCode::OK);
        keys.push(b["key"].as_str().unwrap().to_string());
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM user_sessions WHERE user_id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 10);
    let max_ua: i32 = sqlx::query_scalar("SELECT max(char_length(user_agent)) FROM user_sessions").fetch_one(&pool).await.unwrap();
    assert_eq!(max_ua, 200);
    // the newest key still works, the oldest was evicted
    assert_eq!(req(&app, "GET", "/api/me", Some(&keys[11]), None, None, &[]).await.0, StatusCode::OK);
    assert_eq!(req(&app, "GET", "/api/me", Some(&keys[0]), None, None, &[]).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn bad_keys_are_401(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let id = common::make_user(&pool, "frank", false, &[], &[]).await;
    let key = common::session_for(&pool, id).await;
    assert_eq!(req(&app, "GET", "/api/me", Some(&key), None, None, &[]).await.0, StatusCode::OK);
    assert_eq!(req(&app, "GET", "/api/me", None, None, None, &[]).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(req(&app, "GET", "/api/me", Some("0000"), None, None, &[]).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(req(&app, "GET", "/api/me", Some("' OR 1=1 --"), None, None, &[]).await.0, StatusCode::UNAUTHORIZED);
    // expired
    sqlx::query("UPDATE user_sessions SET expires_at = now() - interval '1 second'").execute(&pool).await.unwrap();
    assert_eq!(req(&app, "GET", "/api/me", Some(&key), None, None, &[]).await.0, StatusCode::UNAUTHORIZED);
    // revoked
    let key2 = common::session_for(&pool, id).await;
    assert_eq!(req(&app, "GET", "/api/me", Some(&key2), None, None, &[]).await.0, StatusCode::OK);
    sqlx::query("DELETE FROM user_sessions").execute(&pool).await.unwrap();
    assert_eq!(req(&app, "GET", "/api/me", Some(&key2), None, None, &[]).await.0, StatusCode::UNAUTHORIZED);
    // deactivated user
    let key3 = common::session_for(&pool, id).await;
    sqlx::query("UPDATE users SET is_active=false WHERE id=$1").bind(id).execute(&pool).await.unwrap();
    assert_eq!(req(&app, "GET", "/api/me", Some(&key3), None, None, &[]).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn sliding_session_refresh_is_throttled(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let id = common::make_user(&pool, "gina", false, &[], &[]).await;
    let key = common::session_for(&pool, id).await;
    // backdate: last used 10 min ago, expiring in 1 h
    sqlx::query("UPDATE user_sessions SET last_used_at = now() - interval '10 minutes', expires_at = now() + interval '1 hour'").execute(&pool).await.unwrap();
    let (before_used, before_exp): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT last_used_at, expires_at FROM user_sessions").fetch_one(&pool).await.unwrap();
    assert_eq!(req(&app, "GET", "/api/me", Some(&key), None, None, &[]).await.0, StatusCode::OK);
    let (used1, exp1): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT last_used_at, expires_at FROM user_sessions").fetch_one(&pool).await.unwrap();
    assert!(used1 > before_used + chrono::Duration::minutes(9), "last_used_at moved");
    assert!(exp1 > before_exp + chrono::Duration::hours(10), "expires_at slid to ~now+12h");
    // immediate second hit: throttled (within 60 s) => unchanged
    assert_eq!(req(&app, "GET", "/api/me", Some(&key), None, None, &[]).await.0, StatusCode::OK);
    let (used2, exp2): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT last_used_at, expires_at FROM user_sessions").fetch_one(&pool).await.unwrap();
    assert_eq!((used1, exp1), (used2, exp2), "no DB write within the 60 s throttle");
}

#[sqlx::test(migrations = "./migrations")]
async fn logout_clears_cookie_and_revokes(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    user_with_pw(&pool, "hank", false, &[]).await;
    let (_, b, _) = login(&app, "hank", PW, "10.8.0.1").await;
    let key = b["key"].as_str().unwrap().to_string();
    let (s, _, h) = req(&app, "POST", "/api/me/logout", Some(&key), None, None, &[]).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(h.get("set-cookie").unwrap().to_str().unwrap().contains("Max-Age=0"));
    assert_eq!(req(&app, "GET", "/api/me", Some(&key), None, None, &[]).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "./migrations")]
async fn password_change_revokes_other_sessions(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    user_with_pw(&pool, "iris", false, &[]).await;
    let (_, b1, _) = login(&app, "iris", PW, "10.9.0.1").await;
    let (_, b2, _) = login(&app, "iris", PW, "10.9.0.1").await;
    let (k1, k2) = (b1["key"].as_str().unwrap().to_string(), b2["key"].as_str().unwrap().to_string());
    // wrong current password
    let (s, b, _) = req(&app, "POST", "/api/me/password", Some(&k1), Some(json!({"current_password":"bad-bad-bad-1","new_password":"Brand-New-Pass-1"})), None, &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    // policy: too short / equals username
    for np in ["short", "iris"] {
        let (s, _, _) = req(&app, "POST", "/api/me/password", Some(&k1), Some(json!({"current_password":PW,"new_password":np})), None, &[]).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{np}");
    }
    let (s, _, _) = req(&app, "POST", "/api/me/password", Some(&k1), Some(json!({"current_password":PW,"new_password":PW})), None, &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, b, _) = req(&app, "POST", "/api/me/password", Some(&k1), Some(json!({"current_password":PW,"new_password":"Brand-New-Pass-1"})), None, &[]).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    assert_eq!(req(&app, "GET", "/api/me", Some(&k1), None, None, &[]).await.0, StatusCode::OK, "current session survives");
    assert_eq!(req(&app, "GET", "/api/me", Some(&k2), None, None, &[]).await.0, StatusCode::UNAUTHORIZED, "other sessions revoked");
    assert_eq!(login(&app, "iris", PW, "10.9.0.1").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(login(&app, "iris", "Brand-New-Pass-1", "10.9.0.1").await.0, StatusCode::OK);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='PASSWORD_CHANGE'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn setup_flow_and_disabled_afterwards(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let (s, b, _) = req(&app, "GET", "/api/setup/status", None, None, None, &[]).await;
    assert_eq!((s, &b["setup_required"]), (StatusCode::OK, &json!(true)));
    // policy enforced
    let (s, _, _) = req(&app, "POST", "/api/setup/admin", None, Some(json!({"username":"root","full_name":"Root","password":"short"})), Some("10.10.0.1"), &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = req(&app, "POST", "/api/setup/admin", None, Some(json!({"username":"bad name!","full_name":"Root","password":PW})), Some("10.10.0.1"), &[]).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, b, h) = req(&app, "POST", "/api/setup/admin", None, Some(json!({"username":"root","full_name":"Root","password":PW})), Some("10.10.0.1"), &[]).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["user"]["is_admin"], true);
    assert!(h.get("set-cookie").is_some());
    let key = b["key"].as_str().unwrap().to_string();
    assert_eq!(req(&app, "GET", "/api/me", Some(&key), None, None, &[]).await.0, StatusCode::OK);
    let (_, b, _) = req(&app, "GET", "/api/setup/status", None, None, None, &[]).await;
    assert_eq!(b["setup_required"], false);
    let (s, b, _) = req(&app, "POST", "/api/setup/admin", None, Some(json!({"username":"root2","full_name":"Root2","password":PW})), Some("10.10.0.1"), &[]).await;
    assert_eq!((s, b["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("SETUP_DISABLED")));
    // stays disabled even if the only admin is deactivated
    sqlx::query("UPDATE users SET is_active=false").execute(&pool).await.unwrap();
    let (s, _, _) = req(&app, "POST", "/api/setup/admin", None, Some(json!({"username":"root3","full_name":"Root3","password":PW})), Some("10.10.0.1"), &[]).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE is_admin").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_setup_creates_exactly_one_admin(pool: PgPool) {
    let (app, _) = common::app(pool.clone()).await;
    let mut hs = vec![];
    for i in 0..6 {
        let app = app.clone();
        hs.push(tokio::spawn(async move {
            req(&app, "POST", "/api/setup/admin", None, Some(json!({"username": format!("racer{i}"), "full_name": "R", "password": PW})), Some(&format!("10.11.0.{}", i + 1)), &[]).await.0
        }));
    }
    let mut codes = vec![];
    for h in hs { codes.push(h.await.unwrap()); }
    assert_eq!(codes.iter().filter(|c| **c == StatusCode::CREATED).count(), 1, "{codes:?}");
    assert!(codes.iter().filter(|c| **c != StatusCode::CREATED).all(|c| *c == StatusCode::CONFLICT), "{codes:?}");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE is_admin").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn bootstrap_admin_is_idempotent_and_never_overwrites(pool: PgPool) {
    water_erp::routes::admin::bootstrap_admin(&pool, "firstadmin", PW).await.unwrap();
    water_erp::routes::admin::bootstrap_admin(&pool, "otheradmin", PW).await.unwrap(); // admin exists => no-op
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    assert!(water_erp::routes::admin::bootstrap_admin(&pool, "x", PW).await.is_err() || n == 1);
    let (app, _) = common::app(pool.clone()).await;
    assert_eq!(login(&app, "firstadmin", PW, "10.12.0.1").await.0, StatusCode::OK);
}
