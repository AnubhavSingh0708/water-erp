# Neersagar ERP – API & Behaviour Contract (v1)

This file is the **single source of truth** shared by backend and frontend agents. Backends implement exactly this;
deviations go in `docs/notes-<owner>.md` and are reconciled by the Lead before the frontends start.

Owners: **A** = auth/admin/infra/seed, **B** = materials/production/QC/warehouse/ref/dashboard, **C1** = QR/loading/dispatch/settlement, **C2** = jars/maintenance/notifications/push.
Schema: `migrations/0001..0010` (Lead-owned). Lead owns 0001-0010 (already applied). **Agent migrations: A = `0011_*.sql`, B = `0012_*.sql`, C1 = `0013_*.sql`, C2 = `0014_*.sql` (Lead also added 0015_godowns)** – nothing else, and only if truly needed.

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
| 409 | `RESERVED_STOCK` | write-off / scrap transfer / negative adjustment / count post / godown transfer would take FG_READY below the crates reserved by open LOADING trips; `details:{batch_id,location_id,reserved,trips:[{trip_no,crates}]}`; never overridable (not by `confirm`, not by Admin) – remove the crates from the trip first |
| 409 | `OVER_PRODUCED` | a positive adjustment / count surplus would put more crates of a batch on record than the batch produced (`details:{batch_id,batch_no,produced,on_record,adding}`); never overridable by `confirm`, dry runs return it too |
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
  PRODUCT stock is in **crates** (jars for `RETURNABLE`, `units_per_crate = 1`).
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
  * `GET /api/loading/availability?product_id=` → `[{product_id,product_name,kind,batch_id,batch_no,expiry_date,qc_status,fg_ready,reserved,available}]` ordered FEFO. `reserved` = Σ crates of items in **all** trips with status LOADING for that batch **including the caller's own trip** (`stock::fg_reserved`); `available = fg_ready − reserved` (`stock::fg_free`). Adding to an existing item must check the *increment* against `available`. Only APPROVED batches (stock in `FG_READY`) are listed; when `loading.strict_qc_lock=false`, QUARANTINE batches are listed too with `qc_status` so the UI can badge them (never REJECTED).
  * `POST /api/loading/trips/{id}/items` body one of: `{product_id, crates}` (server allocates FEFO across batches, may create several items; adding to an existing (trip,batch) item increments it) · `{batch_id, crates}` (explicit batch) · `{qr, crates?}` (only when QR feature on; as in §10). Rules inside one tx: trip `LOADING`; batch row `FOR UPDATE` (serialises concurrent loaders on the same batch); `crates ≤ available` else `INSUFFICIENT_STOCK` (`details.available`); QC rule (`QC_LOCKED` when batch not APPROVED and strict); capacity → `CAPACITY_EXCEEDED`.
  * Gate pass / settle / void work identically for bulk items (no set-state transitions when `qr_set_id IS NULL`). ItemObj gains nullable `code`,`set_label` (null for bulk).
* The warehouse, production and QC modules never depend on QR sets. QC approve/reject only touches `qr_crate_sets` rows if any exist.

