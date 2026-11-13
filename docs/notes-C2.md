# Notes – Subagent C2 (jars / maintenance / notifications / Web Push)

Deviations from `API_REFERENCE.md`, decisions and hand-offs. Everything here is additive or stricter unless stated otherwise.

## Tier 1 – Notifications & Web Push

* **Stricter SSRF rules than section 13** (`push::validate_endpoint`): *every* IP literal in any notation is refused (not only
  private ranges: `8.8.8.8`, `[2001:...]`, `2130706433`, `0x7f.1`, octal), only port 443, ASCII only, no `\ @ %` in the
  authority, host needs >= 2 labels, blocked TLDs `localhost local internal lan home corp intranet localdomain arpa invalid test example onion private`.
  Run at subscribe time and again at send time (a stored endpoint that no longer validates is deactivated).
* **Send-time DNS check** (names like `127.0.0.1.nip.io` pass the syntactic check): the host is resolved by the server, a send is
  refused when ANY resolved address is non-public (`push::is_public_ip`: loopback, private, link-local, CGNAT, TEST-NETs,
  multicast, reserved, IPv6 ULA/link-local/site-local/Teredo/doc, IPv4-mapped/compatible, NAT64 and 6to4 with embedded private v4), and the HTTP client
  is pinned to the checked addresses (`resolve_to_addrs`) so DNS rebinding cannot swap the target.
* **Dedicated push HTTP client** (not `AppState::http`, whose default redirect policy follows redirects): `redirect(none)`,
  connect 5 s / total 10 s, `https_only`, response body never read, and **`no_proxy()`** – an environment `HTTPS_PROXY` would
  bypass the address pin. A plant that can only reach the internet through an HTTP proxy therefore cannot use Web Push
  (documented trade-off; the in-app inbox still works).
* **VAPID**: `VAPID_PRIVATE_KEY` = base64url of the raw 32-byte scalar; the public key is always *derived* from it (the configured
  `VAPID_PUBLIC_KEY` is ignored) so `/push/key` can never advertise a mismatching key. Without a valid private key `enabled=false`.
  `POST /push/test` answers `409 FEATURE_DISABLED` then; it is limited to 5 per user per minute (`429 LOCKED`).
* **Subscribe**: body also tolerates `expirationTime` (what `PushSubscription.toJSON()` emits). `p256dh` must be a valid 65-byte P-256 point,
  `auth` 16 bytes. Re-registering an endpoint that belongs to another user *moves* it to the caller (shared browser/device) – the
  encrypted payload goes to the keys the caller supplied, so the previous owner loses it. Max 10 subscriptions per user (oldest deleted).
  `unsubscribe` only removes the caller's own rows and is always `204` (no probing).
* **Dispatcher** (`workers::push::dispatch_once` / `dispatch_with(state, Option<&dyn PushSender>)`): claim (`FOR UPDATE SKIP LOCKED`),
  recipient selection, `push_deliveries` insert (`ON CONFLICT DO NOTHING`) and `dispatched_at` are one transaction; HTTP sends happen
  after commit => at-most-once. Recipients = `users.is_active` AND subscribed to the event type AND owning >= 1 active subscription.
  Alerts older than 6 h are marked dispatched without a push. Push payload `url` is forced to a same-origin relative path.
* **Inbox** is filtered by the caller's *current* subscriptions in SQL on every call; resolved alerts remain visible for 30 days
  (open alerts always). `POST /inbox/{id}/read` on an alert outside the subscriptions (or unknown) is `404`.
  Response items additionally carry `resolved`. `POST /inbox/read-all` -> `{marked:n}`.
