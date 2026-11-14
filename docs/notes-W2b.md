# Notes - W2b (raw-materials, production, qc, warehouse, godowns: polish + features)

Files touched: `static/js/pages/{warehouse-lib,warehouse,warehouse-actions,godowns,production,production-blow,production-batches,qc}.js`,
`static/css/pages/{warehouse,production,godowns,qc}.css`. No shared file (styles.css, api.js, ui.js, shell.js, nav.js, events.js, crud.js, sw.js) was edited.

## Features
* **Preform stock pre-check** (production > Blow molding): `GET /api/production/preform-stock?preform_material_id=&location_id=` is called (production site, not the switcher) when the product / preform
  changes, after draft restore, after a save, on live `stock`/`materials` events and once more right before the dry run. Shows "Available: N pcs" (green), a warning when the run leaves the stock at or
  below `min_stock`, an error + client-side block when `issued > available` (plain sentence: how many are where, what to do). A 404/405 (older server) is memoised for the page load and the chip stays hidden;
  other errors show a muted "could not check - the server will still check it". Note: that single 404 shows up once as a browser console line on a server without the endpoint (browsers log every 4xx fetch).
* **Stock found** (warehouse > Actions > Adjust stock > Add): the lot list now also offers lots that hold nothing in the chosen zone ("... - 0 here (12 in Raw store)"): lots of the same item in another zone or
  godown (`/stock/item?location_id=all`, only lots the server already knows - it refuses invented RAW lot ids) and, for finished goods, the APPROVED batches of the product (`/api/production/batches`, only when the
  user has the production page; skipped silently otherwise). Returnable jars in JAR_EMPTY/JAR_CLEAN get "Containers (no lot)". When nothing is eligible the form explains why (receive via GRN / production + QC approval).
  "Remove" still lists only lots that hold stock. The picker has `setAdding()`.
* **Review sheet for adjustments** shows both rows: `Lot NSG-B-...: 20 -> 28` and `All lots in <zone> / <godown>: 90 -> 98` (built in `warehouse-actions.js` `adjustEffects`, passed to `submitDocument({decorate})`;
  the shared `ui.reviewSheet` is untouched - it only appends "will be N" to rows, so before/after is written into the row text).