### B. Additional stock documents (tables in migration 0009). Documents are never deleted; the ledger is never edited. **Mistakes are corrected by VOID / reversing ledger rows that carry a reason and are audited.**
| Doc | Owner/page | Endpoints (all POST bodies `deny_unknown_fields`) |
|---|---|---|
| **Dispatch note** (walk-in / direct sale of finished goods – quantities only, no prices/tax/payment) | C · `loading` | `POST /api/loading/dispatches {customer_id?|party_name, vehicle_no?, note?, lines:[{product_id,crates}|{batch_id,crates}]}` → FEFO from `FG_READY` (`DISPATCH` ledger), number `DN-yymmdd-0001`; needs `customer_id` or `party_name` · `GET /dispatches` `{items,total}` · `GET /dispatches/{id}` (printable) · `POST /dispatches/{id}/void {reason}` (restocks the same lots, `DISPATCH_VOID`; within `loading.void_window_hours` or Admin) |
| **Gate-pass void** (wrong load / vehicle never left) | C · `loading` | `POST /api/loading/trips/{id}/void {reason, confirm:true}` only `DISPATCHED` and not settled, within `loading.void_window_hours` (Admin unlimited): restocks every item's lot (`GATE_PASS_VOID`), sets (if any) back to AVAILABLE, trip → `CANCELLED` with `void_reason` |
| **GRN void** | B · `raw-materials` | `POST /api/raw-materials/grns/{id}/void {reason}` – only if each lot is still fully in `RM_STORE` (else `409 INSUFFICIENT_STOCK` "already consumed – use a supplier return or adjustment"); reverses ledger (`GRN_VOID`), rolls back `po_lines.qty_received`/PO status, GRN `status=VOID` |
| **Supplier return** | B · `raw-materials` | `POST /api/raw-materials/supplier-returns {supplier_id, grn_id?, note?, lines:[{raw_material_id, lot_id, qty_stock, reason}]}` – stock out of `RM_STORE` (`RTV_OUT`), `RTV-yymmdd-0001`; `GET /api/raw-materials/supplier-returns` list · `GET /api/raw-materials/supplier-returns/{id}` detail (printable) |
| **Write-off / destroyed stock** | B · `warehouse` | `POST /api/warehouse/write-offs {item_type,item_id,zone,lot_id,qty,reason_code,note,confirm:true,dispose?:bool}` – `reason_code ∈ BREAKAGE,EXPIRED,CONTAMINATED,SPILLAGE,QC_REJECT,THEFT_LOSS,TRANSIT_DAMAGE,OTHER`; note ≥ 5 chars; moves stock to `SCRAP_YARD` (`WRITE_OFF`) unless already there; `dispose:true` (or write-off from SCRAP_YARD) removes it from the books (`DISPOSE`). Not allowed from `QC_QUARANTINE` (QC owns that). `WO-yymmdd-0001`. `GET /write-offs` `{items,total}` (filter reason/from/to) + `?format=csv` · `GET /api/warehouse/write-offs/{id}` detail |
| **Physical stock count** | B · `warehouse` | `POST /api/warehouse/counts {zone, note?}` snapshots expected qty per (item,lot) → DRAFT · `PUT /counts/{id}/lines {lines:[{line_id,counted_qty,note?}]}` · `GET /counts/{id}` (shows `variance = counted − expected`, `% `) · `POST /counts/{id}/post {confirm:true}` posts `ADJUST` rows for variances (reason "stock count CNT-…"); balances changed since the snapshot are recomputed at post time (variance applies to the **current** balance: `delta = counted − current`) · `POST /counts/{id}/cancel`. Lines with large variance (> `entry.large_adjust_pct`) require the line `note`. |
Printable sources (the UI prints them with `@media print`): GRN, supplier return, dispatch note, write-off, gate pass, count sheet.

### C0. Reservation rule (shared helpers in `stock.rs`: `fg_reserved`, `fg_free`, `allocate_free_fefo`)
Crates in a LOADING trip are reserved. **Every flow that takes finished goods out of FG_READY must respect reservations**: dispatch notes and jar counter `issue` **must** use `stock::allocate_free_fefo` (locks batch rows, FEFO, APPROVED only); trip items use `fg_free`; write-offs, scrap transfers and negative adjustments on a PRODUCT lot in `FG_READY` whose quantity would drop below the reserved amount return `CONFIRM_REQUIRED` ("N crates are reserved by open loading trips") – and on confirm the affected trip's gate pass will fail with `INSUFFICIENT_STOCK`, which is acceptable and visible. No module may compute availability itself.

