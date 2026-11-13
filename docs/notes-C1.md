# Notes C1 – Loading, dispatch, gate pass, settlement, optional QR sets

Files: `src/routes/loading.rs` + `src/routes/loading/{common,trips,gate,settle,dispatch}.rs`, `src/routes/qr.rs`,
`tests/loading_*.rs`, `tests/qr_sets.rs`, `tests/loading_support/mod.rs`. No migration, no new crates.

## Findings for the Lead / other owners (please read)

1. **`stock::allocate_free_fefo` has a stale-snapshot race.** It reads `reserved` and takes `FOR UPDATE OF b` in ONE statement.
   Under READ COMMITTED a caller that waits on a batch row that was merely *locked* (an adder inserts `trip_items`, it does not
   update the batch row) resumes with its original snapshot and misses the reservation that just committed.
   *Workaround used here:* `loading::common::prelock_product_batches` locks the candidate rows in a separate prior statement, and every
   allocation is re-verified after `lock_batches` + `lot_avail` (separate statements = fresh snapshots). C2's jar-counter `issue` and any
   write-off/scrap code that uses the helper should do the same (or the helper should be split into "lock" + "compute").
   `tests/loading_concurrency.rs::waiting_adder_*` reproduce the situation deterministically.
2. **Lock order** used by everything in this module: trip row -> batch rows (`expiry_date, created_at, id`, i.e. the allocator's order) ->
   `qr_crate_sets` (batch `FOR SHARE` first) -> stock balance rows -> `customers` rows (settlement). QC approve/reject (B) must lock the
   batch row `FOR UPDATE` *before* touching its sets/stock (spec already says so).
3. **jar_ledger writers (C2)** should `SELECT … FROM customers WHERE id=$1 FOR UPDATE` before checking a customer's holding, as settlement
   does, otherwise "no negative holding" can race between `receive-empties` and a settling trip.
4. **Non-strict QC**: a QUARANTINE batch loaded with `loading.strict_qc_lock=false` is deducted from `QC_QUARANTINE` at gate pass.
   If QC approves in between, the batch row lock makes the gate pass read the new status and deduct from `FG_READY`. QC approve must move
   only the *remaining* balance (spec) – already the contract.

## Deviations / decisions (all within the spirit of API_REFERENCE §0.2, §9, §10)

* **Crate quantities in loading JSON are whole numbers** (`available`, `reserved`, `fg_ready`, `crates`, error `details.available/required`),
  not decimal strings. Stock-ledger `effects` in dry-run bodies use decimal strings (`delta`, `after`) like the rest of the system.
* `GET /availability` extras: `zone` (`FG_READY` or `QC_QUARANTINE`), `in_trip` (crates of that batch already in `?trip_id=`),
  `crates_per_pallet`, `available_pallets`, `available_loose_crates`. Non-strict mode lists QUARANTINE batches (stock in `QC_QUARANTINE`).
* `POST /trips/{id}/items` with `{product_id}` only ever allocates **APPROVED** stock, even in non-strict mode; quarantine stock must be
  added explicitly by `batch_id` (response carries a warning). Response: `{trip, item, items, residual_set, warnings?}` (201).
* **One open LOADING trip per vehicle** (vehicle row locked) -> `409 DUPLICATE`; a vehicle still on an unsettled DISPATCHED trip needs
  `confirm:true` on `POST /trips` (this prevents splitting a load over two trips to dodge the capacity limit). Trip create body: `{vehicle_id, route_id?, driver_name?, notes?, confirm?}`.
* `POST /trips/{id}/cancel` accepts an optional `{reason}` (audited). Items stay on the cancelled trip for history, reservations vanish.
* Trip/Item objects add `fill_pct`, `pallets`, `loose_crates`, `crates_per_pallet`, `units_per_crate`, `expiry_date`; trips add `void_reason`, `voided_at`.
  No `updated_at` column exists on `loading_trips`; clients use `status`.
* Gate pass: vehicle document columns that are NULL are treated as "not tracked" (not expired). Expired = `date < plant_today`.
  Gate-pass number (`GP-…`) and dispatch number (`DN-…`) are taken **after** all checks/stock moves succeeded and are not consumed by dry runs,
  so the series has no gaps from failed attempts. Ledger notes therefore say "gate pass for trip TR-…" (the ref is `TRIP/<id>`).
* Settlement: `damaged` is the item-level damage; the damage-% confirm test is `damaged*100 > settle.damage_warn_pct * loaded` (integers).
  Returned crates go to the lot's *current* zone (APPROVED -> `FG_READY`, QUARANTINE -> `QC_QUARANTINE`, REJECTED -> `SCRAP_YARD`).
  Rule violations in settlement are `422 VALIDATION` with field paths like `items[1].delivered`; "customer would hold a negative number
  of jars" is **`400 BAD_REQUEST`** (the contract text says "409 BAD_REQUEST" but its error table maps BAD_REQUEST to 400).
  Customer jar lines: duplicate (customer, product) pairs are rejected, products must be RETURNABLE products on the trip, customer
  lines must sum to the items' delivered per product. Fleet events: item `damaged` of a RETURNABLE product -> `DESTROYED/DAMAGED_RETURN/VEHICLE`;
  line `damaged` -> `DESTROYED/DAMAGED_RETURN/CUSTOMER`; line `lost` -> `DESTROYED/LOST/CUSTOMER`. Stock: `empties_collected` -> `JAR_EMPTY`
  (`JAR_RECEIVE`, lot `stock::NIL_LOT`), line damaged empties -> `SCRAP_YARD` (`TRIP_DAMAGED`, `NIL_LOT`); damaged *filled* jars -> `SCRAP_YARD` in the batch lot.
* **Dispatch notes reject RETURNABLE products** (422): customer-deposit tracking must go through Jars `issue`, otherwise `jar_ledger` would drift.
  A `confirm` field is accepted (no dispatch case needs it). Void: `{reason (>=5 chars), confirm?}`, window `loading.void_window_hours`, Admin unlimited (403 otherwise).
* Gate-pass void (`POST /trips/{id}/void {reason, confirm:true}`): always `CONFIRM_REQUIRED` first; restock zone follows the batch's *current* QC state.
* `?dry_run=1` (also `true` / bare `?dry_run`) on items, gate-pass, settle, void, dispatch create/void. `CONFIRM_REQUIRED` and all other errors
  are returned unchanged in dry-run mode (as the contract says), so a damage-confirm dry run needs `confirm:true` in the body.
* QR: `/api/qr` create/delete need the `qr-labels` page (the route guard also admits `loading` for read/scan). UNIFORM labels the *whole remaining* unlabelled
  quantity (last set smaller; default size = `crates_per_pallet`, else `loading.default_crate_set_size`), COUNT splits near-equal, CUSTOM takes exact sizes.
  Partition invariant: Σ `original_crates` of root sets ≤ `batch.crates`, batch row locked. Residual label = `<root label>-R<k>` (k counts residuals of that root in the batch),
  `parent_set_id` = the set it was split from. The `{qr}` mode of `/items` re-checks `features.qr_labels` itself (it is not behind the router gate). SVG: pure-Rust, XML prolog stripped.
  `GET /sets/lookup/{code}` is case-insensitive. QR URL base comes from `AppState::base_url` (Host header when no override).
* Error envelope: own extractors (`J`, `Q`, `Ids`) so malformed JSON/query/path ids return the standard envelope (400) instead of axum's plain text.

## Known gaps / not done
* No per-user ownership of trips (shared floor) – any user with the `loading` page can act on any trip.
* Settlement does not compare `empties_collected` with anything physical (nothing to compare with); only holdings can't go negative.
* `GET` lists are not exposed as CSV. Gate-pass/dispatch/QR sheet are JSON only (UI prints them).
* Jar-ledger `txn_date` is `plant_today`; no `updated_at` on trips.
