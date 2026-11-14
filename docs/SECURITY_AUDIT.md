# Security & logic audit log

Status legend: ✅ verified by test/probe · 🔧 fixed · ⏳ to verify in the QA phase (after merge).

## Shared layer (Lead) – audited
| Area | Result |
|---|---|
| Static file path traversal (`/../`, `%2e%2e`, `..%2f`, NUL) | ✅ 404 for all |
| Cross-origin state-changing request (`Origin` ≠ `Host`/domain) | ✅ 403 (test + live probe) |
| Auth: only SHA-256 of key stored; expired/revoked/deactivated → 401; sliding refresh throttled to 60 s | ✅ |
| Security headers (CSP incl. `base-uri 'none'`, `object-src 'none'`, `form-action 'self'`; nosniff; frame deny; HSTS in TLS mode; COOP) | ✅ 🔧 (added base-uri/form-action/object-src) |
| SSE: topics filtered by permission; **open streams followed permission changes only on reconnect** | 🔧 permissions re-read on `users` events and every 30 s; test added |
| SSE: stream outlives revoked session / deactivated user | ✅ ends with `session_ended` |
| SSE: connection exhaustion | ✅ ≤ 8 streams/user, 500 total |
| Idempotency: replay of a *different* request under the same key; replay of `CONFIRM_REQUIRED`; dry-run poisoning; stuck key after timeout | 🔧 fixed (fingerprint of method+path+query+body, only 2xx stored, dry-run bypass, 35 s stale window) – tests |
| Ledger/audit immutability, non-negative balances, 20 racing deductions | ✅ |
| Reservation-aware availability (`fg_free`, `allocate_free_fefo`) | ✅ tests; callers ⏳ |
| Graceful shutdown (health 503, streams told to reconnect, pool closed); **event listener kept reconnecting after shutdown** | ✅ 🔧 |
| Known/accepted: session key is JS-readable by requirement → XSS would expose it; mitigated by strict CSP, no inline scripts, output encoding rules (frontend contract §14) | accepted |
| Known/accepted: login lockout can be triggered by an attacker against a known username (temporary, Admin can unlock) | accepted |
| Idempotent POST responses > 512 KB cannot be stored | accepted (POST bodies are small) |

## Requirements pushed to agents from the audit
* A: Argon2 in `spawn_blocking` with a semaphore (runtime-starvation DoS) · **one-time first-run setup token** (LAN claim-the-admin-account attack) · per-IP login limiter.
* B: production quality gate (stale/out-of-range water log), batch-row lock before reading reservations in write-offs/adjustments.

## Pen-test matrix to execute in QA (after merge)
1. AuthN: brute force/lockout, key tampering/truncation, session fixation (new key per login), logout/revoke races, password change revokes others, setup token.
2. AuthZ: every route × {no key, key without page, other page, non-admin on /admin, admin}; page matrix; SSE page param.
3. Injection: SQL (`'`, `;--`, unicode, huge `limit/offset`, `ORDER BY` params), CSV injection, JSON type confusion, unknown fields, NaN/negative/over-precision decimals, 1e9+ quantities.
4. Logic: QC bypass (adjust/transfer/write-off into FG_READY; load QUARANTINE; approve twice; reject then approve), negative stock under 50 parallel requests, same crates loaded by two trips/dispatch/jar-issue, double gate pass, settle twice, settle arithmetic abuse, void abuse (after settle, outside window, non-admin), count post after balances moved, GRN consumed-then-void, duplicate challan, PO over-receipt, idempotency replay across users.
5. Resource abuse: oversized bodies, 10k-item arrays, SSE cap, slow-loris (timeout), pagination caps.
6. Push: SSRF endpoint forms (IP literals, decimal/hex, IPv6, redirects, userinfo), delivery only to subscribed users.
7. Frontend: XSS via every user-controlled string (names, notes, batch numbers, customer names), CSP violations, service worker never caches `/api/*`, draft storage contains no secrets.