### C. Mistake-proofing patterns (every module must follow)
0. **Every request struct that can return `CONFIRM_REQUIRED` MUST declare `#[serde(default)] confirm: bool`** (otherwise `deny_unknown_fields` rejects the confirmed resend). Idempotency fingerprints the whole body, so a resend with `confirm:true` under the same key is a new request (the earlier 409 is not stored); only 2xx responses are replayed; `?dry_run=1` bypasses idempotency; a key left pending by a timeout/crash is reserved for 10 minutes (§0.2-H).
1. **`confirm` flag** – suspicious but possible entries return `409 CONFIRM_REQUIRED` with `details.warnings:[…]`; the client shows them and re-sends the same body with `"confirm": true`. Mandatory cases: write-off (always), count post (always), adjustment/transfer larger than `entry.large_adjust_pct` % of the balance (or Δ ≥ 1000 units), gate-pass void (always), GRN with qty > 3× the remaining PO line or > 10× the median of the last 10 GRNs of that material, GRN whose `(supplier, challan_no)` already exists → `DUPLICATE` (never confirmable – void the old one), batch `crates` > 2× the max of the last 10 batches of that product, settlement where `damaged` > `settle.damage_warn_pct` % of loaded, blow run with `scrap_pct > production.scrap_warn_pct` (requires a non-empty `notes`, plus a `warnings` entry in the 201 response).
2. **Soft warnings** – 2xx bodies for creates may include `"warnings":["…"]` (e.g. water reading out of range, batch within 10 % of expiry, loading ≥ 95 % of capacity). UI shows them as a non-blocking banner.
3. **`?dry_run=1`** on `POST` of: GRN, batch creation, adjustments, transfers, write-offs, trip items, gate-pass, settle, dispatch, count post → runs the full logic in a transaction that is **rolled back**, returns `200 {"dry_run":true,"effects":[{"item","zone","delta","after"}…],"warnings":[…]}` or the same error the real call would give. The UI uses it to show a *review screen* ("This will deduct 40 crates of 1 L from batch B-…") before the real submit.
4. **`Idempotency-Key`** header (implemented in `routes/mod.rs`): same user+key ⇒ first response replayed, handler not re-run. Frontend generates one key per user action and **reuses it on retries**.
5. **Business dates** use `util::plant_today()` (plant local time, setting `plant.utc_offset_minutes`), never `CURRENT_DATE`.
6. **Stale-screen defence** – state machines + row locks return `409 INVALID_STATE` (hint: "someone else changed this – review and retry"); list/detail responses include `updated_at` (or status) so the UI can show "changed by someone else".
7. **Error envelope additions**: `error.hint` (actionable next step, server-generated per code, see `error.rs::hint_for`) and response header `X-Request-Id` (UI shows "Ref: …" on 5xx so admins can find the log line). `503 UNAVAILABLE` = DB/pool problem, nothing was saved. Request timeout 30 s → 503.
8. **Number inputs**: server rejects NaN/negative/over-precision (see `validate.rs`); client also clamps; large values use `CONFIRM_REQUIRED`, never silently accepted.


