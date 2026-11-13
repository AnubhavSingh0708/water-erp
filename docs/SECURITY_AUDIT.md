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