* Shared extractors `routes::notifications::extract::{ApiJson, ApiQuery, is_dry_run, opt_uuid, opt_date, de_int}` return the
  standard error envelope for malformed bodies/queries (axum's defaults are plain text). Used by all C2 routes.
* Observation for the Lead (not changed): users may subscribe to any event type regardless of `allowed_pages`, so a user without the
  `jars` page can subscribe to `JAR_OVERDUE` and read customer names in alert titles. The contract says subscriptions are self-managed;
  if that is a concern, restrict `PUT /subscriptions` by page (map event type -> page) in a later pass.

## Tier 2 – Returnable-container fleet (`/api/jars/*`)

* **Migration `0014_jar_holding_guard.sql`** (only migration of C2): DEFERRED constraint trigger on `jar_ledger` – a customer's holding per
  product can never be negative at COMMIT (SQLSTATE 23514 -> HTTP 400), serialised per (customer, product) with
  `pg_advisory_xact_lock(hashtextextended('jar:<customer>:<product>', 0))`. Deferred, so row order inside one transaction does not matter.
  **Heads-up C1:** settlement may insert its `jar_ledger` rows in any order, but the final per-(customer,product) sum must be >= 0 at commit
  (your own pre-check should reject first). **Lead:** the trigger is the backstop for every writer, my handlers take the same advisory lock first.
* `wash {product_id, qty, rejected?}`: **`qty` = containers that came out clean** (-> `JAR_CLEAN`), `rejected` = *additional* containers that failed
  inspection (-> `SCRAP_YARD`, event DESTROYED/WASH_REJECT/PLANT_EMPTY). `JAR_EMPTY` is reduced by `qty + rejected` (FEFO over any lots, so non-nil
  lots left by other modules work). One WASHED event with `qty`. A wash with more than `settle.damage_warn_pct` % rejects needs `confirm:true`.
* `destroy`: always `CONFIRM_REQUIRED` until `confirm:true` (also under `?dry_run=1`). `location` PLANT_EMPTY/PLANT_CLEAN deduct FEFO from the zone and move
  to `SCRAP_YARD`; CUSTOMER needs `customer_id` (forbidden for the other locations) and writes `jar_ledger WRITE_OFF`. Event + audit in the same tx.
* `receive-empties`: `qty_good`/`qty_damaged` both optional (default 0, sum >= 1); more than the customer holds -> `400` (record an opening balance first).
  Damaged ones write `DAMAGED` ledger rows, SCRAP_YARD stock and a DESTROYED/DAMAGED_RETURN event with `location=CUSTOMER`, `customer_id`.
  More than `settle.damage_warn_pct` % damaged needs `confirm:true`.
* `issue`: `stock::allocate_free_fefo` (reservation aware, APPROVED only, locks the batch rows); an already-overdue customer or >= 100 containers needs `confirm:true`.
* `opening` has an optional `as_of` date (default today, never in the future) so the overdue clock starts at the real hand-out date; a second opening
  for the same (customer, product) or >= 500 needs `confirm:true`.
* `receive-new`: >= 1000 containers or > 25 % of the current fleet needs `confirm:true`.
* Optional `note` (<= 500) on opening/issue/receive-empties/receive-new/wash.
* Stock ledger `txn_type`s used: `JAR_ISSUE`, `JAR_RECEIVE`, `JAR_DAMAGED`, `JAR_PURCHASE`, `JAR_WASH`, `JAR_WASH_REJECT`, `JAR_DESTROY` (free text column).
* `GET /summary` adds `in_vehicles`, `fleet_total`, `code`, `size_ml`, `max_cycles`. `GET /customers` hides customers with holding 0 unless `all=1`;
  returns at most 500 rows ordered overdue first, then days outstanding. `GET /customers/{id}/ledger[?product_id=]` rows carry `balance_after`
  (running holding per product); response also has `customer`.
* `GET /fleet`: default period = last 30 plant-local days (so `alert` matches the worker's rule); `product_id` may be omitted when exactly one active
  RETURNABLE product exists. Extra fields: `fleet.in_vehicles`, `period.from/to`, `lifetime{fills,destroyed}`, `warn_pct`, `alerts[]`.
  Percentages and the cycle estimate are 2-dp strings (`"3.00"`); counts are JSON numbers. `fills` = Σ `production_batches.crates` by `produced_on`.
  `delivered` = Σ `ISSUE_FILLED`, `empties_returned` = Σ `RETURN_EMPTY` (by `txn_date`); events are bucketed by plant-local date.
  `HIGH_DESTROY_RATE` when `destroyed*100 > destroy_warn_pct*fills` (so any destruction with zero fills counts); `NEAR_RETIREMENT` at `retirement_pct >= 90`;
  `HIGH_DESTROY_RATE` wins when both apply.
* Dry runs return `effects[{item,zone,delta,after}]` with 4-dp strings (`"-2.0000"`); the customer side uses `zone:"CUSTOMER"`. Real responses (`201`) include `effects` and `warnings` too.

## Tier 3 – Maintenance & fleet documents

* Status: `OVERDUE` = date before plant-local today; `DUE_SOON` = within `alerts.expiry_lead_days` **inclusive** (today counts as due, not overdue); else `OK`.
  Vehicle documents use the same three words (not `EXPIRED`) plus an `expired` boolean; documents without a date are not tracked/listed; inactive
  vehicles/equipment and inactive schedules are hidden. Lists are bounded to 500 rows, soonest first.
* `POST /schedules/{id}/complete`: schedule row locked `FOR UPDATE`; `done_on` defaults to plant today and may not be in the future; completing on or before
  `last_done_on` -> `409 INVALID_STATE` (double-submit guard); `next_due_on` defaults to `done_on + interval_days`, required without an interval and must be after
  `done_on`. The old `MAINTENANCE_DUE` alert is resolved in the same transaction. Returns the updated schedule row (+ `log_id`).
* Worker `workers::maintenance::run_once` (hourly, plus once at start): per event type it raises what is currently true and **resolves every open alert of that type
  whose key is no longer current** (set difference), so completed/moved/deactivated/renewed items clear themselves. Already-open alerts get their title/severity refreshed in
  place (DUE_SOON -> OVERDUE) without re-notifying. Keys: see the header of `workers/maintenance.rs`. `JAR_FLEET_ALERT` keys (contract has none):
  `<product_id>:HIGH_DESTROY_RATE` and `<product_id>:NEAR_RETIREMENT`.

## Hand-offs, requests and known gaps

* **Lead – `error.rs` hint:** `hint_for("FEATURE_DISABLED")` says an administrator can enable it "in Settings"; for Web Push (`POST /push/test` without VAPID keys) the real fix
  is generating keys (`water-erp gen-vapid`) and setting `VAPID_PRIVATE_KEY` + restart. Consider a code-specific message or hint.
* **Lead – branch base:** the worktree branch `worktree-agent-a7135c985a9e28055` was created at the initial commit `8be844a`; it was reset to the Lead's `51dbd5c` before any work.
* **No new crates requested.** (`url` is not a direct dependency, so the endpoint validator is hand-written; `async-trait`, `futures`, `p256`, `base64`, `web-push-native`, `reqwest` already present.)
* **A (seed):** the new `jar_ledger` constraint trigger also applies to seed data: seeded customer holdings must not net below zero per (customer, product).
* **C1 (settlement):** see Tier 2 note – final per-(customer, product) sum must be >= 0 at COMMIT; the trigger takes an advisory lock then.
* Trailing-dot hosts (`example.com.`) are rejected by the endpoint validator because the DNS pin key would differ from the host the HTTP client connects to.
* Path-id extractor errors (`ApiPath`) return the standard envelope (`400 BAD_REQUEST`).
* **Known gaps:** the real `WebPushSender` was only exercised up to "connection failed" (sandbox has no push-service egress; request building/encryption is unit-tested by
  decrypting with the browser key). No `?dry_run` on maintenance `complete` (not required by the contract). Push delivery is at-most-once (no retry of transient failures).

## Permission-gated notifications (Lead follow-up)
Uses `alerts::event_pages` / `alerts::user_may_receive`: `GET /types`, `PUT/GET /subscriptions` (unknown or not-permitted type -> 422 on `subscribed`; GET hides stored but ineffective ones),
inbox / unread-count / read / read-all (permission re-evaluated on every request, so granting or removing a page shows or hides old alerts) and the push dispatcher
(recipient SQL requires Admin or one of the event's pages) all apply it. This supersedes the "not restricted by page" observation above.

## Multiple godowns (API_REFERENCE §0.2-F)
* `receive-new`, `wash`, `destroy` (PLANT_EMPTY/PLANT_CLEAN), `receive-empties`, counter `issue` take optional body `location_id` (resolved by `util::resolve_location`: 403 foreign, 422 unknown/inactive/TRANSIT,
  omitted => the single allowed godown else MAIN). All stock moves/reads use `Move::at`, `consume_fefo_at`, `available_at`, `allocate_free_fefo_at` (reservations are per godown). `jar_fleet_events.location_id` is
  always stored (the handling godown, also for customer-site write-offs). Effects carry `location_id`.
* `GET /summary` and `GET /fleet` accept `location_id` (uuid, or `all` = every godown the caller may use; omitted => resolve_location default). `fleet`/`summary` plant counts, `in_vehicles` (trip `location_id`), `fills` (batch
  `location_id`) and fleet events (NULL location counts as MAIN) are filtered to the scope. **Customer holdings, `delivered` and `empties_returned` stay global** (jar_ledger has no godown), so `fleet.total` =
  scope stock + in-vehicles of the scope + all customer holdings. `avg_cycles_estimate`/`retirement_pct` always use the WHOLE fleet (all godowns + transit + customers) so they do not depend on the filter.
  `/fleet` adds `location_id` and `by_location:[{location_id,name,plant_empty,plant_clean,filled_ready,filled_quarantine}]` (all godowns the caller may use, zero rows included; others silently filtered).
* The maintenance worker's JAR_FLEET_ALERT stays per product over the totals of all locations (`fleet_report(.., None)`). `GET /destroyed` items now include `location_id`.