---
## QA / pen-test phase – findings log (newest at the bottom)
Legend: ✅ verified, nothing to fix · 🔧 bug found and fixed (regression test named) · ❌ open · ⚠ needs decision.

| # | Area | Sev | Result |
|---|---|---|---|
| 1 | Route inventory vs contract | low | 187 (method,path) routes. 🔧 contract lacked `GET /warehouse/write-offs/{id}`, `GET /raw-materials/supplier-returns/{id}`, the per-entity `/api/admin/<entity>/{id}` routes; §11 still listed a removed `POST /jars/write-off`. `tests/pentest_authz.rs::route_table_matches_the_source` (source scan == table) and `every_route_is_documented_in_the_contract` keep it that way. |
| 2 | Authorization matrix | – | ✅ `authorization_matrix`: 187 routes, 1545 cells {no key, garbage key, garbage Bearer, expired key, deactivated user, no pages, other page, each right page, non-admin with every page on /api/admin, admin}. No holes. `sse_page_matrix` covers `/api/events?page=` incl. unknown page / topics abuse. |
| 3 | `?dry_run=yes` / `True` / `on` **committed** on every owner-B endpoint (adjustments, transfers, write-offs, counts, GRN, batches ...) while owner-C treated the same value as a dry run; middleware used a third rule (`?dry_run=0` skipped idempotency) | **high** (a "review sheet" call could post real stock) | 🔧 one predicate `util::dry_value` / `dry_run_in_query` (present and not `0/false/no/off`; unknown spelling = dry run, fail safe; percent-decoded) used by B, C1, C2 and the idempotency middleware. `tests/pentest_idempotency.rs::{dry_run_predicate_table, middleware_agrees_with_the_handlers_about_dry_run, unusual_dry_run_spellings_never_commit}` |
| 4 | Idempotency: a committed 2xx whose body exceeds 512 KB became `500` (and the key stayed `IN_PROGRESS` 35 s, then the write could run twice); a failing "remember response" UPDATE also turned a committed write into 503 | medium | 🔧 response is returned in full; stored as `{"idempotent_replay_truncated":true,...}` when > 512 KB; storage failure is logged, not surfaced. `oversized_success_response_is_returned_and_never_re_executed` |
| 5 | QR module was not godown-aware: a user restricted to godown A could read QR sets / sheet / lookup / SVG of godown B's batches, list them in `GET /qr/sets`, generate labels for B's batch and **DELETE B's labels** | medium (QR feature is off by default) | 🔧 every QR read/write checks the batch's godown (403), search is scoped. `tests/pentest_godown_ids.rs::foreign_godown_documents_are_unreachable_by_id` (36 by-id probes + 27 list endpoints grepped for B's ids) |
| 6 | QR label create/delete with the feature ON need `qr-labels`, not just `loading` | – | ✅ `qr_label_writes_need_the_qr_labels_page` |
| 7 | Admin could flip a product between BOTTLED/RETURNABLE after jars were received (no batch yet), customer jar holdings, fleet events, trip items or dispatch lines existed (only batches were checked) | medium | 🔧 `product_kind_is_frozen_once_stock_exists` |
| 8 | Deactivating a godown with LOADING/DISPATCHED trips: settlement / gate-pass void restock into the trip's godown without re-validating it → stock lands in an unselectable godown | low | 🔧 deactivation refused (409 IN_USE, `open_trips`). `godown_with_open_trip_cannot_be_deactivated` |
| 9 | Admin password reset revokes the target's sessions | – | ✅ (already: `USER_PASSWORD_RESET` deletes sessions) |
| 10 | AuthN live probes (lockout via case/space/NUL/unicode variants, unknown-user vs wrong-password timing 27 ms vs 30 ms, 100 k-char password rejected without hashing, X-Auth-Key beats Bearer, tampered/truncated/uppercased keys, cookie flags `Secure; SameSite=Strict`, `Cache-Control: no-store` on login) | – | ✅ plus existing `tests/auth_flow.rs` (setup token, first-run race, per-IP limiter, session purge, password change revokes others) |
