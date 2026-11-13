# Water Plant ERP – API & Behaviour Contract (v1)

This file is the **single source of truth** shared by backend and frontend agents. Backends implement exactly this;
deviations go in `docs/notes-<owner>.md` and are reconciled by the Lead before the frontends start.

Owners: **A** = auth/admin/infra, **B** = materials/production/QC/warehouse/ref/dashboard, **C** = QR/loading/jars/maintenance/push.
Schema: `migrations/0001..0003` (Lead-owned, already applied). A may add `0004_*.sql`, B `0005_*.sql`, C `0006_*.sql` – nothing else.

---------------------------------------------------------------------------------------------------------------------
## 0. Global conventions

* All bodies JSON (`Content-Type: application/json`). IDs are UUID strings. Dates `YYYY-MM-DD`. Timestamps RFC 3339 UTC.
* **Decimals are JSON strings** in responses (`"12.5000"`); requests accept a string **or** a number. Never use floats.
* Auth: header `X-Auth-Key: <key>` (or `Authorization: Bearer <key>`) on every `/api/*` call except the *public* list below.
* **Public**: `GET /api/health`, `POST /api/auth/login`, `GET /api/setup/status`, `POST /api/setup/admin` (only while no Admin exists).
* **Error envelope** (all non-2xx): `{"error":{"code":"…","message":"…","fields":{"field":"msg"}?,"details":{…}?}}`

| HTTP | code | meaning |
|---|---|---|
| 400 | `BAD_REQUEST` | malformed input / rule violation |
| 401 | `UNAUTHORIZED` | missing/invalid/expired key |
| 403 | `FORBIDDEN` | page not in `allowed_pages`, not Admin, or Origin rejected |
| 404 | `NOT_FOUND` | |
| 409 | `DUPLICATE`, `IN_USE`, `RETRY` | unique violation, referenced row, deadlock-retry |
| 409 | `INSUFFICIENT_STOCK` | `details:{available,required,zone,…}` (batch creation: `details.shortages[]`) |
| 409 | `QC_LOCKED` | batch/set is not APPROVED (strict lock) |
| 409 | `CAPACITY_EXCEEDED` | vehicle capacity |
| 409 | `CONFIRM_REQUIRED`, `IDEMPOTENCY_CONFLICT`, `IN_PROGRESS`, `FEATURE_DISABLED` | see §0.2 |
| 503 | `UNAVAILABLE` | database/pool unreachable; nothing saved |
| 409 | `INVALID_STATE` | state-machine violation (e.g. settle a trip that is not DISPATCHED) |
| 409 | `VEHICLE_DOCS_EXPIRED` | gate pass blocked: insurance/fitness/PUC/permit expired |
| 409 | `LAST_ADMIN` | cannot demote/deactivate/delete the last active Admin |
| 409 | `SETUP_DISABLED` | first-run setup already done |
| 422 | `VALIDATION` | `fields:{name:msg}` |
| 429 | `LOCKED` | login throttled / account temporarily locked |
| 500 | `INTERNAL` | never leaks SQL or stack traces |

* **Pagination**: list endpoints marked `{items,total}` take `?limit` (default 50, max 200) & `?offset`. Others return a bare array (bounded to 500 rows).
* **Cookie**: login also sets `erp_key=<key>; Secure; SameSite=Strict; Path=/; Max-Age=2592000` (JS-readable on purpose – NOT HttpOnly).
  Server-side expiry is the real limit (sliding `auth.session_ttl_hours`). Logout clears it (`Max-Age=0`).
* **Page keys** (`users.allowed_pages`): `dashboard, raw-materials, production, qc, warehouse, qr-labels, loading, jars, maintenance, admin`.
  Admins implicitly have all pages. `/api/ref/*`, `/api/me/*`, `/api/notifications/*` need only a valid login.
* **Page → API guard matrix** (enforced in `routes/mod.rs`): `/api/dashboard`→dashboard · `/api/raw-materials`→raw-materials · `/api/production`→production ·
  `/api/qc`→qc · `/api/warehouse`→warehouse · `/api/qr`→qr-labels **or** loading · `/api/loading`→loading · `/api/jars`→jars · `/api/maintenance`→maintenance · `/api/admin`→Admin only.
* **Zones**: `RM_STORE, BLOW_FLOOR, QC_QUARANTINE, FG_READY, SCRAP_YARD, JAR_EMPTY, JAR_CLEAN`.
* **Stock units**: RAW stock is in the material's `stock_unit` (pcs/kg…); PO/GRN quantities are in `purchase_unit` (× `stock_per_purchase_unit` = stock units).
  PRODUCT stock is in **crates** (jars for `JAR20L`, `units_per_crate = 1`).
* **Event types** (notifications): `LOW_STOCK, REORDER_RECOMMENDED, QC_PENDING, QC_APPROVED, QC_REJECTED, WATER_OUT_OF_RANGE, GATE_PASS_ISSUED, TRIP_SETTLED, JAR_OVERDUE, MAINTENANCE_DUE, VEHICLE_DOC_EXPIRY`.
* **Limits** (use `src/validate.rs`): qty ≤ 1e9 & ≤ 4 dp; crates 1..1 000 000; lines ≤ 200; free text ≤ 500 (notes) / 120 (names); search `q` ≤ 100 chars, always bound as `ILIKE` parameter via `validate::like_pattern`.

### 0.1 Security rules every backend module MUST follow (pentest checklist)
1. Parameterised SQL only (`sqlx::query().bind()`); never interpolate user input. Dynamic ORDER BY/columns only from a whitelist.
2. Every state-changing handler runs in **one DB transaction**; rows that gate a decision (`production_batches`, `qr_crate_sets`, `loading_trips`, `users`) are read `FOR UPDATE` and the decision is **re-checked inside the transaction** (QC status, set status, trip status, capacity). No check-then-act outside the tx.
3. All stock changes only via `crate::stock` (never raw `UPDATE stock_balances`).
4. Request structs use `#[serde(deny_unknown_fields)]`; no handler lets a client set `is_admin`, `qc_status`, `status`, `created_by`, `*_by` or ids of the caller.
5. Server-side re-validation of everything the UI "prevents" (capacity, QC lock, state machines, non-negative stock, ownership of ids). Treat every UUID in a body as hostile: verify it exists / is active.
6. Audit (`crate::audit::log`) every admin action and every irreversible floor action (QC decision, gate pass, settle, write-off, adjustment, revoke, user changes, settings changes).
7. Errors never include SQL text, hashes, keys, stack traces. Login responses never reveal whether a username exists.
8. Output encoding: JSON only; SVG QR strings come solely from the QR generator.
9. No endpoint returns `password_hash` or `key_hash`.

