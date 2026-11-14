# Notes - Owner E2 (raw-materials, production, qc, warehouse, godowns pages)

Files: `static/{raw-materials,production,qc,warehouse,godowns}.html`, `static/js/pages/{raw-materials,raw-materials-po,raw-materials-grn,production,production-blow,production-batches,qc,warehouse,warehouse-actions,godowns,warehouse-lib}.js`,
`static/css/pages/{warehouse(shared),raw-materials,production,qc,godowns}.css`. `warehouse-lib.js` / `warehouse.css` are shared by all five pages (kept under the owned name patterns).

## Shared pieces (warehouse-lib.js)
`submitDocument` = dry run -> review sheet -> real POST with the same Idempotency-Key; `confirm:true` is sent on the real call (dry run already showed the warnings; `sendConfirm:'always'` for cancel).
After a network failure the next press skips the review and re-sends the same key ("Not sent - Retry"). `stateDraft` = drafts for forms with dynamic rows. `stockPicker` = zone -> item -> lot (free = qty - reserved).
`printNode/showDocument` print into `#print-root` (body.printing hides everything else). `explain()` turns INSUFFICIENT_STOCK / RESERVED_STOCK / QC_LOCKED / DUPLICATE into plain-language help.

## Verified (Playwright, 390x844 + 1280x800, seeded server)
All five pages and all tabs: no horizontal overflow, 0 console errors / CSP violations / failed requests, targets >= 44 px, inputs >= 16 px, no inline handlers.
Flows: PO -> GRN -> stock; water log (out-of-range confirm) -> blow run (scrap note rule) -> batch (BOM check, waste, returnable jars) -> trace -> QC test -> approve / reject; stale QC (second session approves: banner, buttons hidden);
adjustment, write-off + voucher, count (large-variance note, post), godown transfer create -> receive with damage + shortage, cancel; reorder -> PO -> cancel; GRN void; supplier return.
Sad paths: duplicate challan (inline + field error), RESERVED_STOCK (trip numbers shown), insufficient stock (client BOM block / server explain), 403 redirects (operator -> warehouse, qc -> raw-materials), restricted `store_north` (no switcher, only its godown),
server killed mid-submit -> "Not sent - Retry" after ~6 s, draft kept, retry after restart created exactly one ledger document; PO draft restored after reload.

## Requests for the Lead (shared files / backend)
1. `GET /api/ref/locations` returns only the caller's own godowns, so a restricted user has no destination for a godown transfer (page shows an explanatory message). Suggest `?all=1` (id + name only) for destinations.
2. styles.css: toast dismiss button is 32 px high (< 44 px); excluded from the touch-target check.
3. Optional: a `lot_id`-less way to list approved FG lots with zero balance so "stock found" adjustments can add to an empty lot (currently only lots with stock > 0 are selectable).

## Known gaps
- Preform stock is not pre-checked in the blow form (operators lack the raw-materials page); the server error is explained instead.
- Review sheet "will be N" shows the lot balance (shared component), not the item total.
- Desktop dates in the batch table wrap slightly; no keyboard shortcuts for the count sheet.
