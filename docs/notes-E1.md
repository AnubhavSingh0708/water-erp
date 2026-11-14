# Notes E1 - loading, jars, maintenance, qr-labels pages

Files: `static/{loading,jars,maintenance,qr-labels}.html`, `static/css/pages/{loading,jars,maintenance,qr-labels}.css`,
`static/js/pages/loading.js` (router/list/new trip) + `loading-trips.js` (trip workspace, load sheet, gate pass, void, cancel) + `loading-settle.js` +
`loading-gate.js` (printable gate pass + shared print frame) + `loading-dispatch.js` (direct sales) + `loading-scan.js` (QR mode) +
`loading-common.js` (helpers shared by all four pages: review dialog, dry-run flow, guided errors, busy/"Not sent - Retry"), `jars.js` + `jars-actions.js`, `maintenance.js`, `qr-labels.js`.

## Behaviour summary
* **loading**: hash router (`#trip=<id>`, `#settle=<id>`, `#gp=<id>`, `#dn=<id>`, `#dn-new`, `#dispatch`; `?trip=<id>` from the dashboard and `?qr=<code>` from a scanned sticker URL are mapped). One trip per vehicle, availability per the trip's own godown (`trip_id` + `location_id`), load via a bottom sheet (live free count, vehicle room, pallets+crates echo, over-free / over-capacity blocked with the reason, QUARANTINE batches need an extra confirm).
  Gate pass = dry run -> review dialog (items, vehicle papers, stock effects) -> commit; printable A4 or 80 mm thermal slip (named `@page thermal`). Settle = live per-item arithmetic, container-customer lines (totals must match the delivered count), dry run -> CONFIRM_REQUIRED (damage %) -> review -> commit. Void gate pass = reason + the server's CONFIRM dialog. Direct sales (dispatch notes): customer or walk-in, lines by product (oldest first) or batch, FEFO preview = dry-run effects, void with reason.
* QR mode appears only when `qr_labels_enabled`: scan (BarcodeDetector, else vendored jsQR loaded as a classic script), torch / camera switch when the device offers them, manual code entry, partial-load split with a "print the new label" dialog (label printed alone via `body.printing-label`).
* **jars**: overview (fleet bar, health, period selector, destroy reasons as CSS bars, per godown), customers (search/overdue/zero filters, ledger drawer, opening balance), actions (receive new, wash, receive empties, counter issue, destroy - each form -> dry run -> review -> idempotent commit; destroy = server CONFIRM + review), destroyed log with reason filter.
* **maintenance**: schedules grouped OVERDUE / DUE_SOON / OK, equipment/vehicle filter, "Mark done" (date, notes, next due auto-filled from the interval, confirm dialog, draft kept), history drawer, vehicle documents board (colour + icon + text).
* **qr-labels**: friendly "switched off" state (no `/api/qr` calls) when the feature is off; batch picker, generate (same size / number of sets / custom sizes, live sum vs unlabelled crates), set list, print preview (A4 sheet 3-up; thermal one label per page), delete unused label.
* Every write uses one `api.action()` per dialog/form (double tap, auto-retry and "Not sent - Retry" re-send the same Idempotency-Key); long forms (new trip, settle, direct sale, QR generate, maintenance complete, jar actions) are drafted.

## Requests for shared files (Lead -> foundation owner)
1. `ui.reviewSheet` only takes a summary string and raw numbers; E1 pages use their own `reviewDialog` (in `loading-common.js`) to show crates/containers, extra nodes (item list, vehicle papers) and notes. Consider extending `reviewSheet` with `before`/`after` nodes, `unit` and singular/plural.
2. `ui.qtyInput`: `max` cannot be updated after creation (E1 validates availability itself on a mutable map) and the echo says "1 containers" for a single container. A `setMax()` and a `unit` singular would help.
3. Toast close / action buttons are 32 px high (`.toast button`), below the 44 px touch target.
4. `ui.printHeader` prints "Plant NSG" when the plant name equals the default "Neersagar"; fine, but showing the plant name next to the wordmark would read better.
5. QR page needs a batch list that works for users with only `qr-labels` (they cannot call `/api/production/batches`): a small `GET /api/qr/batches` (id, batch_no, product, crates, qc_status, expiry, labelled crates) would let the picker work for everyone. E1 falls back to `/api/loading/availability` (approved stock only), then to batches that already have sets.
6. Vehicle documents for the loading page: `/api/maintenance/vehicle-docs` needs the maintenance page, so the loading page reads expiry dates from `/api/ref/vehicles` and computes status client-side (works, no request).
7. `nav.js` marks `qr-labels` as `needsQr`; `boot({guard:true})` would redirect before the "switched off" state can render, so the page boots with `guard:false` and checks the page permission itself.
8. A first-run `GET /api/loading/trips?status=OPEN` (LOADING + DISPATCHED) would save one request on the list screen.

## Known gaps
* Product-level "oldest first across batches" loading (`{product_id, crates}`) is supported by the API but the UI loads per batch (list is already FEFO ordered, first row per product is marked "Oldest first"); direct sales do use automatic oldest-first.
* Camera scanning was verified with Chromium's fake camera (jsQR fallback path starts, frames run, stream stops on close); decoding a real sticker and the torch constraint need a phone.
* Ledger drawer in jars shows 30 rows at a time ("Show older"); no CSV export on these pages.
* `window.print()` output was verified through `page.pdf()` (A4) and print media emulation, not on physical thermal printers.