---------------------------------------------------------------------------------------------------------------------
## 0.2 AMENDMENTS (supersede conflicting text elsewhere in this file)

### A. QR labels are OPTIONAL – loading works on gross stock by default
* Setting `features.qr_labels` (default **false**). While false, **all `/api/qr/*` routes answer `409 FEATURE_DISABLED`** (gate already implemented in `routes/mod.rs`) and the QR/sticker UI is hidden. `/api/ref/settings` exposes `qr_labels_enabled`.
* **Default loading = bulk by product/batch** (no stickers, no per-set ids). `trip_items.qr_set_id` is NULL for bulk items; unique per `(trip_id, batch_id)`.
  * `GET /api/loading/availability?product_id=` → `[{product_id,product_name,kind,batch_id,batch_no,expiry_date,qc_status,fg_ready,reserved,available}]` ordered FEFO. `reserved` = Σ crates of bulk items in other **LOADING** trips for that batch; `available = fg_ready − reserved`. Only APPROVED batches (stock in `FG_READY`) are listed; when `loading.strict_qc_lock=false`, QUARANTINE batches are listed too with `qc_status` so the UI can badge them (never REJECTED).
  * `POST /api/loading/trips/{id}/items` body one of: `{product_id, crates}` (server allocates FEFO across batches, may create several items; adding to an existing (trip,batch) item increments it) · `{batch_id, crates}` (explicit batch) · `{qr, crates?}` (only when QR feature on; as in §10). Rules inside one tx: trip `LOADING`; batch row `FOR UPDATE` (serialises concurrent loaders on the same batch); `crates ≤ available` else `INSUFFICIENT_STOCK` (`details.available`); QC rule (`QC_LOCKED` when batch not APPROVED and strict); capacity → `CAPACITY_EXCEEDED`.
  * Gate pass / settle / void work identically for bulk items (no set-state transitions when `qr_set_id IS NULL`). ItemObj gains nullable `code`,`set_label` (null for bulk).
* The warehouse, production and QC modules never depend on QR sets. QC approve/reject only touches `qr_crate_sets` rows if any exist.

### B. Additional stock documents (tables in migration 0009). Documents are never deleted; the ledger is never edited. **Mistakes are corrected by VOID / reversing ledger rows that carry a reason and are audited.**
| Doc | Owner/page | Endpoints (all POST bodies `deny_unknown_fields`) |
|---|---|---|
| **Dispatch note** (walk-in / direct sale of finished goods – quantities only, no prices/tax/payment) | C · `loading` | `POST /api/loading/dispatches {customer_id?|party_name, vehicle_no?, note?, lines:[{product_id,crates}|{batch_id,crates}]}` → FEFO from `FG_READY` (`DISPATCH` ledger), number `DN-yymmdd-0001`; needs `customer_id` or `party_name` · `GET /dispatches` `{items,total}` · `GET /dispatches/{id}` (printable) · `POST /dispatches/{id}/void {reason}` (restocks the same lots, `DISPATCH_VOID`; within `loading.void_window_hours` or Admin) |
| **Gate-pass void** (wrong load / vehicle never left) | C · `loading` | `POST /api/loading/trips/{id}/void {reason, confirm:true}` only `DISPATCHED` and not settled, within `loading.void_window_hours` (Admin unlimited): restocks every item's lot (`GATE_PASS_VOID`), sets (if any) back to AVAILABLE, trip → `CANCELLED` with `void_reason` |
| **GRN void** | B · `raw-materials` | `POST /api/raw-materials/grns/{id}/void {reason}` – only if each lot is still fully in `RM_STORE` (else `409 INSUFFICIENT_STOCK` "already consumed – use a supplier return or adjustment"); reverses ledger (`GRN_VOID`), rolls back `po_lines.qty_received`/PO status, GRN `status=VOID` |
| **Supplier return** | B · `raw-materials` | `POST /api/raw-materials/supplier-returns {supplier_id, grn_id?, note?, lines:[{raw_material_id, lot_id, qty_stock, reason}]}` – stock out of `RM_STORE` (`RTV_OUT`), `RTV-yymmdd-0001`; `GET` list/detail (printable) |
| **Write-off / destroyed stock** | B · `warehouse` | `POST /api/warehouse/write-offs {item_type,item_id,zone,lot_id,qty,reason_code,note,confirm:true,dispose?:bool}` – `reason_code ∈ BREAKAGE,EXPIRED,CONTAMINATED,SPILLAGE,QC_REJECT,THEFT_LOSS,TRANSIT_DAMAGE,OTHER`; note ≥ 5 chars; moves stock to `SCRAP_YARD` (`WRITE_OFF`) unless already there; `dispose:true` (or write-off from SCRAP_YARD) removes it from the books (`DISPOSE`). Not allowed from `QC_QUARANTINE` (QC owns that). `WO-yymmdd-0001`. `GET /write-offs` `{items,total}` (filter reason/from/to) + `?format=csv` |
| **Physical stock count** | B · `warehouse` | `POST /api/warehouse/counts {zone, note?}` snapshots expected qty per (item,lot) → DRAFT · `PUT /counts/{id}/lines {lines:[{line_id,counted_qty,note?}]}` · `GET /counts/{id}` (shows `variance = counted − expected`, `% `) · `POST /counts/{id}/post {confirm:true}` posts `ADJUST` rows for variances (reason "stock count CNT-…"); balances changed since the snapshot are recomputed at post time (variance applies to the **current** balance: `delta = counted − current`) · `POST /counts/{id}/cancel`. Lines with large variance (> `entry.large_adjust_pct`) require the line `note`. |
Printable sources (the UI prints them with `@media print`): GRN, supplier return, dispatch note, write-off, gate pass, count sheet.

