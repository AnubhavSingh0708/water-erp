# Security & logic audit log

Status legend: ✅ verified by test/probe · 🔧 fixed · ⏳ = not yet verified (none left after the QA phase, see the log below).

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
| Reservation-aware availability (`fg_free`, `allocate_free_fefo`) | ✅ tests; callers ✅ (dispatch, jar issue, write-off, scrap transfer, negative adjustment, count post, godown transfer, trip items, gate pass – `pentest_fuzz` asserts Σ LOADING reservations ≤ FG_READY after hundreds of concurrent mixed operations; `dispatch_state_machine_and_reservations`) |
| Graceful shutdown (health 503, streams told to reconnect, pool closed); **event listener kept reconnecting after shutdown** | ✅ 🔧 |
| Known/accepted: session key is JS-readable by requirement → XSS would expose it; mitigated by strict CSP, no inline scripts, output encoding rules (frontend contract §14) | accepted |
| Known/accepted: login lockout can be triggered by an attacker against a known username (temporary, Admin can unlock) | accepted |
| Idempotent POST responses > 512 KB cannot be stored | accepted (POST bodies are small) |

## Requirements pushed to agents from the audit
* A: Argon2 in `spawn_blocking` with a semaphore (runtime-starvation DoS) · **one-time first-run setup token** (LAN claim-the-admin-account attack) · per-IP login limiter.
* B: production quality gate (stale/out-of-range water log), batch-row lock before reading reservations in write-offs/adjustments.

