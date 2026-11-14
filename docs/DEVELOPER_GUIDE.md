# Developer Guide – Neersagar ERP

Packaged mineral-water plant ERP: raw materials → blow molding → bottling → Lab QC → warehouse → loading/dispatch → jars/maintenance.
**Out of scope on purpose:** GST/tax, invoicing, payments, accounting.

Stack: Rust (tokio, axum 0.8, sqlx/PostgreSQL, rustls – **no OpenSSL**: `cargo tree -i openssl` must print nothing),
static HTML/CSS/vanilla-JS PWA in `static/` (no CDNs, no inline scripts – CSP is `script-src 'self'`).

## 1. Quick start (localhost testing)
```bash
# 1. PostgreSQL 16 running locally. Create dev roles/databases (dev only – superuser role for tests):
sudo -u postgres psql -f db/01_dev_database.sql
export DATABASE_URL=postgres://erp_dev:erp_dev@127.0.0.1/water_erp_dev

# 2. Run (plain HTTP on 127.0.0.1 only; browsers treat http://localhost as a secure context, so cookie/camera/service worker work)
cargo run -- --localhost --bind 127.0.0.1:8443 --setup-admin admin     # needs ERP_ADMIN_PASSWORD in the env
cargo run -- seed-demo                                                    # optional demo data, users all use Demo@12345!
# open http://localhost:8443
```
Migrations in `migrations/` are embedded in the binary and applied on every start (sqlx tracks versions). The first Admin is
created by `--setup-admin <user>` (+ env `ERP_ADMIN_PASSWORD`) or by the one-time `/setup.html` screen, which disables itself
permanently once an Admin exists.

### Server flags (`src/config.rs`)
| Flag / env | Meaning |
|---|---|
| `--mode local\|domain` (`ERP_MODE`) | `local` = plant LAN, TLS auto-generated (local CA, install `data/tls/local-ca.pem` on phones) unless `--tls-cert/--tls-key`. `domain` = public HTTPS, needs `--domain --tls-cert --tls-key`. |
| `--localhost` | testing: bind 127.0.0.1, mode local, plain HTTP. Not allowed with `--mode domain`. |
| `--domain` | public host (domain) / LAN host-or-IP override (local): used for QR URLs, VAPID subject, Origin checks |
| `--bind`, `--database-url`, `--tls-cert`, `--tls-key`, `--static-dir`, `--data-dir` | as named |
| `--vapid-*`, `water-erp gen-vapid` | Web Push keys (generate once, keep in the env file) |
| `--setup-admin <user>` | bootstrap first Admin (password via `ERP_ADMIN_PASSWORD`, never a flag) |

Runtime settings (plant name, session TTL, burn-rate window, thresholds, QR feature on/off, time-zone offset…) live in the
`system_settings` table and are edited live in **Admin → Settings** (no restart). Defaults are seeded by the migrations.

## 2. Database
* `db/00_create_role_and_database.sql` – production role + database (non-superuser). `db/01_dev_database.sql` – dev/test only.
* `migrations/NNNN_*.sql` – the schema (append-only history, never edit an applied migration; add a new file).
  `db/schema_full.sql` is the concatenation for reading (`scripts/build-schema.sh` regenerates it – commit the result).
* Backup: `scripts/backup.sh` (+ `deploy/water-erp-backup.timer`). Restore: `pg_restore -d <newdb> file.dump`.
* Core invariants (all enforced in the DB, tested): `stock_balances.qty >= 0`; `stock_ledger` and `audit_logs` are append-only
  (trigger); per-(item,zone,lot) `balance == SUM(ledger deltas)`.

