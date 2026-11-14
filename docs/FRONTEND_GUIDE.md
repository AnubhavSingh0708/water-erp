# Frontend Guide - Neersagar ERP (static/)

Pure HTML5 + modern CSS + vanilla ES modules. **No build step, no frameworks, no CDNs/fonts/external requests.**
CSP: `script-src 'self'` - no inline `<script>`, no `onclick=`, no `eval`. Every dynamic string goes through `el()` / `textContent` / `esc()`;
never `innerHTML` with data. The only HTML-from-string allowed is the server's QR SVG (`/api/qr/...`).

Brand: **Neersagar** (droplet + wave logomark `icons.svg#neersagar`, wordmark "Neersagar", titles `Neersagar ERP – <Page>`). Never hard-code another company/plant name -
plant name comes from `ERP.ui.config.plantName` (`/api/ref/settings` `plant_name`).

Open `/styleguide.html` for every component rendered live (no login needed).

## 1. Files

```
static/
  css/styles.css          design system (tokens on :root, components, print)
  img/icons.svg           sprite, use: icon('truck')  ->  <svg class="i"><use href="/img/icons.svg#truck"/></svg>
  img/icon*.png icon.svg  PWA icons (generated from icon.svg)
  js/api.js               fetch layer            js/ui.js         DOM/dialog/format helpers
  js/drafts.js            form autosave          js/connectivity.js  health ping + banner
  js/events.js            SSE live updates       js/nav.js        nav registry, guard, godown switcher
  js/shell.js             boot() + ERP object    js/crud.js       schema-driven CRUD   js/pwa.js  SW/push/install
  js/pwcheck.js           password policy hints  js/pages/*.js    one module per page (yours: js/pages/<page>.js)
  *.html                  one file per page (tiny - see template)
  sw.js manifest.json offline.html
```
Always import with **absolute paths** (`/js/ui.js`). Page files you own: `raw-materials production qc warehouse godowns qr-labels loading jars maintenance`.

## 2. Page template (copy this)

`static/warehouse.html`
```html
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<meta name="theme-color" content="#064a55">
<title>Neersagar ERP – Warehouse</title>
<link rel="manifest" href="/manifest.json">
<link rel="icon" href="/img/icon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="/img/icon-192.png">
<link rel="stylesheet" href="/css/styles.css">
</head>
<body>
<main id="main" class="page"><noscript><p class="banner err">JavaScript is required to use this app.</p></noscript></main>
<script type="module" src="/js/pages/warehouse.js"></script>
</body>
</html>
```
`boot()` builds the shell (app bar, sidebar >=1024px, bottom nav, More drawer, banners, toasts) around `#main`.

`static/js/pages/warehouse.js`
```js
import { boot, ERP } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
const { el, icon } = ui;

const { me } = await boot({ page: 'warehouse', title: 'Warehouse', godownAll: false });   // godownAll:true => switcher offers "All godowns"
const main = document.getElementById('main');

async function load() {
  try {
    const rows = await api.get('/api/warehouse/stock', { ...ERP.godown.param(), zone: 'FG_READY' });   // location_id added explicitly
    ui.mount(main, ui.cardsTable({ columns: [...], rows, emptyState: { icon: 'box', title: 'No stock here' } }));
  } catch (e) { ui.mount(main, ui.errorState(e, load)); }
}
ERP.godown.onChange(load);                                  // godown switched in the app bar
await load();
ERP.live.start('warehouse', { stock: load, production: load, qc: load, master: load });   // topics per API_REFERENCE section 15
```
`boot` options: `page` (nav key + SSE page key), `title`, `godownAll`, `guard` (default true: redirects to the first allowed page with a toast),
`live` (`false` to skip the default stream, or an object of handlers), `livePage` (SSE `?page=` if different from `page`).
`boot` resolves after: cookie check, `/api/me`, `/api/ref/settings`, godown list, nav, banner, bell, SW registration. It redirects to `login.html?next=` (and never resolves) when there is no/expired key.
`ERP` = `{api, ui, drafts, conn, live, godown, nav, config, me}` (also `window.ERP`).