## Pen-test matrix – results (QA phase)
1. AuthN – ✅ (all items, see log #10 and `tests/auth_flow.rs`; setup token live-tested over TLS: missing/wrong/truncated token 403, 8 concurrent valid setups -> exactly one 201, replay 409 SETUP_DISABLED, token logged exactly once, no password/key in the log).
2. AuthZ – ✅ `tests/pentest_authz.rs` (1 545 cells) + `pentest_godown_ids.rs` (by-id and list leaks across godowns; one bug fixed, #5).
3. Injection / parsing – ✅ 21 970 hostile GET probes (SQL metacharacters, `pg_sleep`, NUL, 150-char, overflow numbers in every param of every GET route) and 1 368 hostile body probes (empty/null/array/NaN/duplicate keys/5 000-deep nesting/invalid UTF-8/600 KB strings/50 000-element arrays/type confusion/wrong content type) on every POST/PUT: **no 5xx, no slow query**. Numeric table (`1e9`, `NaN`, fullwidth/Arabic digits, `0x10`, `1_000`, over-precision, i64 overflow) behaves; CSV formula cells are prefixed; static traversal (17 encodings) all 404; fixed: bidi/BOM spoofing characters in free text (#14), NUMERIC overflow was a 500 (#13).
4. Logic – ✅/🔧 `pentest_states_*.rs` (every state x action cell of trips, dispatch notes, QC, transfers, counts, GRN, PO, one-shot settle/void, void window, over-receipt, duplicate challan, QC bypass attempts) and `pentest_fuzz.rs` (see below).
5. Resource abuse – see #15-#18.
6. Push SSRF – ✅ `push_endpoint_validator_table` (about 60 hostile endpoint forms + 23 address classes).
7. Frontend – owned by the static/ agent (not part of this pass).

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
| 11 | Randomized concurrent workload (`tests/pentest_fuzz.rs`): 8 workers x 60-250 ops, 3 users (admin / MAIN-only / G2-only), 3 godowns, valid + invalid inputs (0, negative, 5e6, "x", 2.5, foreign ids), double-submits, concurrent approve x2, strict and non-strict QC, seeds 1-33 | – | ✅ no 5xx, `balance == Σ ledger` per (location,item,zone,lot), no negative balance, TRANSIT == open transfers, Σ LOADING reservations ≤ FG_READY, per-trip ledger nets to exactly `-loaded` (DISPATCHED) / `-delivered` (SETTLED) / 0 (LOADING, CANCELLED, voided), per-dispatch-note net, settled items d+r+x == loaded, settled jar ledger == delivered containers, jar holdings >= 0, zones agree with QC status, no stuck idempotency key. Dry runs (dispatch, adjustment, jar receive, batch, GRN) leave **no** ledger / audit / alert / jar / fleet rows. `FUZZ_SEED / FUZZ_WORKERS / FUZZ_OPS / FUZZ_NONSTRICT=1` reproduce and scale it. |
| 12 | Duplicate GRN challan race, PO over-receipt race, supplier-return over-return race, GRN void vs consumption | – | ✅ unique index `grns_challan_uq` + PO/PO-line row locks + stock row locks; `transfer_count_grn_po_state_machines` |
| 13 | A balance beyond NUMERIC(18,4) (SQLSTATE 22003) surfaced as `500 INTERNAL` | low | 🔧 mapped to `400 BAD_REQUEST`; `numeric_overflow_is_a_client_error` |
| 14 | Names / notes / reasons accepted U+202E (RTL override), U+2066-69 and U+FEFF: display spoofing in lists and printouts | low | 🔧 rejected by `validate::text` (ZWJ/ZWNJ stay legal for Indic scripts). `free_text_rejects_nul_controls_and_bidi_spoofing_but_keeps_indic_joiners` |
| 15 | SSE: 1 040 open/close cycles, cap of 8 per user enforced, fds and RSS back to baseline | – | ✅ |
| 16 | Graceful shutdown: SIGTERM with 10 writers posting GRNs: exit in 2.2 s, 693 committed, 0 half-applied GRNs (every GRN has lines and ledger rows), ledger == balances, no stuck idempotency key | – | ✅ |
| 17 | 300 concurrent heavy GETs: 300x200 in 0.8 s; 200 concurrent bad logins: 29x401 + 171x429, `/api/health` stayed at 5-10 ms; 3 MB body -> 400; `limit=1e8` clamps to 200; CSV export capped | – | ✅ |
| 18 | **Slow-loris:** 200 half-open connections (partial request line, never completed) were still held after 35 s - the server has no header-read timeout, so ~20 000 idle sockets (the process limit) would exhaust file descriptors | medium (LAN attacker / buggy client) | ⚠ Needs decision (requires `Cargo.toml`): add `hyper-util` and set `http1().header_read_timeout(10 s)` + a `TokioTimer` on both listeners (`axum_server` builder and the plain `axum::serve` path). Interim: set `LimitNOFILE=65536` and `net.ipv4.tcp_keepalive_time=60` in the unit/sysctl, keep the plant LAN firewalled. |
| 19 | The local-CA **server certificate lacked the Authority Key Identifier**; strict TLS clients (Python 3.13 / OpenSSL strict, some Android builds) refused the chain | low | 🔧 `use_authority_key_identifier_extension`; `local_tls_leaf_has_authority_key_identifier`. A server cert generated before this fix is reused until it is renewed - delete `data/tls/server.*` to regenerate (the CA is kept, phones need no re-install). |
| 20 | TLS paths (local CA, `curl --cacert`, HTTP/2, SAN `localhost` + `127.0.0.1`, TLS 1.1 refused, HSTS `max-age=31536000` only on TLS, `max-age=0` on plain HTTP, private keys `0600`, plain HTTP on the TLS port refused) | – | ✅ |
| 21 | Config: `--mode domain` refuses missing `--domain` / cert / key / `--insecure-http`; `--localhost` + `--mode domain` refused; cert without key refused; `--localhost` forces `127.0.0.1`; `cargo tree -i openssl` empty; `cargo clippy --lib --bins`: 36 style warnings, no correctness lints; migrations re-applied on every restart without effect | – | ✅ |
| 22 | Inactive masters: new batches / jar movements refuse an inactive product, trips refuse inactive vehicles/routes, dispatch/settlement refuse inactive customers, godown transfers refuse inactive vehicles/godowns. Trip items and dispatch notes of an **inactive product that still has stock** are allowed (selling off the remainder) | – | ✅ accepted by design |
| 23 | Lock order: count post and godown transfer locked batch rows `ORDER BY id`, loaders use (expiry, created_at, id) - a deadlock surfaced as `409 RETRY` | low | 🔧 canonical order everywhere |
| 24 | Online password guessing: 5 failures -> 15 min lock per account (case / whitespace / unicode / NUL variants all hit the same counter), 30 attempts / min per IP; anyone can lock a known username for 15 min (Admin can unlock) | – | ✅ accepted (also in the first table) |

### Needs decision
1. **Slow-loris / header-read timeout (#18)** - needs `hyper-util` in `Cargo.toml` (not touched by QA).
2. **Zombie SSE streams.** A phone that loses Wi-Fi without closing the socket keeps its stream counted against the 8-per-user cap until the kernel gives up (about 15 min by default). Evicting the oldest stream instead of answering 429 would make 9 legitimate tabs fight each other, so the cap was left alone. Mitigation: `net.ipv4.tcp_retries2=8` / `tcp_keepalive_time=60` on the server, or an explicit per-stream idle policy.
3. **Fleet integrity of count posts.** A physical count that reduces `JAR_EMPTY` / `JAR_CLEAN` (or an adjustment that adds jars) changes stock without a `jar_fleet_events` row, so fleet analytics (`/jars/fleet`) can drift from stock. Counts are corrections, not destruction; decide whether a count variance in a jar zone should write a DESTROYED / NEW_PURCHASED event.
4. **Adjustments may add finished goods above `batch.crates`** (an audited "found stock" correction needing `confirm` when large). Decide whether `FG_READY + SCRAP + shipped` of a batch should be capped by the produced quantity.
5. **Idempotency edge**: if the 30 s request timeout fires *after* the handler committed (slow client, slow DB UPDATE of the key), the key stays pending 35 s and a retry after that window re-executes. The window is only the few milliseconds between commit and the key update; a durable fix needs the key update inside the handler's transaction.

### Residual risks (accepted)
Session key is JS-readable by design (XSS = account takeover; CSP `script-src 'self'` mitigates). No MFA. Single-process in-memory limiters (lost on restart, per process). Web Push delivery is at-most-once. `X-Forwarded-For` is never trusted, so behind a reverse proxy the per-IP limiter sees the proxy address (the per-account lockout still protects).