* **Transfer destinations** use `GET /api/ref/locations?all=1` (`L.loadAllLocations()`, cached 5 min, falls back to the user's own godowns). A restricted user (`store_north`) sees every other active godown; godown names
  of both ends (review text, in-transit list) resolve through the same list. The "only one godown" banner now shows only when no other godown exists at all.
* **Back button**: detail / form views (transfer detail, receive, new transfer, QC batch, warehouse action forms and count sheet, new batch) push one history entry, so Android Back returns to the list instead of leaving
  the page (`L.enterSub / clearSub / goBack`, `L.backBar` uses `goBack`). Tabs keep `history.state` when they rewrite the hash.

## Visual QA fixes (390x844 @2x and 1280x800, every tab/dialog/state)
Tabs fit on 360 px (icons hidden < 480 px, 48 px targets); batch / run / transfer numbers never break at their hyphens (`b.nowrap`, ellipsis safety net); cards put the status chip under a long identifier on <= 400 px;
card-in-card lists flattened (`.inset` / `.flat`); By-godown card no longer orphans a value; "Transfers" link no longer wraps; Stock list on phones is one tappable card per item (button hidden, "Tap an item" hint);
Ledger filters = "Filters (n)" collapsible on phones, inline row on desktop, 2 per row; compact empty states for secondary lists; Actions list headings spaced; QC wait chip shortened ("Waiting 15 h");
QC test form 2-up numeric fields, "Reject" label no longer wraps; form pages (new transfer / receive) get proper spacing and a dashed "no goods yet" box; missing item/lot/destination is flagged on the control
(not only in a banner) and all errors are shown at once; placeholders "e.g. 8000" instead of a lookalike "0"; water-log banner button aligned; single-button sticky bars full width on phones.

## Verification (Playwright, localhost:18454, seeded demo DB)
* 62 tab/dialog/form states at 390x844, 1280x800 plus all tabs at 360x640 and 640x400 (= 200 % zoom): no horizontal overflow, 0 console / CSP errors (except the memoised preform-stock 404 above),
  all targets >= 44 px, inputs >= 16 px, no inline handlers, no `innerHTML` with data.
* Preform check with mocked endpoint: plenty / low / short (field error + block) / none / 500 / 404.
* Server killed for real mid-submit (blow run): review shown, server `pkill`ed, "Not sent - Retry" after ~10 s, draft and field values kept, server restarted, retry = exactly **1** row (`blow_molding_runs`).
  Double tap on the submit and on the review confirm = 1 row.
* store_north: only its godown in "Send from", all other godowns offered as destination, review text names them. Operator on /warehouse.html and /godowns.html, qc on /production.html -> redirected to the dashboard with a toast.
* Stale screens: transfer received by a second session while the detail is open -> toast "now received", action bar removed. (QC decision / count stale covered by E2.)
* Back / forward on godowns, warehouse actions, QC; Escape closes review sheets without losing the form; focus is inside the dialog when it opens.
* Long names and identifiers (injected via route rewriting) at 360 px: no overflow. Throttled network (1.5 s latency): skeleton cards, no blank screen.

## Requests for the Lead / shared files
1. `ui.reviewSheet`: a first-class "before -> after" per effect row (and an optional second "total" row) would replace the text workaround in `adjustEffects`.
2. A backend `GET /api/warehouse/stock/item?include_zero=1` (or a lots-of-item endpoint) would let "stock found" list every historic lot, including ones used up, for storekeepers who have no production / raw-materials
   access to the lot lists; today the list is "lots the item holds somewhere" + approved batches for users with the production page.
3. styles.css could offer `td.k b.nowrap` ellipsis and the `.compact` empty state globally; they currently live in `css/pages/warehouse.css`.
4. Native date inputs render `mm/dd/yyyy` in Chromium with an en-US UI language; nothing to do in the page.

## Left
* Mixed `lot_id` for RAW lots that exist only in a GRN and never reached stock (fully consumed) is still not selectable (needs request 2).
* Desktop ledger table is dense with 7 columns at 1024 px; works, could drop "Balance after" below 1280 px.

## Addendum (after merging main with W1's backend)
* Server-side contract of `/api/production/preform-stock` and `OVER_PRODUCED` re-read in `src/`; the page matches it. The prebuilt snapshot binary used for testing predates both (no Rust build possible),
  so the pre-check was verified against a mocked endpoint (plenty/low/short/none/500/404) and the 404 path against the real binary; `OVER_PRODUCED` has a plain-language explanation in `warehouse-lib.js` `explain()` (also shown for count posts).
* Double tap: every submit handler is wrapped in `L.once` and `submitDocument` ignores re-entry (found while testing: with the endpoint present, two taps stacked two review sheets). 1 row after dblclick.
* Stale stock: INSUFFICIENT_STOCK / RESERVED_STOCK / INVALID_STATE on adjust/move/write-off re-load the lot list (`picker.refreshLots()`), so "On hand" shows the real balance next to the plain-language explanation.
* Bugs fixed on the way: a literal "null" appended to the blow scrap echo and to the count difference chip (DOM `append(null)`); transit rows in review sheets now say "On the road / Raw store" and name godowns the user does not own.
* Raw materials: filters in 2 columns, shorter tab labels ("Orders", "Receive"), Back button for GRN / PO forms.
* RAW "0 here" candidates (lot held in another zone, lot held in another godown) were dry-run against the server: accepted. FG approved-batch candidate dry-run accepted.
* Not done / for W2a: no shared code closes dialogs on `popstate` (checked: none), so opening a voucher right after `home()` is safe today; if one is added, revisit `clearSub()`.
  `ui.errorState` joins message and hint without punctuation ("...on the server Try again in a moment.").
* Demo-data side effects of testing: TF-261114-0003 received (a new TF-261114-0004 Main -> City is in transit), blow runs with 1234/1235/1236 preforms, 2-3 cancelled counts,
  a 4,730 Cap write-off to the scrap yard that was "restored" with an adjustment (so Cap now also shows 4,730 in the scrap yard), 100 Cap in transit.