### C. Mistake-proofing patterns (every module must follow)
1. **`confirm` flag** – suspicious but possible entries return `409 CONFIRM_REQUIRED` with `details.warnings:[…]`; the client shows them and re-sends the same body with `"confirm": true`. Mandatory cases: write-off (always), count post (always), adjustment/transfer larger than `entry.large_adjust_pct` % of the balance (or Δ ≥ 1000 units), gate-pass void (always), GRN with qty > 3× the remaining PO line or > 10× the median of the last 10 GRNs of that material, GRN whose `(supplier, challan_no)` already exists → `DUPLICATE` (never confirmable – void the old one), batch `crates` > 2× the max of the last 10 batches of that product, settlement where `damaged` > `settle.damage_warn_pct` % of loaded, blow run with `scrap_pct > production.scrap_warn_pct` (requires a non-empty `notes`, plus a `warnings` entry in the 201 response).
2. **Soft warnings** – 2xx bodies for creates may include `"warnings":["…"]` (e.g. water reading out of range, batch within 10 % of expiry, loading ≥ 95 % of capacity). UI shows them as a non-blocking banner.
3. **`?dry_run=1`** on `POST` of: GRN, batch creation, adjustments, transfers, write-offs, trip items, gate-pass, settle, dispatch, count post → runs the full logic in a transaction that is **rolled back**, returns `200 {"dry_run":true,"effects":[{"item","zone","delta","after"}…],"warnings":[…]}` or the same error the real call would give. The UI uses it to show a *review screen* ("This will deduct 40 crates of 1 L from batch B-…") before the real submit.
4. **`Idempotency-Key`** header (implemented in `routes/mod.rs`): same user+key ⇒ first response replayed, handler not re-run. Frontend generates one key per user action and **reuses it on retries**.
5. **Business dates** use `util::plant_today()` (plant local time, setting `plant.utc_offset_minutes`), never `CURRENT_DATE`.
6. **Stale-screen defence** – state machines + row locks return `409 INVALID_STATE` (hint: "someone else changed this – review and retry"); list/detail responses include `updated_at` (or status) so the UI can show "changed by someone else".
7. **Error envelope additions**: `error.hint` (actionable next step, server-generated per code, see `error.rs::hint_for`) and response header `X-Request-Id` (UI shows "Ref: …" on 5xx so admins can find the log line). `503 UNAVAILABLE` = DB/pool problem, nothing was saved. Request timeout 30 s → 503.
8. **Number inputs**: server rejects NaN/negative/over-precision (see `validate.rs`); client also clamps; large values use `CONFIRM_REQUIRED`, never silently accepted.

### D. Operational endpoints
`GET /api/health` → `200 {status:"ok",db,db_ms,version}` · `503 {status:"degraded"|"shutting_down"}`. Graceful shutdown (SIGTERM/Ctrl-C): health turns 503, SSE streams get `server_restarting`, in-flight requests finish (≤ 25 s), workers see `state.shutdown`, DB pool closes. Workers MUST `select!` on `state.shutdown.wait()` in their sleep.

---------------------------------------------------------------------------------------------------------------------
## 1. Auth, setup, me  (Owner A)

**Me object**: `{id, username, full_name, is_admin, allowed_pages:[…], subscribed_notifications:[…]}` (Admin: `allowed_pages` = all page keys).

| Method & path | Body | Response |
|---|---|---|
| `GET /api/health` | – | `{status,db,version}` |
| `GET /api/setup/status` | – | `{setup_required:bool}` |
| `POST /api/setup/admin` | `{username,full_name,password}` | `201 {key,expires_at,user:Me}` + cookie. `409 SETUP_DISABLED` once any Admin exists (atomic: advisory lock). |
| `POST /api/auth/login` | `{username,password}` | `200 {key,expires_at,user:Me}` + cookie. Bad creds → `401 UNAUTHORIZED` "Invalid username or password" (unknown user does a dummy Argon2 verify – no enumeration/timing leak). After `auth.max_failed_logins` failures → `429 LOCKED` for `auth.lockout_minutes` (counter resets on success). Also a small in-memory per-IP limiter (≥ 30 attempts/min → 429). Inactive users cannot log in. |
| `GET /api/me` | – | Me |
| `POST /api/me/logout` | – | `204`, deletes this session, clears cookie |
| `POST /api/me/password` | `{current_password,new_password}` | `204`; revokes all **other** sessions. Policy: ≥ 10 chars, ≠ username. |

Session rules: key = 32 CSPRNG bytes hex; only `SHA-256(key)` stored; sliding `last_used_at`/`expires_at` (DB write throttled to 60 s – implemented in `auth::authenticate`);
login creates a new session each time; max 10 active sessions per user (oldest deleted).
First Admin can also be created with `--setup-admin <user>` + env `ERP_ADMIN_PASSWORD` (`routes::admin::bootstrap_admin`).

## 2. Admin  (Owner A) – every route Admin-only, every mutation audited

* `GET /api/admin/settings` → `[{key,value,description}]` · `PUT /api/admin/settings` body `{ "<key>": value, … }` (all-or-nothing; validated by `settings::update`) → updated list. Live (no restart).
* `GET /api/admin/pages` → `[{key,label}]` · `GET /api/admin/notification-types` → `[{code,label}]`
* **Users**: `GET /users` · `POST /users {username,full_name,password,is_admin,allowed_pages[],subscribed_notifications[]}` · `GET/PUT /users/{id}` (`full_name,is_admin,is_active,allowed_pages,subscribed_notifications`) · `POST /users/{id}/password {new_password}` · `POST /users/{id}/unlock` · `DELETE /users/{id}` (= deactivate + revoke sessions).
  Validate `allowed_pages ⊆ PAGES`, `subscribed ⊆ EVENT_TYPES`. Usernames: 3–40 chars `[A-Za-z0-9._-]`, case-insensitively unique. `LAST_ADMIN` guard on demote/deactivate/delete. User objects never expose hashes.