## 3. api.js

```js
import { api } from '/js/api.js';
const rows = await api.get('/api/warehouse/stock', { zone: 'FG_READY', q: 'cap' });        // query object; ''/null skipped
await api.post('/api/raw-materials/purchase-orders', body);
await api.put(url, body);  await api.del(url);                                              // DELETE answers 204 -> null
```
* Sends `X-Auth-Key` from the `erp_key` cookie. Never put the key in a URL.
* **Every write gets an `Idempotency-Key` (uuid) generated once per call and re-used by the automatic retries.**
* Auto-retry 3x (back-off 0.6 s / 1.6 s / 3.6 s + jitter) on network error, 502/503/504, 429 (not login, not `LOCKED`) and 409 `IN_PROGRESS`. Never on other 4xx. 30 s abort.
* Errors are **normalised objects** (thrown): `{status, code, message, hint, fields, details, requestId, network, requestKey}`.
  Network failures: `network:true, code:'NETWORK'|'TIMEOUT'`. A 401 saves nothing special (drafts autosave continuously), redirects to `login.html?next=...` and throws `{redirecting:true}` - **ignore errors with `.redirecting`** (`ui.toastError`/`formError` already do).
* Options (3rd arg of get / 2nd of post): `{key, retries, timeout, noAuthRedirect, noRetry429, raw}`; `raw:true` returns `{data,status,headers,requestId,replayed}`.

### One user action = one key: `api.action()`
Use it for every submit button so a double tap, a retry after a timeout or "Not sent - Retry" re-sends the **same** key (server replays the first 2xx instead of creating a duplicate).
The key is replaced automatically when the body changes (ignoring `confirm`) and after a success.
```js
const act = api.action();                       // create once per form/dialog, outside the submit handler
form.addEventListener('submit', async (ev) => {
  ev.preventDefault(); ui.clearFormError(form);
  await api.busy(submitBtn, async () => {       // disables + spinner, always restores
    try {
      const grn = await act.post('/api/raw-materials/grns', body);   // or act.put / act.del / act.postConfirm
      draft.clear(); ui.toast.ok(`Saved ${grn.grn_no}`);
    } catch (e) {
      if (e.network) submitBtn.lastChild.textContent = 'Not sent - Retry';   // optional label swap
      ui.formError(form, e);                    // inline fields + message + hint + "Ref: <requestId>" on 5xx
    }
  });
});
```
### Dry run -> review sheet -> real submit (GRN, batch, adjustment, transfer, write-off, trip item, gate pass, settle, dispatch, count post)
```js
const dry = await api.dryRun('/api/production/batches', body);            // POST ...?dry_run=1  -> {dry_run:true, effects:[{item,zone,delta,after}], warnings:[]}
const ok = await ui.reviewSheet({ title: 'Review batch', effects: dry.effects, warnings: dry.warnings, confirmText: 'Create batch' });
if (ok) { const batch = await act.postConfirm('/api/production/batches', body); }
```
`dryRun` also surfaces the same errors (e.g. `INSUFFICIENT_STOCK` with `details.shortages`) before the user commits.
### CONFIRM_REQUIRED
`act.postConfirm(url, body)` / `api.withConfirm(send, body)`: on 409 `CONFIRM_REQUIRED` a dialog lists `details.warnings`; "Yes, continue" re-sends **the same body with `confirm:true`** (same key). Declining rejects with `{cancelled:true}` (ignored by `toastError`/`formError`).
### Other
```js
await api.download('/api/warehouse/stock', { query: { format: 'csv', zone: 'FG_READY' } });   // authenticated fetch -> Blob -> save (CSV)
api.newKey();  api.busy(btn, promiseOrFn);
```
Soft warnings in 2xx bodies (`"warnings":[...]`) -> `ui.toast.warn(w)` or a `.banner.warn`.

