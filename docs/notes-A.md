# Notes from Subagent A (auth / admin / infra / seed)

## Requests for Lead-owned files
1. **Client IP (`src/main.rs`)**: serve with `app.into_make_service_with_connect_info::<std::net::SocketAddr>()` on the TLS path and
   `axum::serve(l, app.into_make_service_with_connect_info::<SocketAddr>())` on the plain-HTTP path. Auth routes read
   `ConnectInfo<SocketAddr>` *optionally* from request extensions; without it `user_sessions.ip`/`audit_logs.ip` are NULL and the
   per-IP login limiter is skipped (per-account lockout still works). `X-Forwarded-For` is never trusted.
2. **JSON/Query/Path rejections** (`ApiJson`, `ApiQuery`, `ApiPath` live in `src/routes/admin_common.rs`): axum's stock extractors answer
   plain-text 4xx outside the error envelope. Other modules should reuse these (or Lead lifts them into shared code).
3. **`settings::update` is not all-or-nothing and `SCHEMA` is private.** Admin PUT validates the whole body up-front with a mirror of
   the schema (plus extra rules: qr_base_url shape, name/code charset, ph/tds min<max), applies key by key under an advisory lock and
   restores the snapshot if anything fails. Preferable: `settings::validate(key,&Value)` and a tx variant of `update`.
4. **Argon2 in debug builds is slow**; consider `[profile.dev.package.argon2] opt-level = 3` (Cargo.toml is Lead-owned).
