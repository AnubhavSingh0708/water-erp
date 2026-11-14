# Notes from Subagent D (frontend foundation)

Scope delivered: `static/` design system (`css/styles.css`, `img/icons.svg`), core modules (`api, ui, drafts, connectivity, events, nav, shell, crud, pwa, pwcheck`),
PWA (`manifest.json`, `sw.js`, `offline.html`, PNG icons from `img/icon.svg`), pages `index, login, setup, profile, dashboard, admin (Users, Sessions, Audit, Settings, Master data + BOM editor, Godowns), styleguide`.
Usage for other page authors: `docs/FRONTEND_GUIDE.md`.

## Deviations / decisions
* **Brand**: Neersagar (titles `Neersagar ERP – <Page>`, logomark `icons.svg#neersagar`, wordmark). `ui.printHeader()` added for printable documents.
* **Login 401**: `api.js` never redirects on the login call (`noAuthRedirect`), and treats `429 LOCKED` as non-retryable. `409 IN_PROGRESS` IS retried (same key) because a killed server leaves the key pending for 35 s.
* **Idempotency**: keys also on PUT/DELETE; `api.action()` regenerates the key only when the body changes (ignoring `confirm`) or after success. Dry runs carry no key (server ignores it).
* **Dashboard** follows the REAL `/api/dashboard` (not only section 4): `kpis.*` may be `null` (page not permitted -> tile hidden), plus `by_location`, `in_transit_warnings`, `kpis.in_transit_transfers`. 404/unavailable -> "not ready" empty state.
* **Admin users**: `allowed_pages` checklist excludes `admin` (backend rejects it; admin = `is_admin` toggle). Godown restriction = `allowed_location_ids` checklist (none ticked = all). Master-data PUTs send only changed fields; nullable cleared fields send `null`, hidden dependent fields (e.g. preform on a RETURNABLE product) are nulled.
* **Settings** typed from a local metadata table (labels/units/ranges mirror `admin_settings.rs` SCHEMA); unknown keys fall back to typeof. Only changed keys are sent; field errors keyed by dotted setting names map to inputs.
* **Live**: `ERP.live.start()` called again with the same page just swaps handlers (boot already opened the stream for the bell / permissions / settings). `'*'` handlers are not run for `alerts`/`users` topics. `server_restarting` (in events.rs, missing from the §15 table) shows the "Server restarting..." banner.
* Page guard: `qr-labels` also requires `features.qr_labels` (`/api/ref/settings.qr_labels_enabled`); `godowns.html` is guarded by the `warehouse` permission and appears as a child of Warehouse.
* Cookie is `Secure`: test at `http://localhost:<port>`, never `127.0.0.1`. If the browser refuses the cookie the login page falls back to setting it from the login response.

## Verification (Playwright, chromium, 390x844 + 360x740 + 1280x800)
* Pages walked: login, dashboard, profile, admin (all 6 tabs, dialogs: customer/product/BOM/user/vehicle/audit details, inbox, godown chooser, More drawer): no horizontal overflow, 0 console/CSP/failed-request errors (outside deliberate outage tests), all visible controls >= 44x44 (a 43 px reading on desktop dialogs was the open animation), inputs 16 px, 0 inline handlers/scripts.
* Restricted users: `loader` (dashboard, warehouse, godown transfers, loading), `qc` (dashboard, qc), `store_north` (dashboard, raw materials, warehouse, transfers; godown switcher hidden - single godown), `gate`: nav shows only allowed pages; `/admin.html` redirects to the first allowed page with a toast.
* Live permission change: admin added `jars` to `loader` -> loader's open page gained the Jars link + toast without reload; removing `dashboard` moved the open page to the first allowed page.
* Outage: server killed with a form open -> banner "Server unreachable - nothing is lost, your entries are kept", submit shows "Not sent - Retry", fields intact; after restart the banner clears and Retry succeeds with the SAME `Idempotency-Key` (5 attempts, 1 distinct key, 1 record). Response-lost test (`route.fetch()` then abort): client retried with the same key, server replayed, exactly 1 record. Reload while offline: page + menu come from the service-worker cache / cached `/api/me`, the draft is restored when the form is reopened.
* Service worker: active; caches `erp-shell-v1` / `erp-rt-v1` contain 0 `/api/` entries (fetch handler returns early for `/api/*` and non-GET).
* Contrast (computed): all text pairs >= 4.5:1, control borders/switch tracks >= 3:1.

## Known gaps
* Pages owned by E1/E2 do not exist yet in this tree; links to them 404 until they land (a user whose first allowed page is such a page is redirected there).
* Web Push enable flow is implemented per the contract but was not exercised end-to-end (no VAPID keys in the dev server); status/instructions branches render.
* Table->card keeps long unbreakable batch numbers on two lines when a status badge sits beside them (use `b.nowrap` for short identifiers).
* No dark theme (light-first by design for sunlit docks).