## 4. ui.js cheat-sheet

| Need | Use |
|---|---|
| Build DOM | `el('div.card#id', {class:'x', dataset:{a:1}, on:{click:fn}, 'aria-label':'..'}, ...children)` - strings become text nodes. `icon('truck','sm')`. `ui.mount(node, ...kids)`, `ui.clear(node)`, `$`, `$$` |
| Toasts | `ui.toast.ok/err/warn(msg, {sub, ms, action:{label,onClick}})`, `ui.toastError(err)` |
| Dialogs (native `<dialog>`; bottom sheet on phones) | `ui.openDialog({title, body, actions:[btn], wide})` -> `{close(result), closed}`; `await ui.confirmDialog({title,message,warnings,danger,confirmText})` -> bool; `await ui.promptDialog({title,label,min})` -> string\|null; `await ui.reviewSheet(...)` |
| Form fields | `ui.field({label,name,type:'text'\|'select'\|'textarea'\|'int'\|'decimal'\|'date',value,help,required,unit,options})` -> `{el,input,get(),set(),setError(msg)}`; `ui.readForm(form)`; `ui.formError(form, err)`; `ui.clearFormError(form)`; `ui.formBanner(form,msg)` |
| Quantity entry | `ui.qtyInput({name,label,unit,max,decimals,cratesPerPallet,onInput})` - stepper, live "Check: you entered 130 crates = 2 pallets + 10 crates", "Enter as pallets + crates" mode, clamps on blur. `q.value` (number\|null), `q.valid`, `q.error`. Use `fmtCrates` everywhere crates are shown. |
| Numbers/dates | `fmtNum(v,dp)`, `fmtQty(v,unit)`, `fmtCrates(n, cratesPerPallet)` -> "2 pallets + 10 crates", `fmtDate`, `fmtDateTime`, `fmtTime`, `fmtRelative`, `fmtMinutes`, `todayPlant()`, `addDays(ymd,n)` (plant-local via `plant.utc_offset_minutes`). Decimal strings from the API are fine as input. |
| Status badges | `ui.badge('QUARANTINE')` (text + icon + colour). Known: APPROVED FG_READY OK RECEIVED SETTLED AVAILABLE PASS (green) / QUARANTINE LOW DUE_SOON PARTIAL IN_TRANSIT LOADING OPEN DRAFT RESERVED (amber) / REJECTED CRITICAL OVERDUE OUT FAIL VOID CANCELLED EXPIRED (red) / DISPATCHED INFO (teal) / other = grey. `ui.badge(s, 'custom label', 'ok'\|'warn'\|'bad'\|'info')` |
| Lists | `ui.cardsTable({columns, rows, onRow, emptyState})` (see 6), `ui.pager({total,limit,offset,onPage})`, `ui.empty({icon,title,text,action})`, `ui.errorState(err, retry)`, `ui.skeletonCards(n)`, `ui.spinner()`, `ui.loadInto(region, asyncFn)` |
| Tabs / segmented / collapsible / progress | `ui.tabs({tabs:[{id,label,icon}], active, onChange})` -> `{el, select}`, `ui.segmented({options,value,onChange})`, `ui.collapsible(title, content)`, `ui.progress(pct)` (amber >=90 %, red >=100 %) |
| Misc | `debounce`, `sleep`, `copyText`, `safeNext(path)` (same-origin relative only), `esc(str)`, `setTitle` |
| **Printing** | `ui.printHeader({title:'Gate pass', docNo:'GP-261113-0001', subtitle})` -> logomark + "Neersagar" + plant name + doc number + date. Visible **only in print**. Put it first inside the printable region, then call `window.print()`. `@media print` hides chrome, buttons, nav; tables render as real tables; `.no-print` hides anything else; `.print-thermal` = 76 mm width. |