* **Sessions**: `GET /sessions` → `[{id,user_id,username,created_at,last_used_at,expires_at,user_agent,ip,is_current}]` · `DELETE /sessions/{id}` · `POST /users/{id}/revoke-sessions` · `POST /sessions/revoke-all` (all except caller's).
* **Audit**: `GET /audit?user=&action=&entity=&from=&to=&limit&offset` → `{items:[{id,at,user_id,username,action,entity,entity_id,details,ip}],total}` (read-only, DB-immutable).
* **Master data CRUD** (`GET` list incl. inactive + `?q=`, `POST`, `GET/{id}`, `PUT/{id}`, `DELETE/{id}` = soft delete `is_active=false`, never cascades). Field names = DB columns of `migrations/0002_master.sql`:
  `/suppliers`, `/raw-materials`, `/products`, `/customers`, `/routes`, `/vehicles`, `/equipment`, `/maintenance-schedules`.
  BOM: `GET /products/{id}/bom` → `[{id,raw_material_id,raw_material_name,qty_per_crate,source_zone}]` · `PUT /products/{id}/bom` replaces the list.
  Validation: `EMPTY_BOTTLE` materials must have `source_zone=BLOW_FLOOR` in BOMs; other categories `RM_STORE`; quantities > 0; vehicles `capacity_crates` 1..100000.
* Seed: `water-erp seed-demo` (idempotent). Demo logins (all passwords `Demo@12345!`): `admin`, `manager` (all pages except admin), `loader` (loading, qr-labels, warehouse, dashboard), `qc` (qc, dashboard), `gate` (loading, dashboard), `store` (raw-materials, warehouse, dashboard), `operator` (production, dashboard), `jars` (jars, maintenance, dashboard).
  Seed also creates: 3 suppliers, preform/cap/label/sleeve/shrink/carton/mineral/chemical materials with stock, empty-bottle materials, products (500 ml×24, 1 L×12, 20 L jar), BOMs, 1 route, 5 customers, 3 vehicles (one with an expired PUC), equipment + schedules, stock, jars at customers, **one QUARANTINE batch with QR sets, one APPROVED batch with AVAILABLE QR sets**, a few ledger/water-log rows.

## 3. Reference data (any logged-in user)  (Owner B) – active rows only, `GET` only

`/api/ref/products` · `/raw-materials` · `/suppliers` · `/customers` · `/vehicles` · `/routes` · `/equipment` → arrays of master rows (vehicles incl. expiry dates; raw materials incl. conversion fields).
`/api/ref/zones` → `["RM_STORE",…]` · `/api/ref/settings` → public subset `{plant_name,plant_code,default_crate_set_size,strict_qc_lock,jar_overdue_days,expiry_lead_days}`.

## 4. Dashboard  (Owner B, page `dashboard`)

`GET /api/dashboard` → `{ kpis:{low_stock_count, open_reorders, batches_in_quarantine, fg_ready_crates, trips_on_road, jars_overdue, maintenance_due, water_log_overdue}, pending_qc:[batch summary], low_stock:[{raw_material_id,name,stock,min_stock,days_cover,status}], active_trips:[trip summary], recent_alerts:[{id,event_type,severity,title,body,url,created_at}] }`
(`jars_overdue`/`maintenance_due` = counts of open `alerts` of that type.)

## 5. Raw materials & procurement  (Owner B, page `raw-materials`)

* `GET /api/raw-materials` → `[{…master, stock_rm_store, stock_total, status:"OK"|"LOW"|"OUT", avg_daily_burn, days_cover|null, open_reorder:bool}]` (`?category=&q=&low=1`)
* `GET /api/raw-materials/{id}` → master + `lots:[{lot_id,zone,qty,expiry_date,lot_no,received_at}]` + `recent_ledger[20]`.
* `GET /api/raw-materials/suppliers` → suppliers with `lead_time_days`.
* **PO**: `POST /purchase-orders {supplier_id,expected_on?,notes?,lines:[{raw_material_id,qty_ordered}]}` → PO (`po_no` `PO-yymmdd-0001`, status `OPEN`) · `GET /purchase-orders?status=` → `[{id,po_no,supplier_name,status,ordered_on,expected_on,lines_count}]` · `GET /purchase-orders/{id}` (with `lines:[{id,raw_material_id,name,purchase_unit,qty_ordered,qty_received}]`) · `POST /purchase-orders/{id}/cancel` (only OPEN with nothing received).
* **GRN**: `POST /grns {po_id?,supplier_id,challan_no?,vehicle_no?,notes?,lines:[{raw_material_id,po_line_id?,qty_purchase,lot_no?,expiry_date?}]}` → posts `GRN` ledger rows into `RM_STORE` with lot = `grn_lines.id`, `qty_stock = qty_purchase × stock_per_purchase_unit`; updates `po_lines.qty_received` & PO status (`PARTIAL`/`RECEIVED`); closes matching reorder rec (`RESOLVED`). Over-receipt beyond 110 % of PO line → 409 `BAD_REQUEST`. `GET /grns` `{items,total}` · `GET /grns/{id}`.
* **Reorder engine** (worker every `inventory.worker_interval_minutes` + `POST /reorder-recommendations/run`): for each active non-`EMPTY_BOTTLE` material:
  `burn = Σ(−delta of RAW ledger rows with txn_type ∈ {BLOW_ISSUE,BATCH_CONSUME} in last N days) / N` (N = `inventory.burn_rate_lookback_days`);
  `reorder_point = burn × (supplier.lead_time_days + inventory.safety_stock_days)`; `current = RM_STORE qty`.
  Reason `BELOW_MIN` (current < min_stock), `BURN_RATE` (burn>0 and current ≤ reorder_point), or `BOTH`. `target = 2 × max(min_stock, reorder_point)`; `suggested_qty_purchase = ceil((target − current) / stock_per_purchase_unit)` (≥ 1).
  One `OPEN` row per material (upserted). Raises `LOW_STOCK` (BELOW_MIN/BOTH) and `REORDER_RECOMMENDED` (BURN_RATE/BOTH) alerts (dedupe key = material id); resolves alerts + rec when stock recovers.
  `GET /reorder-recommendations?status=OPEN` → rows joined with material & supplier names · `POST /reorder-recommendations/{id}/dismiss` · `POST /reorder-recommendations/{id}/create-po` → creates PO (suggested supplier, one line) & marks rec `ORDERED`.

## 6. Production  (Owner B, page `production`)

* **Blow molding** `POST /blow-runs {run_date?,shift,equipment_id?,preform_material_id,bottle_material_id,preforms_issued,good_bottles,notes?}` – preform must be category `PREFORM`, bottle `EMPTY_BOTTLE`, `good ≤ issued`.
  Ledger: preform `−issued` from `RM_STORE` (FEFO, `BLOW_ISSUE`); bottle `+good` into `BLOW_FLOOR` (`BLOW_OUTPUT`); preform `+scrap` into `SCRAP_YARD` (`BLOW_SCRAP`); `scrap_pct`, `scrap_weight_kg = scrap × unit_weight_g / 1000`.
  `GET /blow-runs?from=&to=` `{items,total}` · `GET /blow-runs/{id}`.
* **Water logs** `POST /water-logs {tds_ppm,ph,ozone_ppm?,uv_ok,minerals:{name:number},notes?}` → `out_of_range` vs `lab.*` settings; out-of-range raises `WATER_OUT_OF_RANGE`. `GET /water-logs?from=&to=` `{items,total}` · `GET /water-logs/status` → `{last_logged_at,minutes_since,overdue}` (overdue > 60 min).
* **Batches**
  * `GET /bom-check?product_id=&crates=` → `{ok,lines:[{raw_material_id,name,unit,required,available,short}]}`
  * `POST /batches {product_id,crates,shift,produced_on?,water_log_id?,notes?}` → one transaction: for each BOM line consume `qty_per_crate × crates` from `source_zone` (FEFO, `BATCH_CONSUME`, writes `batch_material_usage`); for `JAR20L` products also consume `crates` jars of that product from `JAR_CLEAN`; output `+crates` of PRODUCT into **`QC_QUARANTINE`** lot=`batch.id` with `expiry = mfg + shelf_life_days` (`BATCH_OUTPUT`). `qc_status=QUARANTINE`. Shortage → `409 INSUFFICIENT_STOCK` with `details.shortages:[{raw_material_id,name,required,available}]` (pre-checked for **all** lines). Raises `QC_PENDING`. `batch_no` = `<plant_code>-B-yymmdd-0001`.
  * `GET /batches?qc_status=&product_id=&from=&to=&q=` `{items,total}` · `GET /batches/{id}` → batch + product + `materials:[…usage]` + `tests:[…]` + `sets_summary:{count,allocated_crates}` + `stock:{QC_QUARANTINE,FG_READY,…}`.
  * Batch summary object: `{id,batch_no,product_id,product_name,crates,qc_status,produced_on,mfg_date,expiry_date,shift,created_at,decided_at}`.

## 7. Lab QC  (Owner B, page `qc`)

* `GET /api/qc/batches?status=QUARANTINE|APPROVED|REJECTED|ALL` (default QUARANTINE) `{items,total}` (summary + `age_minutes`, `last_test_result`).
* `GET /api/qc/batches/{id}` (same as production detail) · `POST /api/qc/batches/{id}/tests {tds_ppm?,ph?,ozone_ppm?,turbidity_ntu?,microbial_ok?,seal_ok?,label_ok?,result:"PASS"|"FAIL",remarks?}` (only while QUARANTINE).
* `POST /api/qc/batches/{id}/approve {notes?}` · `POST /api/qc/batches/{id}/reject {reason}` (reason required, 5–500 chars).
  **Rules**: batch row `FOR UPDATE`; only from `QUARANTINE` (else `409 INVALID_STATE`; decisions are final – no reversal). Approve additionally needs the **latest** test to be `PASS` (`409 INVALID_STATE` code message "QC test required"). 
  Approve moves the **remaining balance of that lot** in `QC_QUARANTINE` → `FG_READY` (`QC_RELEASE`) — *not* `batch.crates` (crates may already have been loaded when `loading.strict_qc_lock=false`) – and flips that batch's `QUARANTINE` QR sets to `AVAILABLE`.
  Reject moves the remaining balance → `SCRAP_YARD` (`QC_REJECT`) and flips `QUARANTINE`/`AVAILABLE` sets of the batch to `REJECTED`. Alerts `QC_APPROVED` / `QC_REJECTED` (one-shot). Audit. The deciding user is stored in `qc_decided_by`.

## 8. Warehouse  (Owner B, page `warehouse`)

* `GET /stock?zone=&item_type=&q=&low=1` → `[{item_type,item_id,name,code,unit,zone,qty,lots}]` · `GET /stock/summary` → `{zones:[{zone,items,qty_rows}], products:[{product_id,name,quarantine,fg_ready,scrap,jar_empty,jar_clean}]}` · `GET /stock/item?item_type=&item_id=` → lots per zone with `expiry_date` (FEFO order).
* `GET /ledger?item_type=&item_id=&zone=&txn_type=&from=&to=` `{items,total}` rows `{id,at,txn_group,item_type,item_id,item_name,zone,lot_id,qty_delta,balance_after,txn_type,ref_type,ref_id,note,user_name}`.
* `POST /adjustments {item_type,item_id,zone,lot_id?,delta,reason}` – `reason` 5–300 chars required; ledger `ADJUST`; audited. **Guards**: not allowed in `QC_QUARANTINE`; PRODUCT `delta>0` in `FG_READY` requires `lot_id` of an `APPROVED` batch; negative results rejected (`INSUFFICIENT_STOCK`).
* `POST /transfers {item_type,item_id,lot_id,from_zone,to_zone,qty,note?}` – allowed only: RAW `RM_STORE↔BLOW_FLOOR`; any item `→ SCRAP_YARD` (write-off, note ≥ 5 chars); PRODUCT `FG_READY→SCRAP_YARD`. **Anything touching `QC_QUARANTINE`, or moving PRODUCT into `FG_READY`, is refused** (`400`) so QC cannot be bypassed.

## 9. QR crate sets  (Owner C, pages `qr-labels` or `loading`)

QR payload = URL `{base}/loading.html?qr={code}` (`base` from `AppState::base_url`). `code` = 10 chars Crockford-base32 random, unique. Sticker also prints `code`, set label, batch, product, crates, expiry.

* `POST /api/qr/batches/{batch_id}/sets` (page `qr-labels`) body one of `{mode:"UNIFORM",set_size}` · `{mode:"COUNT",count}` (near-equal split) · `{mode:"CUSTOM",sizes:[…]}` →
  creates root sets labelled `X,Y,Z,A,B,…` (continuing after existing roots; `AA…` after 26) with status `QUARANTINE` (batch QUARANTINE) or `AVAILABLE` (batch APPROVED); `REJECTED` batch → `409 QC_LOCKED`.
  **Partition invariant**: Σ `original_crates` of root sets (`parent_set_id IS NULL`) of a batch ≤ `batch.crates` (batch row locked `FOR UPDATE`); ≤ 500 sets per call. Returns `[SetObj]`.
* `GET /batches/{batch_id}/sets` → `[SetObj]` · `GET /batches/{batch_id}/sheet` → `{plant_name,batch:{…},product:{…},sets:[SetObj+svg]}` (printable sheet) · `DELETE /sets/{id}` (root set never loaded, no children, status QUARANTINE/AVAILABLE – frees allocation).
* `GET /sets?status=&batch_id=&q=` `{items,total}` (manual search: `q` matches code/label/batch_no) · `GET /sets/lookup/{code}` (scan) → `SetObj` or 404 · `GET /sets/{id}/qr` → `{code,url,svg}`.
* **SetObj**: `{id,code,set_label,seq,batch_id,batch_no,product_id,product_name,units_per_crate,crates,original_crates,status,qc_status,parent_set_id,trip_id,expiry_date,loadable:bool,loadable_reason|null,qr_url,svg?}`.
* **Set status machine** (only these transitions; each under `FOR UPDATE`):
  `QUARANTINE→AVAILABLE` (QC approve) · `QUARANTINE|AVAILABLE→REJECTED` (QC reject) · `AVAILABLE→RESERVED` (added to trip) · `QUARANTINE→RESERVED` (only if `strict_qc_lock=false` and batch not rejected) · `RESERVED→AVAILABLE|QUARANTINE` (removed from trip / trip cancelled; back to what the batch status implies) · `RESERVED→LOADED` (gate pass) · `LOADED→DELIVERED` (settle, nothing returned) · `LOADED→AVAILABLE` (settle with returns; `crates = returned`).

## 10. Loading, gate pass, settlement  (Owner C, page `loading`)

**Trip status machine**: `LOADING → DISPATCHED` (gate pass) · `LOADING → CANCELLED` · `DISPATCHED → SETTLED`. Anything else → `409 INVALID_STATE`.
**TripObj**: `{id,trip_no,status,vehicle_id,vehicle_reg,route_id,route_name,driver_name,capacity_crates,loaded_crates,gate_pass_no,gate_pass_issued_at,created_at,settled_at,notes,items?:[ItemObj]}`; **ItemObj**: `{id,qr_set_id,code,set_label,batch_id,batch_no,product_id,product_name,kind,crates,crates_delivered,crates_returned,crates_damaged,qc_status}`.

* `POST /trips {vehicle_id,route_id?,driver_name?,notes?}` (vehicle active) → TripObj · `GET /trips?status=` `{items,total}` · `GET /trips/{id}` (with items).
* `POST /trips/{id}/items {qr, crates?}` – `qr` = set `code` or set `id` (scan/manual). `crates` omitted ⇒ whole set. If `crates < set.crates` ⇒ **partial split**: the scanned label keeps `crates = n` (status RESERVED, travels with the load), and a **residual child** set (`parent_set_id`, label `<label>-R<n>`, new code, remaining crates, status per batch QC) is created; response `{trip:TripObj, item:ItemObj, residual_set:SetObj+svg|null}` so the UI can print the new sticker.
  Checks (all inside tx, set `FOR UPDATE`, batch row re-read): trip `LOADING`; set status loadable else `QC_LOCKED` (QUARANTINE/REJECTED while strict) or `INVALID_STATE` (RESERVED/LOADED/DELIVERED); Σ crates ≤ `capacity_crates` else `CAPACITY_EXCEEDED` (`details:{capacity,loaded,adding}`); `0 < crates ≤ set.crates`; duplicate set in trip → `DUPLICATE`.
* `DELETE /trips/{id}/items/{item_id}` (LOADING only; set back to AVAILABLE/QUARANTINE; a split residual is not merged) · `POST /trips/{id}/cancel`.
* `POST /trips/{id}/gate-pass {}` – tx: trip `FOR UPDATE`, must be `LOADING` with ≥ 1 item; vehicle documents (insurance/fitness/PUC/permit) not expired → else `VEHICLE_DOCS_EXPIRED`; for each item re-check set `RESERVED` and batch `APPROVED` (if strict) → `QC_LOCKED`; deduct **each set's own lot** (`lot_id = batch.id`) from `FG_READY` (or `QC_QUARANTINE` if batch not yet approved and strict is off) – `GATE_PASS` ledger, **no FEFO**; sets → `LOADED`; trip → `DISPATCHED`, `gate_pass_no` = `GP-yymmdd-0001`; alert `GATE_PASS_ISSUED`; audit. Returns TripObj. Double click ⇒ second call gets `INVALID_STATE` (idempotent by state).
* `GET /trips/{id}/gate-pass` → `{gate_pass_no,plant_name,issued_at,issued_by_name,vehicle:{reg_no,name,driver_name,driver_phone},route,driver_name,items:[ItemObj+units_per_crate],totals:{crates,bottles},notes}` (DISPATCHED/SETTLED only).
* `POST /trips/{id}/settle {items:[{item_id,delivered,returned,damaged}], customer_jars?:[{customer_id,product_id,delivered,empties_collected,damaged,lost}]}` – trip must be `DISPATCHED` (row locked); every item listed exactly once with `delivered+returned+damaged == crates` (non-negative ints).
  Per item: `returned` → back into the same lot in `FG_READY` (`TRIP_RETURN`) and set → `AVAILABLE` with `crates=returned` (if `returned=0`, set → `DELIVERED`, crates unchanged for history); `damaged` → `SCRAP_YARD` (`TRIP_DAMAGED`).
  Jars: for each JAR20L product, Σ `customer_jars.delivered` must equal Σ item `delivered` of that product (customers must exist/active). Each line writes `jar_ledger` `ISSUE_FILLED(+delivered)`, `RETURN_EMPTY(−empties_collected)`, `DAMAGED(−damaged)`, `LOST(−lost)` (rows with qty 0 skipped) with `trip_id`; `empties_collected` adds to `JAR_EMPTY` stock (`JAR_RECEIVE`), `damaged` jars to `SCRAP_YARD`. A customer's holding may not go negative (`409 BAD_REQUEST`). Trip → `SETTLED`; alert `TRIP_SETTLED`; audit.

## 11. 20 L jars  (Owner C, page `jars`)

* `GET /api/jars/summary` → `[{product_id,name,plant_empty,plant_clean,filled_ready,filled_quarantine,with_customers,overdue_customers}]`
* `GET /api/jars/customers?product_id=&overdue=1&q=` → `[{customer_id,name,phone,product_id,holding,oldest_outstanding_date,days_outstanding,overdue:bool}]` (FIFO match of issues vs returns; overdue when `days_outstanding > jars.overdue_days` and holding > 0) · `GET /customers/{id}/ledger` `{items,total}`.
* `POST /opening {customer_id,product_id,qty}` · `POST /issue {customer_id,product_id,qty}` (counter sale: FEFO from `FG_READY`, `ISSUE_FILLED`) · `POST /receive-empties {customer_id,product_id,qty_good,qty_damaged}` (good → `JAR_EMPTY`, damaged → `SCRAP_YARD`; ledger `RETURN_EMPTY`/`DAMAGED`) · `POST /wash {product_id,qty}` (`JAR_EMPTY→JAR_CLEAN`, `JAR_WASH`) · `POST /write-off {scope:"PLANT"|"CUSTOMER",product_id,customer_id?,qty,reason}` (PLANT: `JAR_EMPTY→SCRAP_YARD`; CUSTOMER: ledger `WRITE_OFF`; reason ≥ 5 chars; audited). Filling = a normal production batch of the JAR20L product (consumes `JAR_CLEAN`).

## 12. Maintenance & fleet  (Owner C, page `maintenance`)

* `GET /api/maintenance/schedules?subject_type=&status=` → `[{id,subject_type,subject_name,task,interval_days,last_done_on,next_due_on,days_left,status:"OK"|"DUE_SOON"|"OVERDUE"}]` (`DUE_SOON` = within `alerts.expiry_lead_days`).
* `GET /api/maintenance/vehicle-docs` → `[{vehicle_id,reg_no,doc:"INSURANCE"|"FITNESS"|"PUC"|"PERMIT",expiry_date,days_left,status}]`.
* `POST /schedules/{id}/complete {done_on?,notes?,next_due_on?}` – `next_due_on` defaults to `done_on + interval_days`; required when no interval. Writes `maintenance_logs`. · `GET /schedules/{id}/logs`.
* Worker (hourly): `MAINTENANCE_DUE` (key `schedule:<id>:<next_due_on>`), `VEHICLE_DOC_EXPIRY` (key `vehicle:<id>:<doc>:<date>`), `JAR_OVERDUE` (key `<customer>:<product>`; resolved when no longer overdue).

## 13. Notifications & Web Push  (Owner C, any logged-in user)

* `GET /api/notifications/types` → `[{code,label}]` · `GET /subscriptions` → `{subscribed:[…]}` · `PUT /subscriptions {subscribed:[codes]}` (validated; users manage only their own).
* `GET /push/key` → `{public_key|null,enabled}` · `POST /push/subscribe {endpoint,keys:{p256dh,auth}}` · `POST /push/unsubscribe {endpoint}` · `POST /push/test` (sends to the caller's own subscriptions).
  **SSRF defence**: endpoint must be `https://`, ≤ 600 chars, host a DNS name that is not `localhost`/`*.local`/`*.internal` and not an IP literal in private/loopback/link-local/CGNAT/metadata ranges; outbound push client uses `redirect(none)`.
* `GET /inbox?unread=1` `{items,total}` of alerts whose `event_type` is in the caller's subscriptions `[{id,event_type,severity,title,body,url,created_at,read}]` · `POST /inbox/{id}/read` · `POST /inbox/read-all` · `GET /inbox/unread-count` → `{count}`.
* **Dispatcher** (worker every 5 s): picks alerts with `dispatched_at IS NULL`, finds active users whose `subscribed_notifications ? event_type`, sends VAPID Web Push (`web-push-native` + `reqwest`) payload `{title,body,url,tag,alert_id}` to each active subscription of those users **only**, records `push_deliveries` (no duplicate per alert/user), deactivates subscriptions on 404/410, marks `dispatched_at`. Without VAPID keys: marks dispatched and logs once.
* CLI: `water-erp gen-vapid` prints `VAPID_PRIVATE_KEY=` / `VAPID_PUBLIC_KEY=`.

## 13b. Extra ERP features (added by Lead)

**Owner B** (all inside B's own route files):
* `GET /api/warehouse/expiring?days=60` → lots (RAW and PRODUCT, qty > 0, zones other than SCRAP_YARD) expiring within N days or already expired, with `days_left`. The reorder worker file (`workers/reorder.rs`) ALSO runs this check each loop and raises `STOCK_EXPIRY` alerts (key `lot:<lot_id>:<zone>`, resolved when qty reaches 0) using `alerts.expiry_lead_days`.
* `GET /api/production/batches/{id}/trace` → traceability / recall view: batch, materials used (+ GRN lot numbers & suppliers via `lot_id`), QC tests, QR sets with their trips (trip_no, vehicle, gate pass, status), totals loaded / delivered / returned / damaged / still in stock.
* `GET /api/production/summary?from=&to=` → `{days:[{date,batches,crates,bottles,blow_good,blow_scrap,scrap_pct}], by_product:[…], material_usage:[{raw_material_id,name,qty}]}`.
* **CSV export**: `?format=csv` on `GET /api/warehouse/stock`, `/api/warehouse/ledger`, `/api/production/batches`, `/api/production/blow-runs`, `/api/raw-materials` returns `text/csv; charset=utf-8` with `Content-Disposition: attachment` (header row, max 50 000 rows). **CSV-injection defence**: any cell starting with `= + - @ \t \r` is prefixed with `'`. The frontend downloads via authenticated `fetch` → Blob.
**Owner C**: `GET /api/loading/trips/{id}/gate-pass` and `/api/qr/batches/{id}/sheet` are the print sources. `GET /api/loading/summary` → `{open_trips, on_road, settled_today, crates_dispatched_today}`.

## 13c. Start-up modes (Lead, `src/config.rs`)
`--mode local|domain` (required unless `--localhost`). `--localhost` (testing): forces bind to `127.0.0.1`, mode `local`, plain HTTP unless `--tls-cert/--tls-key` are given (browsers treat `http://localhost` as a secure context, so the Secure cookie, camera and service worker still work). Not allowed with `--mode domain`. QR URLs then use the request Host (`http://localhost:<port>`).

---------------------------------------------------------------------------------------------------------------------
## 15. Realtime events (Lead: `src/events.rs`, `migrations/0007_events.sql`) – pages refresh instantly

**How it works.** DB triggers `pg_notify` a *topic name only* after every COMMITTED change (rollbacks emit nothing). A listener
coalesces bursts (150 ms) and fans out over **SSE** `GET /api/events?page=<page>[&topics=a,b]`. Events never carry row data – the page
re-fetches through its normal authorised REST calls. The stream is filtered twice: (1) by the user's `allowed_pages` (a user never receives
topics of pages they cannot open) and (2) by the **page subscription** (`?page=`), so a page is only woken by events it actually renders.
Errors: unknown page `400`, page not permitted `403`, > 8 streams/user `429`. The stream ends with `session_ended` when the key is revoked/expired/user deactivated (checked every 30 s).

**Wire format** (use `fetch` + `ReadableStream` with `X-Auth-Key`; `EventSource` cannot send headers and we never put the key in a URL):
`event: hello` · `event: change` `data:{"id":12,"topics":["stock","qr"],"ts":"…"}` · `event: resync` (listener/stream lagged – refresh everything) · `event: session_ended` (go to login) · `: ping` every 20 s.

**Topics**: `stock, materials, production, qc, qr, trips, jars, maintenance, alerts, master, users, settings, admin`.

| Page (`?page=`) | Subscribed topics | On event → refresh (only these regions; never reset open forms/modals) |
|---|---|---|
| `dashboard` | stock, materials, production, qc, trips, jars, maintenance, alerts, users | KPI tiles + lists (`GET /api/dashboard`) |
| `raw-materials` | materials, stock, master, alerts, users | materials table + reorder list + open PO/GRN list (keep an open PO/GRN form untouched) |
| `production` | production, stock, qc, materials, master, alerts, users | batch list, blow runs, water-log status (“hourly due”), BOM availability check of the open form (`/bom-check`) |
| `qc` | qc, production, alerts, users | pending-QC queue + the open batch detail (if the open batch was decided elsewhere, show a banner “already decided by X” and disable buttons) |
| `warehouse` | stock, production, qc, master, alerts, users | zone summary + current stock/ledger view (keep the current filter/page) |
| `qr-labels` | qr, qc, production, alerts, users | batch sets list / sheet preview (QC status badges flip QUARANTINE→AVAILABLE live) |
| `loading` | qr, trips, qc, stock, master, alerts, users | open trip + its items, capacity bar, scanned-set lookup status (e.g. a set just approved by QC becomes loadable; a set loaded by another phone shows `RESERVED` and the add button disables), trips list |
| `jars` | jars, stock, master, alerts, users | summary + customer balances/overdue list |
| `maintenance` | maintenance, alerts, users | schedules + vehicle docs |
| `admin` | admin, settings, users, master, alerts | active admin tab only (users / sessions / audit / settings / master) |
| `profile` | alerts, users, settings | subscription list |
| all pages | `alerts` → bell badge `GET /api/notifications/inbox/unread-count`; `users` → re-fetch `/api/me`: if `allowed_pages` changed rebuild the nav, if the current page is no longer allowed redirect to the first allowed page; `resync` → refresh everything; `session_ended` → clear cookie, go to `login.html` | |

**Client rules (`js/events.js`, owner D, used by every page)**
* API: `ERP.live.start(page, {topicName: handler, …, '*': handler})`; handlers are debounced 300 ms per topic and run at most once concurrently (coalesce while a refresh is in flight).
* Reconnect with exponential back-off (1 s → 30 s) + jitter; after every reconnect call every handler once (events may have been missed). Pause when `document.hidden` (close the stream) and resume + refresh on `visibilitychange`; also on `online`.
* While a modal/scanner/form field is focused, refresh only list regions – never `innerHTML` the form. After a refresh the user's scroll position, filters, tab and selection are preserved.
* The page that made a change also receives its own event; handlers must be idempotent and cheap (a refresh right after the actor’s own action is fine).
* Show a small “Live”/“Reconnecting…” indicator (icon + text) in the app bar.

---------------------------------------------------------------------------------------------------------------------
## 14. Frontend contract (Owners D, E – later phase)

Static files in `./static/`, served by the same origin. **CSP is `script-src 'self'`: no inline `<script>`, no `onclick=` attributes** – use `addEventListener` in modules. All dynamic text must be inserted with `textContent` or the shared `esc()` helper (the only HTML built from strings is trusted SVG from `/qr` endpoints). Service worker must **never cache `/api/*`**. No external CDNs/fonts.
Pages: `index.html, login.html, setup.html, dashboard.html, admin.html, profile.html, raw-materials.html, production.html, qc.html, warehouse.html, qr-labels.html, loading.html, jars.html, maintenance.html`. Shared: `css/styles.css`, `js/api.js` (cookie→header, 401→login redirect, error→toast), `js/events.js` (section 15), `js/nav.js`, `js/ui.js`, `js/jsQR.js` (vendored via `npm pack jsqr`), `sw.js`, `manifest.json`.
