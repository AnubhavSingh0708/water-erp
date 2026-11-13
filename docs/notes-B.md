# Notes – Owner B (materials, production, QC, warehouse, ref data, dashboard)

Branch: `worktree-agent-a9ddff392f4b1e8cf`. No migration (0012) and no new crates were needed.

## Layout
* `src/routes/ref_data/bh.rs` – shared helpers (envelope-preserving `J`/`Q`/`Id` extractors, `D` decimal input (string or number, never f64),
  `Arg`/`jval`/`paged` parameterised JSON queries (SQL text is `&'static str`), CSV export with injection defence, dry-run effects, doc numbers).
* `materials.rs` + `materials/grn.rs`, `production.rs` + `production/batches.rs`, `qc.rs`, `warehouse.rs` + `warehouse/corrections.rs`,
  `dashboard.rs`, `ref_data.rs`, `workers/reorder.rs` (`run_once`, `evaluate`, `evaluate_materials`, `check_expiry`).
* Tests: `tests/materials_fixtures/mod.rs` (shared fixtures), `materials_flow.rs`, `production_flow.rs`, `qc_flow.rs`, `warehouse_flow.rs` (28 tests).

## Behaviour decisions / deviations from API_REFERENCE.md
1. **Over-receipt** (> 110 % of a PO line) answers `409` with code `BAD_REQUEST` (the endpoint text says "409 BAD_REQUEST"); same for "return more than received".
2. **dry_run** (`?dry_run=1`): the whole logic runs in a transaction that is rolled back. `confirm` is treated as satisfied and the would-be confirm
   warnings are returned in `warnings` (so the review sheet can show them) – hard errors (insufficient stock, QC lock, duplicate challan, validation) are returned as the real call would.
   Document numbers are not consumed (preview strings). `effects` come from the ledger rows the document wrote (by `ref_type/ref_id`).
3. **Reorder engine refinement**: suggested quantity subtracts quantity already **on order** (outstanding qty of OPEN/PARTIAL POs, in stock units);
   when that covers the need no OPEN recommendation is created (LOW_STOCK still fires). A DISMISSED recommendation suppresses a new one for 24 h.
   A GRN resolves the material's `ORDERED` recommendations and re-evaluates the engine for the received materials. Burn counts `BLOW_ISSUE`, `BATCH_CONSUME`, `BATCH_WASTE`.
   Materials with no default supplier use a 7-day lead time.
4. **Batch creation** (`POST /api/production/batches`):
   * extra request fields: `confirm`, `waste:[{raw_material_id, qty, reason}]` (≤ 50 lines, BOM materials only, never EMPTY_BOTTLE; deducted from the same zone, ledger `BATCH_WASTE`;
     soft warning > 5 % of the BOM quantity, `CONFIRM_REQUIRED` > 25 %). Usage rows: one `batch_material_usage` row for the BOM quantity and one for the waste; detail `materials[]` aggregates per material (`qty`, `waste_qty`, `basis`) and `waste[]` lists the waste lines (also in trace).
   * quality gate: no water log in the last 60 min → soft warning "No water log in the last hour"; the most recent (or selected) water log out of range → `CONFIRM_REQUIRED`; crates not a multiple of `crates_per_pallet` → soft info warning; crates > 2× the largest of the last 10 batches → `CONFIRM_REQUIRED`.
   * a product without any BOM line is refused (400). RETURNABLE products ignore PREFORM/EMPTY_BOTTLE BOM lines defensively and consume `crates` containers (item_type PRODUCT) from `JAR_CLEAN` (any lots, FEFO); that line appears in `shortages[]` with `raw_material_id:null`.
   * response = batch detail + `warnings`, `litres`, `lines` (per requirement: required incl. waste, available, short).
