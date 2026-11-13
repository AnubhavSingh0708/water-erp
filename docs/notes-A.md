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
5. **`announce_setup_token`** exists in `routes/auth_routes.rs` (`pub async fn announce_setup_token(&PgPool)`): call it from `main.rs` after
   migrations. Also generated lazily (fail closed) by `GET /api/setup/status`. Token is keyed per database name internally.
   `GET /api/setup/status` -> `{setup_required, token_required}`; `POST /api/setup/admin` needs `setup_token` (403 otherwise).
6. CLI: `water-erp --database-url ... seed-demo` (global option must precede the subcommand, or use `DATABASE_URL`).
7. Test hint: low disk can kill the Postgres test cluster; use `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0` to keep target dirs small.

## Deviations / decisions
* Request bodies are strict (`deny_unknown_fields`): a frontend must not send `id`, `created_at`, `*_name` display fields back in PUT bodies.
* Master-data PUT is partial (absent = unchanged, `null` clears nullable fields). BOM PUT accepts a bare array, `{lines:[..]}` or `{items:[..]}`.
* `allowed_pages` may not contain `admin` (administration is the `is_admin` role). Admin `Me.allowed_pages` always lists all pages.
* DELETE on users/master data answers `204`. Master-data lists are bare arrays (<=500 rows, `?q=&active=&limit=&offset=`).
* Argon2id m=19 MiB, t=2, p=1; hashing/verifying in `spawn_blocking` behind a 4-permit semaphore.
* Unknown usernames lock out exactly like real accounts (in-memory strikes) so 429 does not reveal account existence.
* Session `ip` / per-IP limiter need `ConnectInfo` (see request 1); until then `ip` is NULL and the IP limiter is inactive.
* Settings PUT is all-or-nothing via up-front validation + snapshot restore (not a single SQL transaction).
* Admin CRUD of `raw-materials` refuses category changes while the material is used (BOM, product mapping or stock).
