# Notes W3 - per-customer deliveries on settlement (X) and counter returns (Y)

Contract: `API_REFERENCE.md` section 0.2-I (I.1 deliveries, I.2 customer returns). Migrations `0030_trip_customer_deliveries.sql`, `0031_customer_returns.sql`.
Code: `src/routes/loading/{deliveries,customer_returns}.rs`, `settle.rs` (customer_deliveries), `common.rs` (`deliveries_block`, used by `trip_json` for SETTLED trips).
Tests: `tests/loading_deliveries.rs` (4), `tests/customer_returns.rs` (13); route table / authz matrix / foreign-godown lists extended in `pentest_authz.rs`, `pentest_godown_ids.rs`.

## Decisions / deviations (for the Lead and the frontend agent)
* **Document number `CR-yymmdd-0001` does not go through `util::next_no`**: its whitelist (`src/util.rs`, not editable here) has no `cr_seq` and a `debug_assert!` would panic. `customer_returns.rs::next_cr_no` runs `nextval('cr_seq')` itself with the same format. If you prefer, add `"cr_seq"` to the whitelist later and switch.
* **New error code `NOT_RESTOCKABLE` (409)** for intact crates of a non-APPROVED / expired batch (message: "Book them as damaged instead"; hint in `error.rs`; `details.reason` = `NOT_APPROVED | EXPIRED`). `QC_LOCKED`'s hint ("cannot be loaded or dispatched") would have misled the operator. Expired = `expiry_date < plant_today`.
* **Large-quantity rule of a return** (per batch, intact + damaged added up so lines cannot be split to dodge it): `>= 100` crates, or `total*100 > entry.large_adjust_pct * FG_READY balance of that godown` **only when that balance is > 0**. With the literal spec reading a sold-out batch (balance 0, the normal case for a return) would need a confirmation for every single crate; there only the 100-crate rule applies. Damaged crates count towards both.
* Hard errors (validation, `NOT_RESTOCKABLE`, `OVER_PRODUCED`, and for void `RESERVED_STOCK` / `INSUFFICIENT_STOCK`) are reported **before** `CONFIRM_REQUIRED`, so the operator never confirms something that then fails. Dry runs return `CONFIRM_REQUIRED` unchanged (loading convention): send `confirm:true` to preview a large return.
* `OVER_PRODUCED` guard runs on the per-batch **intact** total (several lines on one batch are added up); damaged crates are not capped (they never enter finished goods).
* Void window anchor = document `created_at`; Admin unlimited; void always needs `confirm:true`. The void checks `lot_avail` (FG on hand and free after reservations) with the batch row locked, so a concurrent trip add-item and a void on the same crates have exactly one winner (`tests/customer_returns.rs::void_racing_a_trip...`).
* `customer_deliveries`: duplicates `(customer, product)` are **rejected** (422), not merged; crates must be >= 1; the customer rows (jar + delivery customers) are locked in ONE sorted `FOR UPDATE` statement (last in the lock order) and `is_active` is re-read from that locked statement. Trip detail adds `customer_deliveries[]` and `unassigned_crates[]` only for SETTLED trips (absent otherwise); `unassigned_crates` has one row per non-returnable product `{product_id, product_name, delivered, assigned, unassigned}`.
* No prices, tax or payment anywhere. Realtime: both new tables notify topic `trips`.

## Frontend to-do (not done here)
* Settlement screen: optional per-customer rows for bottled products; show `unassigned_crates`; review sheet from `?dry_run=1` (`TRIP_DELIVERY` effects).
* Deliveries list + summary screen (`/api/loading/deliveries`, `/deliveries/summary`); customer-returns create (with dry-run review and confirm), list, printable detail (`printHeader` keys match `ui.printHeader`), void.
* Add the ledger `txn_type`s `CUSTOMER_RETURN`, `CUSTOMER_RETURN_DAMAGED`, `CUSTOMER_RETURN_VOID` to the ledger filter list in `static/js/pages/warehouse-lib.js`; handle error code `NOT_RESTOCKABLE`.