5. **Blow runs**: no `confirm`; scrap % above `production.scrap_warn_pct` requires non-empty `notes` (422 on `notes`), and a `warnings` entry is returned. Whole numbers only for issued/good.
6. **Water logs**: out of range = pH or TDS outside the `lab.*` limits; `uv_ok:false` only adds a soft warning. `GET /water-logs/status` also returns `last_out_of_range`.
7. **QC**: batch row `FOR UPDATE` → QR sets (`FOR UPDATE`) → stock (loading's lock order). `tested_at` uses `clock_timestamp()` so "latest test" is well defined. Approve/reject release the *remaining* lot balance; QR sets flipped by plain SQL.
   QC reject of a RETURNABLE batch does not write a fleet DESTROYED event (containers are not necessarily destroyed).
8. **Warehouse corrections** (all inside one tx, batch row of an FG_READY lot locked `FOR UPDATE` BEFORE `fg_reserved` is read):
   * adjustments accept optional `reason_code ∈ COUNT_CORRECTION, ENTRY_ERROR, FOUND, DAMAGE, OTHER` (stored as `[CODE] reason` in the ledger note);
     RAW zones: RM_STORE/BLOW_FLOOR/SCRAP_YARD; PRODUCT: FG_READY/SCRAP_YARD (+ JAR_EMPTY/JAR_CLEAN for RETURNABLE, nil lot). QC_QUARANTINE → 400. FG_READY needs a batch lot of that product; adding needs an APPROVED batch (`QC_LOCKED`).
     RAW lot ids must be a GRN line of the material or a lot the material already holds in some zone (opening/seed stock) – invented ids → 422.
   * "large" = |Δ| ≥ 1000 units, or balance ≤ 0, or > `entry.large_adjust_pct` % of the balance → `CONFIRM_REQUIRED`.
   * transfers: RAW RM_STORE↔BLOW_FLOOR, or anything → SCRAP_YARD (note ≥ 5 chars); never touching QC_QUARANTINE, never PRODUCT → FG_READY.
   * write-offs/transfers of RETURNABLE containers to scrap write a `jar_fleet_events` DESTROYED row (reason mapped from the write-off code; location PLANT_EMPTY for JAR_EMPTY else PLANT_CLEAN).
   * counts: one snapshot per zone (QC_QUARANTINE refused); post recomputes `delta = counted − current balance` under row locks; lines with a variance above `large_adjust_pct` need a line note (422 otherwise); positive variance on FG_READY needs an APPROVED batch.
9. **GRN**: unknown/invented ids → 422 field errors; expiry date in the past → 422; PO of another supplier / material not on the PO / `po_line_id` of another PO → 422. GRN void is allowed only while each lot still has its full received quantity in RM_STORE.
   Supplier returns are capped at (received − already returned) per lot and do not change PO `qty_received`.
10. **Dashboard** honours `alerts::user_may_receive`: `recent_alerts` only contains event types the caller may see; KPI numbers for pages the caller cannot open are `null` and the matching lists are empty.
11. Lists documented as "bare array" (`/api/raw-materials`, `/stock`, POs, recommendations, ref data) are bounded to 500 rows; `?format=csv` bypasses the cap up to 50 000 rows.
12. Extractor rejections (bad JSON, unknown field, bad query, bad id) always use the error envelope (`400 BAD_REQUEST` / `404`).

## Requests to the Lead
* None blocking. Optional: a `batch_material_usage.kind` column would make waste rows self-describing (waste is currently derived from the ledger `BATCH_WASTE` rows).
* Test env tip: set `CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0` – debug-info test binaries filled the 250 GB disk once and took the Postgres clusters down.

## Contract question for the Lead
Write-offs and count posts always need `confirm:true`, and that single flag also covers the "N crates are reserved by open loading trips" warning.
In the normal flow (dry run -> user confirms -> real submit) a loader could reserve crates between the dry run and the submit, and the submit then succeeds
without the user having seen the reservation warning (the trip's gate pass later fails with `INSUFFICIENT_STOCK`, which is visible). Options: a separate acknowledgement
flag (e.g. `ack_reserved`) or a hard block for write-offs of reserved crates. Not changed – needs a contract decision.

## Known gaps
* No inline reorder evaluation on batch/blow consumption beyond a best-effort spawned task after commit (the worker is authoritative).
* Supplier returns do not reduce PO `qty_received` (a replacement delivery counts against the 110 % limit).
* Pallet convenience fields are present on batch/stock responses; there is no pallet input conversion server-side (UI concern).
