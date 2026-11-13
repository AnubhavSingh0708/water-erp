mod common;
use sqlx::PgPool;
use water_erp::workers::session_purge::purge_once;

#[sqlx::test(migrations = "./migrations")]
async fn purge_removes_expired_and_untouched_keeps_active(pool: PgPool) {
    let u = common::make_user(&pool, "p", false, &[], &[]).await;
    let active = common::session_for(&pool, u).await;
    let expired = common::session_for(&pool, u).await;
    let untouched = common::session_for(&pool, u).await;
    let borderline = common::session_for(&pool, u).await;
    use water_erp::auth::hash_key;
    sqlx::query("UPDATE user_sessions SET expires_at = now() - interval '1 minute' WHERE key_hash = $1").bind(hash_key(&expired)).execute(&pool).await.unwrap();
    sqlx::query("UPDATE user_sessions SET last_used_at = now() - interval '8 days' WHERE key_hash = $1").bind(hash_key(&untouched)).execute(&pool).await.unwrap();
    sqlx::query("UPDATE user_sessions SET last_used_at = now() - interval '6 days' WHERE key_hash = $1").bind(hash_key(&borderline)).execute(&pool).await.unwrap();
    assert_eq!(purge_once(&pool, 7).await.unwrap(), 2);
    let left: Vec<String> = sqlx::query_scalar("SELECT key_hash FROM user_sessions").fetch_all(&pool).await.unwrap();
    assert_eq!(left.len(), 2);
    assert!(left.contains(&hash_key(&active)) && left.contains(&hash_key(&borderline)));
    // idempotent; smaller window now catches the borderline one
    assert_eq!(purge_once(&pool, 7).await.unwrap(), 0);
    assert_eq!(purge_once(&pool, 5).await.unwrap(), 1);
    // absurd values are clamped, never an error
    assert_eq!(purge_once(&pool, -3).await.unwrap(), 0);
    assert_eq!(purge_once(&pool, i64::MAX).await.unwrap(), 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn worker_runs_once_at_start_and_stops_on_shutdown(pool: PgPool) {
    let u = common::make_user(&pool, "p", false, &[], &[]).await;
    let k = common::session_for(&pool, u).await;
    sqlx::query("UPDATE user_sessions SET last_used_at = now() - interval '30 days'").execute(&pool).await.unwrap();
    let st = common::state(pool.clone()).await;
    water_erp::workers::session_purge::spawn(st.clone());
    let _ = k;
    let mut gone = false;
    for _ in 0..50 {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM user_sessions").fetch_one(&pool).await.unwrap();
        if n == 0 { gone = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(gone, "worker purges immediately at start");
    st.shutdown.trigger();
}