### Forms - the standard recipe
```js
const form = el('form.stack', { novalidate: true, id: 'grn' }, el('div.form-error', { role: 'alert' }), /* fields */);
const draft = ERP.drafts.bind(form, { page: 'raw-materials', id: 'grn' });   // autosave + "Draft restored" banner; never stores passwords
// on success: draft.clear()
```
* Name every control (`name="lines[0].qty_purchase"`): `formError` maps server `fields` keys (dotted/indexed too) to controls via `[name=...]`, focuses the first, scrolls to it.
* Mark secrets `data-nodraft`. Drafts are keyed per user + page + form and survive reload, outage and re-login.
* Put the primary submit in a sticky bar: `el('div.action-bar', {}, el('button.btn.primary', {type:'submit'}, el('span.btn-icon', {}, icon('save')), 'Save'))` (sits above the bottom nav on phones). Inside dialogs pass the button via `actions` and `form="<formid>"`.
* Inputs are >= 48 px high, 16 px font. Add `inputmode` (`numeric`, `decimal`, `tel`). Show helper text (`.help`) before the user can err.

## 5. Live updates (`ERP.live`)
```js
ERP.live.start('qc', { qc: refreshQueue, production: refreshQueue, '*': refreshAll });
```
* Page key = SSE `?page=`. Topics: `stock materials production qc qr trips jars maintenance alerts master users settings admin`.
* One function per topic; `'*'` runs for any topic **except** `alerts`/`users` (register those explicitly if the page renders them, e.g. dashboard).
* Debounced 300 ms per function, never concurrent (coalesced), re-run after every **re**connect and on `resync`; stream pauses while the tab is hidden; closed on `pagehide`.
* `shell.js` already handles: `alerts` -> bell badge, `users` -> re-fetch `/api/me`, rebuild nav / redirect if the page is no longer allowed, `settings` -> `/api/ref/settings` (QR flag), `session_ended` -> login, `server_restarting` -> banner.
* **Handlers must refresh list regions only** - never rebuild a form or dialog the user is typing in. Preserve filters/scroll (re-render into the same container with the same state).
* `ERP.live.status` / `onStatus(fn)`: `live | reconnecting | paused | off` (app-bar dot).

## 6. Tables become cards on phones
```js
ui.cardsTable({
  columns: [
    { label: 'Batch',  cls: 'k', render: (r) => [el('b', {text: r.batch_no}), el('small', {text: r.product_name})] },   // k = card title
    { label: 'QC',     cls: 's', render: (r) => ui.badge(r.qc_status) },                                              // s = badge, top-right on mobile
    { label: 'Crates', cls: 'num', render: (r) => ui.fmtCrates(r.crates, r.crates_per_pallet) },
    { label: 'Expiry', render: (r) => ui.fmtDate(r.expiry_date), hideMobile: true },
    { label: '',       cls: 'a', render: (r) => el('div.btn-row', {}, el('button.btn.secondary.sm', {type:'button'}, 'Open')) },   // a = full-width actions
  ],
  rows, onRow: (r) => open(r), emptyState: { icon: 'box', title: 'No batches', text: 'Create the first batch.' },
});
```
Desktop = normal table (`.tbl`); < 768 px each row is a card: key + badge on top, 2-column mini grid of labelled values (`data-label` comes from `label`), `w` = full-width cell, `a` = actions. Don't hand-write `<table>` markup for lists.

## 7. Godowns
`ERP.godown.current()` (`id` | `'all'` | null), `.param()` -> `{location_id}`, `.currentObj()`, `.list`, `.multiple`, `.onChange(fn)`, `.set(id)`. The switcher is hidden when the user has one godown; choice persists per user (localStorage).
**Always send `location_id` explicitly** on stock-touching calls: spread `...ERP.godown.param()` into query (GET) or body (POST). Reads may use `all` only if boot was called with `godownAll:true`. Re-fetch on `ERP.godown.onChange`.
If `/api/ref/locations` is unavailable the list is empty and `param()` is `{}` (server defaults to MAIN).