### E. Plant-specific product model (migration 0010) – supersedes earlier wording about "JAR20L"/20 L only
* **Product `kind`**: `BOTTLED` (single-use PET) | `RETURNABLE` (reusable container, e.g. 20 L jar; any size). Product fields (admin CRUD + `/api/ref/products`): `code,name,kind,size_ml,units_per_crate,crates_per_pallet,shelf_life_days,min_stock_crates,preform_material_id,bottle_material_id,max_cycles,is_active`.
  * **BOTTLED**: blown from a **preform** (`preform_material_id`, category `PREFORM`) into an empty bottle (`bottle_material_id`, category `EMPTY_BOTTLE`). BOM contains `EMPTY_BOTTLE` (BLOW_FLOOR) + cap + label + sleeve + shrink + carton + minerals/chemicals.
  * **RETURNABLE**: **never consumes preforms or empty bottles** – validation: `preform_material_id`/`bottle_material_id` must be null and its BOM may not contain `PREFORM`/`EMPTY_BOTTLE`. A batch consumes `crates` containers from `JAR_CLEAN` (the product's own stock) plus BOM consumables (cap/seal, label, minerals). `units_per_crate = 1`, crates = containers.
* **Standard pack configuration**: `units_per_crate` (bottles per crate) and `crates_per_pallet`. Responses never change units (always crates); the UI shows `pallets + loose crates` (`floor(c / cpp)` + `c % cpp`) and accepts "pallets + crates" inputs; vehicle capacity stays in crates and the UI also shows it in pallets. Default QR crate-set size = `crates_per_pallet` when set.
* **Mineral/chemical dosing**: BOM lines have `basis`: `PER_CRATE` (qty per crate) or `PER_1000L` (qty in stock units per 1000 L of water). Litres in a batch = `crates × units_per_crate × size_ml / 1000`; required qty = `qty × litres / 1000` for `PER_1000L`. Batch/`bom-check` responses show `basis` and litres. Minerals are consumed FEFO from `RM_STORE` like any raw material.
* **Terminology in UI**: "Preform" (the small test-tube-shaped PET piece that is blown into a bottle), "Blow molding", "Crate", "Pallet", "Returnable container / jar".
* **Container fleet lifecycle** (table `jar_fleet_events`; every flow that destroys/loses/buys/washes containers MUST write one event row in the same tx):
  `NEW_PURCHASED` (new containers join the fleet → `JAR_EMPTY`) · `WASHED` (qty washed, `JAR_EMPTY→JAR_CLEAN`) · `DESTROYED` with `reason_code ∈ WASH_REJECT,CRACKED,LEAKING,DAMAGED_RETURN,AGE_RETIRED,LOST,OTHER` and `location ∈ PLANT_EMPTY,PLANT_CLEAN,CUSTOMER,VEHICLE`.
  Trip settlement `damaged` ⇒ `DESTROYED/DAMAGED_RETURN/VEHICLE` (or CUSTOMER), `lost` ⇒ `DESTROYED/LOST/CUSTOMER`; `receive-empties` damaged ⇒ `DAMAGED_RETURN`.
  New/changed `/api/jars/*` endpoints (page `jars`; supersede §11 where they overlap):
  * `GET /fleet?product_id=&from=&to=` → `{product, fleet:{total, plant_empty, plant_clean, filled_ready, filled_quarantine, with_customers}, period:{fills, delivered, empties_returned, new_purchased, washed, destroyed_total, destroyed_by_reason:{CRACKED:n,…}}, destroy_rate_pct (destroyed/fills), avg_cycles_estimate (lifetime fills ÷ (fleet total + lifetime destroyed)), max_cycles|null, retirement_pct|null, alert:"OK"|"HIGH_DESTROY_RATE"|"NEAR_RETIREMENT"}` (`fills` = Σ crates of that product's batches). Fleet total counts containers in vehicles on DISPATCHED trips too.
  * `POST /receive-new {product_id, qty, supplier_id?, note?}` (new containers → `JAR_EMPTY`, ledger `JAR_PURCHASE`, event NEW_PURCHASED) · `POST /wash {product_id, qty, rejected?:int}` (washed qty `JAR_EMPTY→JAR_CLEAN`; `rejected` ones go to `SCRAP_YARD` as DESTROYED/WASH_REJECT; one WASHED event) · `POST /destroy {product_id, qty, location:"PLANT_EMPTY"|"PLANT_CLEAN"|"CUSTOMER", customer_id?, reason_code, note, confirm:true}` (replaces `/write-off`: PLANT_* ⇒ stock to `SCRAP_YARD`; CUSTOMER ⇒ `jar_ledger` WRITE_OFF; never below zero) · `GET /destroyed?product_id=&reason=&from=&to=` `{items,total}` events.
  * `JAR_FLEET_ALERT` (worker, C2): raised when destroyed/fills over the last 30 days > `jars.destroy_warn_pct`, or `retirement_pct ≥ 90`; resolved when clear.
* Seed (A) must create: one BOTTLED 500 ml (24/crate, 60 crates/pallet) and 1 L (12/crate, 80/pallet) SKU with preform+bottle mapping, minerals as `PER_1000L`, and a RETURNABLE 20 L SKU with `max_cycles=50`, a fleet (empty/clean/filled/with customers) and a few DESTROYED events.


### F. Multiple godowns / locations (migration 0015) – inventory is kept PER GODOWN
One inventory, partitioned by location: `stock_balances` and `stock_ledger` carry `location_id` (every godown has its own balances, lots, FEFO queue and ledger). Rows that exist before a godown is chosen belong to `MAIN` (`00000000-0000-0000-0000-000000000001`, the production plant). `TRANSIT` (`…0002`) is a system location holding goods between godowns – never selectable, never listed.
* **Location object**: `{id, code, name, kind: PLANT|GODOWN, address, city, contact_person, phone, is_production_site, is_active}`. Admin CRUD `/api/admin/locations` (A; soft delete; cannot deactivate a godown that still holds stock → 409 `IN_USE`; MAIN cannot be deactivated). Users get `allowed_location_ids` (A: in user create/update/list; `[]` = all godowns; must reference active non-TRANSIT locations). `GET /api/ref/locations` (B) = active selectable godowns **the caller may use**; `?all=1` returns only `{id,code,name,kind}` of every active godown (destination picker for transfers).
* **Resolving the godown**: every stock-touching endpoint accepts optional `location_id` (query on GET, body on POST) and resolves it with `util::resolve_location` (validates active/not TRANSIT and `AuthUser::require_location` → 403). Omitted ⇒ the user's single allowed godown if restricted to exactly one, else MAIN. **UI always sends it explicitly.** Reads may use `location_id=all` (sum over the caller's allowed godowns; rows still show `location_id`/`location_name`).
* **Who is godown-aware** (all use `stock::*_at(location, …)` / `Move::at(location)`; never aggregate across godowns by accident):
  * **B**: GRNs (`location_id` = receiving godown, stored on `grns`), supplier returns, write-offs, adjustments, in-godown zone transfers, stock counts, `GET /warehouse/stock|summary|ledger|expiring|stock/item` (filter + by-location breakdown), blow runs & production batches & QC (only at `is_production_site` locations; raw materials are consumed from that site's `RM_STORE`/`BLOW_FLOOR`; batch output goes to that site's `QC_QUARANTINE`; default MAIN), dashboard (per-godown low-stock/FG numbers + `?location_id=`), reorder engine (**totals across all godowns** of `RM_STORE` vs min, suggestion mentions which godowns hold stock).
  * **C1**: trips carry `location_id` = **source godown** (trip create body; `loading_trips.location_id`); availability, add-item, reservations (`fg_free_at`, `allocate_free_fefo_at`), gate pass deduction, dispatch notes, gate-pass/dispatch void and settlement restock all operate on the trip's/note's godown. Vehicle capacity unchanged. One open LOADING trip per vehicle still applies.
  * **C2**: jar stock zones are per godown: `receive-new`, `wash`, `destroy`, `receive-empties`, counter `issue`, summary and `/fleet` accept `location_id` (fleet events store `location_id`; `/fleet` shows totals and `by_location[]`).
* **Godown transfers** (`/api/godowns/*`, page `warehouse`, owner B). Goods never move directly between godowns; they pass through TRANSIT so nothing disappears:
  * `POST /transfers {from_location_id, to_location_id, lines:[{item_type,item_id,lot_id,zone,qty}], vehicle_id?, vehicle_no?, driver_name?, note?, confirm?}` → status `IN_TRANSIT`, number `TF-yymmdd-0001`; stock leaves the source zone into TRANSIT (`GODOWN_TRANSFER_OUT`, expiry carried). Rules: `from ≠ to`, both active selectable, caller allowed at **from**; zones allowed `RM_STORE, FG_READY, JAR_EMPTY, JAR_CLEAN, BLOW_FLOOR` only (never `QC_QUARANTINE`/`SCRAP_YARD`); PRODUCT lots must belong to APPROVED batches; FG quantities respect reservations (`fg_free_at`, batch row locked); ≤ 200 lines; `?dry_run=1` supported.
  * `POST /transfers/{id}/receive {lines:[{line_id,qty_received,qty_damaged,note?}], confirm?}` – caller allowed at **to**; every line exactly once; `received + damaged ≤ sent`. Received → destination same zone (`GODOWN_TRANSFER_IN`); damaged → destination `SCRAP_YARD` (`TRANSIT_DAMAGE`); shortage = `sent − received − damaged` > 0 → removed from TRANSIT (`TRANSFER_SHORT`), requires `confirm:true` and a line `note`, audited. Only from `IN_TRANSIT` (else `INVALID_STATE`); status `RECEIVED`.
  * `POST /transfers/{id}/cancel {reason, confirm:true}` – only `IN_TRANSIT`, caller allowed at **from** (or Admin): stock returns to the source zone. `GET /transfers?status=&location_id=&from=&to=` `{items,total}` (visible if the caller may use either end) · `GET /transfers/{id}` (printable, lines with sent/received/damaged/short) · transfers `IN_TRANSIT` > 48 h raise a dashboard warning.
  * Invariants (tests): TRANSIT balances == Σ(sent − received − damaged − short) of IN_TRANSIT transfers; Σ over all godowns of an item is conserved except documented losses; per-(location,item,zone,lot) `balance == Σ ledger`.
* **Access**: a user restricted to godowns can neither read nor write other godowns' stock/docs (403 on explicit `location_id`; lists silently filter). Admin sees all.
* **Realtime**: `godown_transfers`/lines → topic `stock`; `locations` → `master`.

### G. QA amendments (pen-test pass)
* **`?dry_run=`** has ONE definition everywhere (handlers and the idempotency middleware): present and not one of `0`, `false`, `no`, `off` (any case, percent-decoded). So `dry_run=1`, `true`, `yes`, `True`, a bare `?dry_run` and any unknown spelling are dry runs (fail safe); `dry_run=0` is a real, idempotent write. Dry runs never touch the idempotency store and leave no ledger, audit, alert, jar or fleet rows.
* **Idempotency**: a committed 2xx whose body is larger than 512 KB is delivered in full, but replays answer `{"idempotent_replay_truncated":true,...}` (the handler never re-runs). Failing to remember a response never turns a committed write into an error.
* **QR sets are godown-scoped** (the batch's godown): read, sheet, lookup, SVG, search, create and delete answer `403` for a user who may not use that godown; `GET /qr/sets` silently filters.
* **Products**: `kind` cannot change once batches, any stock row, customer jar holdings, fleet events, trip items or dispatch lines exist (409 `IN_USE`).
* **Godowns**: deactivation is also refused (409 `IN_USE`, `details.open_trips`) while a trip of that godown is `LOADING` or `DISPATCHED`.
* **Free text** (names, notes, reasons): control characters, bidi overrides (U+202A-E, U+2066-9) and U+FEFF are rejected (422); ZWJ/ZWNJ are allowed.
* A numeric overflow in the database (NUMERIC(18,4)) answers `400 BAD_REQUEST`, never `500`.

### H. Gap-closing amendments (W1)
* **`GET /api/qr/batches?q=&limit=&offset=`** (page `qr-labels` or `loading`; feature gate as all `/api/qr/*`) → `{items:[{id,batch_no,product_id,product_name,crates,qc_status,expiry_date,location_id,location_name,allocated_crates,free_crates}],total}`. Batches that can still get labels: `qc_status` `QUARANTINE` or `APPROVED` (never `REJECTED`), godown-scoped to the caller (a restricted user only sees batches of their godowns), newest first. `allocated_crates` = Σ `original_crates` of the batch's root sets, `free_crates = crates − allocated`. `q` (≤ 100 chars) is a bound `ILIKE` on batch no / product name / product code. Lets label users without the `production` page choose a batch.
* **`GET /api/production/preform-stock?preform_material_id=&location_id=`** (page `production`) → `{material_id,name,unit,available,min_stock,location_id,location_name}`: the `RM_STORE` quantity of that PREFORM material at the resolved godown (same resolution and 403 rules as blow runs: production sites only, omitted ⇒ caller's default). `422` for an unknown / inactive / non-PREFORM material. Lets operators without the `raw-materials` page see whether enough preforms exist before a blow run.
* **Finished goods are capped at what was produced.** A positive `PRODUCT` adjustment in `FG_READY`, and a count post whose line raises a lot in `FG_READY`, are refused with `409 OVER_PRODUCED` when `Σ qty` of that batch lot in `FG_READY` + `QC_QUARANTINE` over **all** godowns (TRANSIT included) plus the addition would exceed `production_batches.crates`. Hard error: not overridable by `confirm`, also returned by `?dry_run=`. The batch row is locked `FOR UPDATE` first (same row loaders / QC lock), then the quantity is read, so concurrent corrections cannot jointly exceed the cap. Crates already shipped (on the road, delivered) or scrapped are not counted, so the cap is a floor-level guard against typos and double entries, not a full reconciliation; a real shortfall in the production record is fixed on the batch, not by adjusting stock. Known gap: restock paths (settlement returns, gate-pass void, dispatch-note void) put crates back into `FG_READY` and are never blocked (that would strand trips and notes), so e.g. a batch of 100 with 10 crates on a DISPATCHED trip allows a "found" +10 (90 + 10 = 100 on record) and the later void/return takes it to 110; counting outstanding (loaded, not yet settled) crates toward the cap is a possible follow-up.
* **Fleet integrity.** Containers that vanish from the plant's jar zones write a `jar_fleet_events` row in the same transaction: a **negative** adjustment of a `RETURNABLE` product in `JAR_EMPTY` / `JAR_CLEAN`, and a **negative variance** of a count post in those zones → `DESTROYED`, `reason_code OTHER`, `location PLANT_EMPTY` / `PLANT_CLEAN`, `qty` = containers lost, `note` `adjustment` / `stock count CNT-…`; jars reported **damaged** / **short** when a godown transfer is received → `DESTROYED` with `DAMAGED_RETURN` / `LOST` (note `godown transfer TF-…`). A **surplus** (positive adjustment / count variance) writes nothing: a surplus is not a purchase, so no `NEW_PURCHASED` is invented. Dry runs and refused calls leave no event. `GET /jars/fleet` `fleet.total` is derived live from stock balances + customer holdings + containers on DISPATCHED trips (never from events), so it always agrees with stock (pass `location_id=all` for the whole fleet; the default is the caller's godown). There is deliberately **no** `unexplained_delta`: the fleet has no recorded opening balance (seeded / pre-system stock has no event), so `total − (NEW_PURCHASED − DESTROYED)` would only be a constant baseline, not a signal. Filled-jar (`FG_READY`) adjustments of a returnable lot are finished-goods corrections and write no fleet event.
* **Idempotency and the 30 s timeout.** A key whose request was cut off (request timeout, or the process died mid-request) has no stored response. It is **not** freed after 35 s any more: the timeout can fire right after the handler's transaction committed, and re-running it would write twice. A retry with that key answers `409 IN_PROGRESS` ("an earlier attempt timed out ... not known whether it was saved") (`error.details.outcome_unknown: true` when the first attempt is older than the request timeout, so a client can tell it from a genuinely in-flight duplicate) until the key is older than **10 minutes** (`routes::STALE_KEY_MINUTES`), after which it is taken over and the request runs again. The response is stored by a detached task, so a caller that disappears right after the handler finished still leaves a replayable key. Trade-off: a request that was cut off *before* it committed (nothing saved) also blocks a retry **with the same key** for 10 minutes; the user simply repeats the action (a new action gets a new key) after checking the list. Clients must not blindly retry `IN_PROGRESS` for ever: after the back-off, show "check the list, then repeat".
* **Zombie SSE streams / dead peers.** The SSE ping is every **15 s**. Listening sockets are opened with `SO_KEEPALIVE` (`server::bind_listener`; accepted sockets inherit it), so the host's `net.ipv4.tcp_keepalive_time/intvl/probes` apply to **idle** half-open connections (set `tcp_keepalive_time=60`, `tcp_keepalive_intvl=10`, `tcp_keepalive_probes=3`: reaped in about 90 s). A stream that is being pinged is never idle: its unacknowledged pings sit in the send buffer and keep-alive does not fire, so a dead phone is only dropped when the kernel gives up retransmitting (`net.ipv4.tcp_retries2`, default 15 = about 15 minutes; **set `tcp_retries2=8`, about 100 s**) - that sysctl, not the ping, is what bounds a zombie SSE stream. HTTP/2 connections (TLS mode) are additionally probed with PING frames every 20 s (10 s timeout), which drops a dead h2 peer in about 30 s. No application-level self-heal: the application cannot observe a dead peer before the kernel aborts the connection (a write into the send buffer succeeds), so there is no "last write failed" signal to evict on, and evicting the user's oldest stream at the cap would make 9 legitimate tabs evict each other in a reconnect loop - accepted. Crate need for a real fix: `socket2 = "0.6"` as a direct dependency (per-socket `TCP_USER_TIMEOUT` and keep-alive time/interval).

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
  `/suppliers`, `/raw-materials`, `/products`, `/customers`, `/routes`, `/vehicles`, `/equipment`, `/maintenance-schedules`, `/locations` – each with `/api/admin/<entity>` (list, create) and `/api/admin/<entity>/{id}` (get, put, delete), i.e. `/api/admin/suppliers/{id}`, `/api/admin/raw-materials/{id}`, `/api/admin/products/{id}`, `/api/admin/customers/{id}`, `/api/admin/routes/{id}`, `/api/admin/vehicles/{id}`, `/api/admin/equipment/{id}`, `/api/admin/maintenance-schedules/{id}`, `/api/admin/locations/{id}`.
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
  * `POST /batches {product_id,crates,shift,produced_on?,water_log_id?,notes?}` → one transaction: for each BOM line consume `qty_per_crate × crates` from `source_zone` (FEFO, `BATCH_CONSUME`, writes `batch_material_usage`); for `RETURNABLE` products also consume `crates` jars of that product from `JAR_CLEAN`; output `+crates` of PRODUCT into **`QC_QUARANTINE`** lot=`batch.id` with `expiry = mfg + shelf_life_days` (`BATCH_OUTPUT`). `qc_status=QUARANTINE`. Shortage → `409 INSUFFICIENT_STOCK` with `details.shortages:[{raw_material_id,name,required,available}]` (pre-checked for **all** lines). Raises `QC_PENDING`. `batch_no` = `<plant_code>-B-yymmdd-0001`.
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
  Jars: for each RETURNABLE product, Σ `customer_jars.delivered` must equal Σ item `delivered` of that product (customers must exist/active). Each line writes `jar_ledger` `ISSUE_FILLED(+delivered)`, `RETURN_EMPTY(−empties_collected)`, `DAMAGED(−damaged)`, `LOST(−lost)` (rows with qty 0 skipped) with `trip_id`; `empties_collected` adds to `JAR_EMPTY` stock (`JAR_RECEIVE`), `damaged` jars to `SCRAP_YARD`. A customer's holding may not go negative (`409 BAD_REQUEST`). Trip → `SETTLED`; alert `TRIP_SETTLED`; audit.

## 11. 20 L jars  (Owner C, page `jars`)

* `GET /api/jars/summary` → `[{product_id,name,plant_empty,plant_clean,filled_ready,filled_quarantine,with_customers,overdue_customers}]`
* `GET /api/jars/customers?product_id=&overdue=1&q=` → `[{customer_id,name,phone,product_id,holding,oldest_outstanding_date,days_outstanding,overdue:bool}]` (FIFO match of issues vs returns; overdue when `days_outstanding > jars.overdue_days` and holding > 0) · `GET /customers/{id}/ledger` `{items,total}`.
* `POST /opening {customer_id,product_id,qty}` · `POST /issue {customer_id,product_id,qty}` (counter sale: FEFO from `FG_READY`, `ISSUE_FILLED`) · `POST /receive-empties {customer_id,product_id,qty_good,qty_damaged}` (good → `JAR_EMPTY`, damaged → `SCRAP_YARD`; ledger `RETURN_EMPTY`/`DAMAGED`) · `POST /wash {product_id,qty}` (`JAR_EMPTY→JAR_CLEAN`, `JAR_WASH`) · ~~`POST /write-off`~~ (removed – replaced by `POST /destroy`, §0.2-E; there is no `/api/jars/write-off` route). Filling = a normal production batch of the RETURNABLE product (consumes `JAR_CLEAN`).

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
`event: hello` · `event: change` `data:{"id":12,"topics":["stock","qr"],"ts":"…"}` · `event: resync` (listener/stream lagged – refresh everything) · `event: session_ended` (go to login) · `: ping` every 15 s.

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
**Failure handling in the browser (all pages, implemented once in `api.js`/`ui.js`/`drafts.js`):**
1. *Drafts*: every form autosaves to `localStorage` (key per user+page+form, text/number fields only, never the auth key) and restores after reload/login; cleared on success.
2. *Connectivity*: `navigator.onLine` + `/api/health` ping every 10 s (and on failed fetch) drives a sticky banner "Server unreachable – nothing is lost, your entries are kept" (icon + text); SSE `server_restarting` shows "Server restarting…" and reconnects with back-off. Submit buttons stay enabled but show the unreachable message instead of hanging.
3. *Writes*: each user action gets one `Idempotency-Key` (uuid) reused on every retry; automatic retry (network error / 502/503/504 / 429) 3× with back-off, then a persistent inline result "Not sent – Retry" (same key ⇒ safe). Never auto-retry on 4xx. Button shows spinner and is disabled while in flight.
4. *Errors*: show `error.message` + `error.hint`; 422 → inline field errors and focus the first; 5xx/503 → also "Ref: <X-Request-Id>"; `UNAUTHORIZED` → save draft, go to login, return to the page; `INVALID_STATE` → auto-refresh the record and show a "changed by someone else" banner.
5. *Confirm*: a 409 `CONFIRM_REQUIRED` opens a dialog listing `details.warnings` with Cancel / "Yes, continue" (re-sends with `confirm:true`, same key). Irreversible actions always use an explicit confirm dialog.
6. *Review before commit*: stock-affecting forms (GRN, batch, gate pass, settlement, dispatch, write-off, count post, adjustment) first call the endpoint with `?dry_run=1` and show a review sheet ("effects" list + warnings); the real submit happens only on "Confirm".
7. *Numbers*: `inputmode`, min/max/step from the product config, live pallet↔crate helper, and a visible "Check: you entered N crates = P pallets + L loose" echo to catch typos.

Pages: `index.html, login.html, setup.html, dashboard.html, admin.html, profile.html, raw-materials.html, production.html, qc.html, warehouse.html, qr-labels.html, loading.html, jars.html, maintenance.html`. Shared: `css/styles.css`, `js/api.js` (cookie→header, 401→login redirect, error→toast), `js/events.js` (section 15), `js/nav.js`, `js/ui.js`, `js/jsQR.js` (vendored via `npm pack jsqr`), `sw.js`, `manifest.json`.

---------------------------------------------------------------------------------------------------------------------
## 16. Branding
Company: **Neersagar** (setting `plant.name`, code `NSG`, editable in Admin → Settings; defaults applied by migration 0020). Every page title is `Neersagar ERP – <Page>`; the PWA is named "Neersagar ERP" (short name "Neersagar"); app bar, login, setup, gate pass, dispatch note, GRN, transfer and write-off printouts show the plant name from `/api/ref/settings.plant_name`.