## 3. Architecture map
```
src/main.rs        process: parse flags → connect DB (retry) → migrate → settings → workers → serve → graceful shutdown
src/config.rs      CLI/env flags                       src/settings.rs   typed live settings (DB-backed)
src/auth.rs        key hashing, auth middleware, page_guard/admin_guard
src/stock.rs       THE ONLY way to change stock (ledger + balances + FEFO + non-negative)
src/events.rs      realtime SSE (pg_notify triggers → /api/events?page=…)
src/alerts.rs      alert outbox + notification event types   src/validate.rs / util.rs   shared input rules, doc numbers, plant_today
src/routes/*.rs    one file per URL prefix (see API_REFERENCE.md)    src/workers/*.rs   background loops
tests/             integration tests (#[sqlx::test]) + tests/common helpers
static/            the PWA (css/, js/, *.html, sw.js, manifest.json)
```
Process model: one binary, one Tokio runtime. Background workers (spawned in `workers::spawn_all`): realtime listener, session purge,
reorder/low-stock/expiry engine, maintenance + jar-overdue alerts, Web Push dispatcher. Each loop `select!`s on
`state.shutdown.wait()`. **Shutdown:** SIGTERM/SIGINT → `/api/health` 503 → SSE `server_restarting` → in-flight requests drain (≤ 25 s)
→ workers stop → DB pool closes. systemd: `deploy/water-erp.service` (`TimeoutStopSec=40`).

## 4. Rules for changing code
1. **Stock only through `stock.rs`.** Multi-row changes in one transaction; use `post_all`/`transfer` (stable lock order).
2. **Decide inside the transaction.** Lock the gating row (`FOR UPDATE`) and re-check status (QC, set, trip, capacity) *in the tx*.
3. **Corrections are documents, not edits** – void/reverse with reason + audit (see API_REFERENCE §0.2). Never `UPDATE`/`DELETE` ledger rows.
4. Request structs `#[serde(deny_unknown_fields)]`; validate with `validate.rs`; bind every SQL parameter; whitelist ORDER BY.
5. Suspicious entries → `AppError::confirm(..)` (client re-sends with `confirm:true`); support `?dry_run=1` on stock-affecting POSTs.
6. Business dates via `util::plant_today`. Quantities are `rust_decimal::Decimal` (JSON strings) – never f64.
7. Every irreversible action calls `audit::log`. Errors never leak SQL; add a hint in `error::hint_for` for new error codes.
8. New table that the UI watches → add a trigger line in a new migration using `erp_notify('<topic>')` and map the topic in `events.rs`.

### Recipes
* **Add an endpoint:** handler in `routes/<area>.rs` → route in that module's `router()`; the page guard comes from `routes/mod.rs`; document it in `API_REFERENCE.md`; add a test (success, 403 for a user without the page, validation, concurrency if it moves stock).
* **Add a page:** key in `auth::PAGES` + guard line in `routes/mod.rs` + `page_topics` row in `events.rs` + `static/<page>.html` + nav entry in `js/nav.js`.
* **Add a setting:** migration `INSERT INTO system_settings`, field in `settings.rs` (+ `SCHEMA` validation row), Admin UI picks it up automatically.
* **Add an alert/notification type:** `alerts::EVENT_TYPES`, raise via `alerts::raise` (deduped) or `notify_once`; users subscribe in Profile.

## 5. Tests
```bash
export DATABASE_URL=postgres://erp_dev:erp_dev@127.0.0.1/water_erp_test   # role must be able to CREATE DATABASE
cargo test                       # unit + integration (each #[sqlx::test] gets its own throw-away database)
cargo tree -i openssl            # must print: package ID specification `openssl` did not match any packages
```
Parallel test runs against one server are fine; parallel *agents* should use separate Postgres clusters.

## 6. Production deployment (summary)
1. Provision DB (`db/00_…sql`), put secrets in `/etc/water-erp/env` (template `deploy/env.example`, `chmod 600`).
2. `cargo build --release`; install binary + `static/` + `scripts/` under `/opt/water-erp`, user `erp`.
3. `deploy/water-erp.service` (+ backup timer) → `systemctl enable --now water-erp`.
4. First start with `--setup-admin <name>` (or open `/setup.html` once). Check `/api/health`.
5. Phones (local mode): install `local-ca.pem`, open the LAN URL, "Add to Home screen".
6. Upgrades: build → `systemctl restart water-erp` (graceful; migrations run on boot; take a backup first).

## 7. Security model (what is enforced where)
Argon2id passwords + lockout · random 32-byte session keys stored only as SHA-256 · sliding expiry + purge worker ·
`X-Auth-Key` header auth (CSRF-immune) + Origin check · per-user `allowed_pages` enforced in routing (403) and UI ·
Admin-only `/api/admin/*` · CSP `script-src 'self'`, nosniff, frame-deny, HSTS (domain mode) · SSRF-safe Web Push endpoints ·
idempotent writes · immutable audit log · QC quarantine hard-lock enforced inside the loading/dispatch transactions.