## 8. Navigation & permissions
`js/nav.js` `PAGES` registry lists every page (dashboard, raw-materials, production, qc, warehouse, godowns (child of warehouse, perm `warehouse`), qr-labels (only if `features.qr_labels`), loading, jars, maintenance, admin (is_admin)). Pages in the registry appear automatically from `/api/me` `allowed_pages`; nothing to edit when you add your page - the file just has to exist.
Mobile: bottom nav = first 4 allowed pages (+ the current one) + "More" drawer; >= 1024 px sticky sidebar. `ui.config.qrLabels` tells whether QR UI should exist - **hide all QR/sticker UI when false** (the API answers 409 `FEATURE_DISABLED`).
Admins: `ERP.me.is_admin`. Never trust the UI for security - the server enforces everything; the UI hides and redirects.

## 9. CRUD component (`js/crud.js`)
`createCrud(hostEl, {entity, title, singular, endpoint, columns, fields, rowActions, ...})` -> list + search + active/inactive filter + create/edit dialog + deactivate/reactivate.
Field types `text textarea tel password int decimal select refselect bool date checks`; extras `required nullable showWhen(values) createOnly readOnlyOnEdit optionsFrom{url,value,label,filter} unit help upper`.
PUT bodies contain **only changed fields** (backend PUT is partial + strict: never send `id`, `created_at`, `*_name`). See `js/pages/admin-master.js` for ten examples.

## 10. Rules of the road (mistake-proofing)
1. Entry screens: dry-run + `reviewSheet` for stock-affecting submits; `postConfirm` for confirmable operations; irreversible actions use `confirmDialog({danger:true})`; corrections are void/reverse documents with a reason (`promptDialog({min:5})`).
2. Numbers: `qtyInput` (inputmode, min/max, echo). Server re-validates everything.
3. Offline/outage: never discard input. `connectivity.js` shows the banner; drafts keep text; the submit shows "Not sent - Retry" and re-uses the key.
4. `INVALID_STATE` (someone else changed it): re-fetch the record and show `.banner.warn` "changed by someone else"; don't silently retry.
5. Touch targets >= 44x44 px, text >= 16 px inputs, status = text + icon + colour, focus rings visible, `prefers-reduced-motion` respected (all in the CSS - use the classes).
6. Don't cache or intercept `/api/*` in the service worker (it doesn't). Add new JS files to `PRECACHE` in `sw.js` only if they are shared; page files are cached on first visit (network-first HTML, stale-while-revalidate JS/CSS). Bump `VERSION` in `sw.js` after changing the shared modules' contracts.
7. Timestamps from the API are UTC ISO; format with `fmtDateTime` (plant time). Dates `YYYY-MM-DD` are plain dates (`fmtDate`).
8. One `<h1>` per page = the app-bar title (`boot({title})`). Page content starts with `.page-head` (subtitle + actions) or directly with cards.

## 10b. Page-specific CSS
Shared components live in `css/styles.css` (owned by D - ask via the Lead for shared changes; additions are welcome, renames are not). Page-only styles go in `static/css/pages/<page>.css`,
linked **after** `styles.css` in the page's `<head>`: `<link rel="stylesheet" href="/css/pages/loading.css">`. Use the design tokens (`var(--brand-600)`, `var(--s4)`, `var(--r)`, `var(--shadow)`), keep >= 44 px targets, and put mobile rules first (min-width media queries at 768/1024).
Identifiers that must not wrap in a card title (registration numbers, batch numbers): `el('b.nowrap', ...)`.

## 11. Testing a page (what D verified, copy for yours)
Playwright (`chromium.launch()`), 390x844 and 1280x800: no horizontal overflow (`document.documentElement.scrollWidth <= innerWidth`), zero console errors / CSP violations / failed requests, visible controls >= 44x44, inputs >= 16 px, no `[onclick]`/inline scripts. Log in at **`http://localhost:<port>`** (not `127.0.0.1` - the `Secure` cookie needs a secure context).
