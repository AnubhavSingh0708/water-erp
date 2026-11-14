// warehouse-lib.js - helpers shared by the plant-floor and stock pages (raw-materials, production, qc, warehouse, godowns).
// Pure client logic: reference-data cache, godown helpers, dry-run -> review -> confirmed submit flow, plain-language error
// explanations, printable documents, tab/panel scaffolding and the stock/lot picker. Everything dynamic goes through el()/textContent.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import * as drafts from '/js/drafts.js';
import { godown } from '/js/nav.js';

const { el, icon } = ui;
export { el, icon };

/* ------------------------------------------------------------------------------------------------ labels */
export const ZONE_LABEL = {
  RM_STORE: 'Raw store', BLOW_FLOOR: 'Blow floor', QC_QUARANTINE: 'QC quarantine', FG_READY: 'Finished goods',
  SCRAP_YARD: 'Scrap yard', JAR_EMPTY: 'Empty jars', JAR_CLEAN: 'Clean jars', TRANSIT: 'In transit',
};
export const zoneLabel = (z) => ZONE_LABEL[z] || z || '-';
export const WO_REASONS = [
  ['BREAKAGE', 'Breakage'], ['EXPIRED', 'Expired'], ['CONTAMINATED', 'Contaminated'], ['SPILLAGE', 'Spillage'],
  ['QC_REJECT', 'QC rejected'], ['THEFT_LOSS', 'Theft / loss'], ['TRANSIT_DAMAGE', 'Damaged in transit'], ['OTHER', 'Other'],
];
export const ADJ_REASONS = [
  ['COUNT_CORRECTION', 'Count correction'], ['ENTRY_ERROR', 'Earlier entry was wrong'], ['FOUND', 'Stock found'], ['DAMAGE', 'Damage'], ['OTHER', 'Other'],
];
export const CATEGORY_LABEL = {
  PREFORM: 'Preform', CAP: 'Cap', LABEL: 'Label', SLEEVE: 'Sleeve', SHRINK_FILM: 'Shrink film', CARTON: 'Carton', MINERAL: 'Mineral',
  CHEMICAL: 'Chemical', EMPTY_BOTTLE: 'Empty bottle', OTHER: 'Other',
};
export const SHIFTS = [['DAY', 'Day shift'], ['NIGHT', 'Night shift'], ['GENERAL', 'General']];
export const NIL_UUID = '00000000-0000-0000-0000-000000000000';

/* ------------------------------------------------------------------------------------------------ small DOM helpers */
export const settings = () => (ui.config && ui.config.settings) || {};
export const opt = (value, label, extra = {}) => ({ value, label, ...extra });
export const optionsOf = (arr) => arr.map(([v, l]) => ({ value: v, label: l }));
export function banner(tone, ic, ...kids) { return el('div.banner.inline', { class: tone }, icon(ic), el('div.grow', {}, ...kids)); }
export function kv(rows) {
  return el('dl.kv', {}, rows.filter(Boolean).flatMap(([k, v]) => [el('dt', { text: k }), el('dd', {}, v === null || v === undefined || v === '' ? '-' : v)]));
}
export function card(title, ic, ...kids) {
  return el('section.card', {}, title ? el('div.card-head', {}, el('h2', {}, ic ? icon(ic) : null, title)) : null, ...kids);
}
/** Primary / secondary button with an icon and a label span that can be swapped ("Not sent - Retry"). */
export function btn(label, ic, { kind = 'primary', type = 'button', onClick, cls = '', form, sm = false } = {}) {
  const b = el('button.btn', { type, class: `${kind} ${sm ? 'sm' : ''} ${cls}`.trim(), form, on: onClick ? { click: onClick } : undefined },
    ic ? el('span.btn-icon', {}, icon(ic, sm ? 'sm' : '')) : null, el('span.lbl', { text: label }));
  b.dataset.label = label;
  return b;
}
export function setBtnLabel(b, text) { const s = b.querySelector('.lbl'); if (s) s.textContent = text; }
export const resetBtnLabel = (b) => setBtnLabel(b, b.dataset.label || '');
export function actionBar(...kids) { return el('div.action-bar', {}, ...kids); }
export function backBar(label, onBack, ...right) {
  return el('div.row.between.back-bar', {}, el('button.btn.ghost.sm', { type: 'button', on: { click: onBack } }, icon('chevron-left', 'sm'), label), el('div.btn-row', {}, ...right));
}
export function chip(text, tone = '') { return el('span.chip-s', { class: tone, text }); }
export const dash = (v) => (v === null || v === undefined || v === '' ? '-' : v);
export const num = (v, dp = 4) => ui.fmtNum(v, dp);
export const qty = (v, unit, dp = 4) => ui.fmtQty(v, unit, dp);
/** crates value with the pallet breakdown underneath when the pack config is known. */
export function cratesCell(v, cpp, unit = 'crates') {
  const n = Math.round(ui.toNum(v));
  const main = `${ui.fmtNum(n, 0)} ${unit}`;
  if (!cpp || n < cpp || unit !== 'crates') return el('span', { text: main });
  return el('span', {}, el('b', { text: main }), el('small.block', { text: ui.fmtCrates(n, cpp) }));
}
export const unitOfRow = (r) => (r.item_type === 'PRODUCT' ? (r.kind === 'RETURNABLE' ? 'jars' : 'crates') : r.unit || '');
export function daysLeftBadge(days) {
  if (days === null || days === undefined) return null;
  const d = Number(days);
  if (d < 0) return ui.badge('EXPIRED', `Expired ${-d} d ago`);
  if (d <= 7) return ui.badge('CRITICAL', `${d} d left`, 'bad');
  if (d <= 30) return ui.badge('DUE_SOON', `${d} d left`);
  return el('span.small.muted', { text: `${d} d left` });
}
export function daysBetween(ymd) {
  if (!ymd) return null;
  const [y, m, d] = ymd.split('-').map(Number); const [ty, tm, td] = ui.todayPlant().split('-').map(Number);
  return Math.round((Date.UTC(y, m - 1, d) - Date.UTC(ty, tm - 1, td)) / 86400000);
}
/** Run `fn` and surface API errors as a toast (used by buttons that are not part of a form). */
export async function guarded(button, fn) {
  try { return await api.busy(button, fn); } catch (e) { ui.toastError(e); return undefined; }
}
export function offlineNote(host, e) {
  if (e && e.network) { host.dataset.stale = '1'; return true; }
  return false;
}

/* ------------------------------------------------------------------------------------------------ godowns */
export const gdn = godown;
export const isAll = () => godown.current() === 'all';
export const godownName = (id) => (godown.list.find((g) => g.id === id) || {}).name || (id === NIL_UUID ? '' : '');
/** Query fragment for reads (may be 'all'). */
export const readParam = () => godown.param();
/** The concrete godown a write acts on: the selected one, else the first allowed (production flows: first production site). */
export function writeLocation({ production = false } = {}) {
  const list = godown.list || [];
  const cur = godown.current();
  const pool = production ? list.filter((g) => g.is_production_site) : list;
  const sel = pool.find((g) => g.id === cur);
  return sel || pool.find((g) => g.kind === 'PLANT') || pool[0] || null;
}
export function locationOptions({ production = false } = {}) {
  const list = godown.list || [];
  return (production ? list.filter((g) => g.is_production_site) : list).map((g) => ({ value: g.id, label: `${g.name}${g.code ? ` (${g.code})` : ''}` }));
}
/** Godown <select> for forms; hidden (single godown) unless the user has a choice. Returns {field, get()}. */
export function godownField({ production = false, label = 'Godown', name = 'location_id' } = {}) {
  const options = locationOptions({ production });
  const w = writeLocation({ production });
  const f = ui.field({ label, name, type: 'select', options, value: w ? w.id : '' });
  if (options.length <= 1) f.el.hidden = true;
  return { field: f, el: f.el, get: () => f.get() || (w ? w.id : undefined), count: options.length };
}
export function allGodownsNotice() {
  return banner('info', 'info', el('b', { text: 'Showing all godowns. ' }), 'Choose one godown in the top bar to make changes.');
}

/* ------------------------------------------------------------------------------------------------ reference data cache */
const cache = new Map();
async function ref(path, ttl = 60000) {
  const hit = cache.get(path);
  if (hit && Date.now() - hit.at < ttl) return hit.data;
  const data = await api.get(path);
  cache.set(path, { at: Date.now(), data });
  return data;
}
export const refProducts = () => ref('/api/ref/products');
export const refMaterials = () => ref('/api/ref/raw-materials');
export const refSuppliers = () => ref('/api/ref/suppliers');
export const refEquipment = () => ref('/api/ref/equipment');
export const refVehicles = () => ref('/api/ref/vehicles');
export function invalidateRef() { cache.clear(); }
export async function unitMap() {
  const [p, m] = await Promise.all([refProducts().catch(() => []), refMaterials().catch(() => [])]);
  const map = new Map();
  m.forEach((x) => map.set(x.id, x.stock_unit));
  p.forEach((x) => map.set(x.id, x.kind === 'RETURNABLE' ? 'jars' : 'crates'));
  return map;
}

/* ------------------------------------------------------------------------------------------------ errors in plain language */
export function explain(e) {
  if (!e || !e.code) return null;
  const d = e.details || {};
  const note = (...kids) => el('div.explain', {}, ...kids);
  const todo = (t) => (e.hint ? null : el('p.small', { text: t }));
  if (e.code === 'INSUFFICIENT_STOCK') {
    if (Array.isArray(d.shortages) && d.shortages.length) {
      return note(el('b', { text: 'Not enough stock for:' }), el('ul', {}, d.shortages.map((s) => el('li', {}, el('b', { text: s.name }),
        ` - needs ${ui.fmtNum(s.required, 4)}, only ${ui.fmtNum(s.available, 4)} available${s.zone ? ` in ${zoneLabel(s.zone)}` : ''}.`))),
      todo('What to do: receive more stock (Raw materials > Receive), make the batch smaller, or ask the store to move stock into the right zone.'));
    }
    if (d.available !== undefined) {
      return note(`Only ${ui.fmtNum(d.available, 4)} available${d.zone ? ` in ${zoneLabel(d.zone)}` : ''} but ${ui.fmtNum(d.required, 4)} is needed.`,
        todo('What to do: lower the quantity, pick another lot, or check the stock page for where the goods really are.'));
    }
  }
  if (e.code === 'RESERVED_STOCK') {
    const trips = Array.isArray(d.trips) ? d.trips : [];
    return note(el('b', { text: 'These crates are held for loading.' }),
      trips.length ? el('ul', {}, trips.map((t) => el('li', { text: `${t.trip_no}: ${ui.fmtNum(t.crates, 0)} crates reserved` }))) : null,
      todo('What to do: open Loading, remove those crates from the trip (or finish the trip), then repeat this action. This cannot be overridden.'));
  }
  if (e.code === 'QC_LOCKED') return note('This batch has not been approved by Lab QC, so its stock is locked.', todo('What to do: wait for QC approval, or choose an approved batch.'));
  if (e.code === 'DUPLICATE') return e.hint ? null : note(el('p.small', { text: 'What to do: this has already been entered. Look for it in the list instead of entering it again, or void the earlier one first if it was wrong.' }));
  return null;
}
/** formError + plain-language explanation block; INVALID_STATE calls onStale (re-fetch + "changed by someone else"). */
export function showError(form, e, { onStale, btnEl } = {}) {
  if (!e || e.redirecting || e.cancelled || e.aborted) return false;
  ui.formError(form, e);
  const box = form.querySelector('.form-error');
  const x = explain(e);
  if (x && box) box.append(x);
  if (e.network && btnEl) setBtnLabel(btnEl, 'Not sent - Retry');
  if (e.code === 'INVALID_STATE' && onStale) onStale(e);
  return true;
}
export function staleBanner(text = 'Someone else changed this while you were looking at it. The screen was refreshed - check it and try again.') {
  return banner('warn', 'alert', el('b', { text: 'Changed by someone else. ' }), text);
}

/* ------------------------------------------------------------------------------------------------ dry run -> review -> confirmed submit */
const zoneWithPlace = (e) => {
  const place = (e.location_id && godown.list.length > 1) ? (godown.list.find((g) => g.id === e.location_id) || {}).name : '';
  const z = e.zone === 'TRANSIT' ? 'In transit' : zoneLabel(e.zone);
  return [place, z].filter(Boolean).join(' / ');
};
export function prepEffects(effects, units) {
  return (effects || []).map((e) => ({
    item_name: e.item, zone: zoneWithPlace(e), delta: e.delta, after: e.after,
    unit: units ? units.get(e.item_id) : undefined,
  }));
}
/** One submit action: its idempotency key and the "already reviewed, only the network failed" memory. */
export function makeFlow() { return { act: api.action(), retrySig: null }; }
/**
 * submitDocument({flow, form, button, path, body, title, confirmText, danger, summary, dry=true, sendConfirm=true, onStale})
 * dry-run -> review sheet -> real POST (same Idempotency-Key on retry). Resolves with the created document or null
 * (user went back / error shown in the form). A network failure after the review leaves "Not sent - Retry" and the next
 * press skips straight to the resend with the SAME key (the server replays the first success instead of duplicating).
 */
export async function submitDocument({ flow, form, button, path, body, title, confirmText, danger = false, summary, dry = true, sendConfirm = true, onStale, onError, units, method = 'post' }) {
  ui.clearFormError(form);
  if (button) resetBtnLabel(button);
  const sig = JSON.stringify([path, body]);
  try {
    let warnings = [];
    if (dry && flow.retrySig !== sig) {
      const d = await api.busy(button, () => api.dryRun(path, body));
      const u = units || await unitMap().catch(() => null);
      const ok = await ui.reviewSheet({ title, effects: prepEffects(d.effects, u), warnings: d.warnings || [], confirmText, danger, summary });
      if (!ok) return null;
      warnings = d.warnings || [];
    }
    const payload = sendConfirm && (sendConfirm === 'always' || warnings.length || flow.retrySig === sig) ? { ...body, confirm: true } : body;
    const call = () => (method === 'put' ? flow.act.put(path, payload) : flow.act.postConfirm(path, payload));
    try {
      const res = await api.busy(button, call);
      flow.retrySig = null;
      return res;
    } catch (e) {
      if (e && e.network) flow.retrySig = sig;
      throw e;
    }
  } catch (e) {
    showError(form, e, { onStale, btnEl: button });
    if (onError) onError(e);
    return null;
  }
}
/** Simple (no dry run) action with the same error handling. */
export async function submitPlain({ flow, form, button, path, body, onStale, onError, method = 'post' }) {
  ui.clearFormError(form);
  if (button) resetBtnLabel(button);
  try {
    const call = () => (method === 'put' ? flow.act.put(path, body) : flow.act.post(path, body));
    return await api.busy(button, call);
  } catch (e) { showError(form, e, { onStale, btnEl: button }); if (onError) onError(e); return null; }
}

/* ------------------------------------------------------------------------------------------------ drafts for forms with dynamic rows */
/**
 * stateDraft({page, id, form, get, set}) - autosaves get() (a JSON-able object) on any input/change inside `form` and
 * restores it through set(state) (which must rebuild the rows). Shows the standard "Draft restored" banner.
 */
export function stateDraft({ page, id, form, get, set }) {
  const initial = JSON.stringify(get());
  const save = ui.debounce(() => { const v = get(); if (JSON.stringify(v) === initial) drafts.clear(page, id); else drafts.save(page, id, v); }, 400);
  form.addEventListener('input', save); form.addEventListener('change', save);
  let bar = null;
  const handle = {
    touch: save,
    clear() { save.cancel(); drafts.clear(page, id); if (bar) { bar.remove(); bar = null; } },
    discard() { handle.clear(); set(JSON.parse(initial)); },
    restore() {
      const d = drafts.load(page, id);
      if (!d || !d.values) return false;
      try { set(d.values); } catch { drafts.clear(page, id); return false; }
      bar = el('div.banner.info.inline.draft-banner', { role: 'status' }, icon('history'), el('div.grow', {}, el('b', { text: 'Draft restored' }), ' - your earlier entries were kept. Check them before saving.'),
        el('button.btn.ghost.sm', { type: 'button', on: { click: () => handle.discard() } }, 'Discard draft'));
      form.prepend(bar);
      return true;
    },
  };
  return handle;
}

/* ------------------------------------------------------------------------------------------------ printing */
function printRoot() {
  let r = document.getElementById('print-root');
  if (!r) { r = el('div#print-root.print-root'); document.body.append(r); }
  return r;
}
/** Print a document: renders header + body into #print-root (everything else is hidden by the print CSS). */
export function printNode({ title, docNo, subtitle, body }) {
  const r = printRoot();
  ui.mount(r, ui.printHeader({ title, docNo, subtitle }), body.cloneNode(true));
  document.body.classList.add('printing');
  const done = () => { document.body.classList.remove('printing'); window.removeEventListener('afterprint', done); };
  window.addEventListener('afterprint', done);
  setTimeout(() => { try { window.print(); } catch { /* ignore */ } setTimeout(done, 1500); }, 60);
}
/** Show a document in a dialog with a Print button. `build()` returns the printable body (a fresh node each call). */
export function showDocument({ title, docNo, subtitle, build, actions = [], dlgTitle }) {
  const body = build();
  const print = btn('Print', 'print', { kind: 'primary', onClick: () => printNode({ title, docNo, subtitle, body }) });
  const close = btn('Close', null, { kind: 'secondary', onClick: () => dlg.close() });
  const dlg = ui.openDialog({ title: dlgTitle || title, body: el('div.doc-preview', {}, el('div.doc-head', {}, el('b', { text: title }), el('span.mono', { text: docNo || '' }), subtitle ? el('span.small.muted', { text: subtitle }) : null), body), actions: [close, ...actions, print], wide: true });
  return dlg;
}
/** Plain table for printed documents. cols=[{h, v(row), num}] */
export function docTable(cols, rows, { empty = 'No lines' } = {}) {
  if (!rows.length) return el('p.muted', { text: empty });
  return el('table.tbl.doc-table', {}, el('thead', {}, el('tr', {}, cols.map((c) => el('th', { class: c.num ? 'num' : '', text: c.h })))),
    el('tbody', {}, rows.map((r) => el('tr', {}, cols.map((c) => el('td', { class: c.num ? 'num' : '' }, c.v(r) ?? '-'))))));
}
export function signatureBlock(labels = ['Prepared by', 'Checked by', 'Received by']) {
  return el('div.sign-row', {}, labels.map((l) => el('div.sign', {}, el('div.sign-line'), el('small', { text: l }))));
}

/* ------------------------------------------------------------------------------------------------ tabs + panels */
/**
 * tabbed(host, defs, {hashes}) - defs: [{id,label,icon,render(panel)->Promise|void, badge?}].
 * Panels render lazily the first time they are shown. Returns {select(id), current(), panel(id), tabs}.
 */
export function tabbed(host, defs, { initial } = {}) {
  const panels = {}; const rendered = new Set(); let active = null;
  host.querySelectorAll(':scope > [aria-busy]').forEach((n) => n.remove());   // boot()'s skeleton
  const wrap = el('div.tabbed');
  const t = ui.tabs({ tabs: defs.map((d) => ({ id: d.id, label: d.label, icon: d.icon })), active: initial || defs[0].id, onChange: (id) => show(id) });
  const stick = el('div.sticky-tabs', {}, t.el);
  const body = el('div.tab-body');
  defs.forEach((d) => { panels[d.id] = el('div.tab-panel', { id: `panel-${d.id}`, role: 'tabpanel', 'aria-labelledby': `tab-${d.id}`, hidden: true }); body.append(panels[d.id]); });
  wrap.append(stick, body); host.append(wrap);
  function show(id) {
    if (!panels[id]) id = defs[0].id;
    active = id; t.select(id);
    defs.forEach((d) => { panels[d.id].hidden = d.id !== id; });
    try { history.replaceState(null, '', `#${id}`); } catch { /* ignore */ }
    if (!rendered.has(id)) { rendered.add(id); const d = defs.find((x) => x.id === id); try { const r = d.render(panels[id]); if (r && r.catch) r.catch((e) => ui.mount(panels[id], ui.errorState(e, () => { rendered.delete(id); show(id); }))); } catch (e) { ui.mount(panels[id], ui.errorState(e)); } }
  }
  const api_ = { select: (id) => show(id), current: () => active, panel: (id) => panels[id], tabs: t, rendered: (id) => rendered.has(id), invalidate: (id) => rendered.delete(id) };
  show(initial && panels[initial] ? initial : defs[0].id);
  return api_;
}
export const hashTab = (ids, fallback) => { const h = (location.hash || '').replace('#', ''); return ids.includes(h) ? h : fallback; };

/** Render `fn()` into `host`; the first time with a skeleton, later silently (keeps old content on network errors). */
export async function refreshInto(host, fn, { quiet = true } = {}) {
  const first = !host.dataset.loaded;
  if (first) ui.mount(host, ui.skeletonCards(2));
  try {
    const node = await fn();
    ui.mount(host, node); host.dataset.loaded = '1'; host.querySelector(':scope > .stale-note')?.remove();
    return true;
  } catch (e) {
    if (e && (e.redirecting || e.aborted)) return false;
    if (!first && quiet && (e.network || e.status >= 500)) { if (!host.querySelector(':scope > .stale-note')) host.prepend(el('p.small.muted.stale-note', {}, icon('wifi-off', 'sm'), ' Showing the last data - server unreachable.')); return false; }
    ui.mount(host, ui.errorState(e, () => { delete host.dataset.loaded; refreshInto(host, fn, { quiet }); }));
    return false;
  }
}

/* ------------------------------------------------------------------------------------------------ stock / lot picker */
const RAW_ZONES = ['RM_STORE', 'BLOW_FLOOR', 'SCRAP_YARD'];
/**
 * stockPicker({locationId(), zones, mode:'existing'|'adjust', onChange}) -> {el, get(), reload()}
 * Zone -> item -> lot, always showing what is really there (qty, FEFO, crates reserved by loading trips -> free).
 * mode 'existing' lists only stock that exists in the godown (write-off, zone move, godown transfer);
 * mode 'adjust' lists every item that may be held in the zone (so a found item can be added).
 */
export function stockPicker({ locationId, zones, mode = 'existing', onChange, namePrefix = 'pk' }) {
  const zoneF = ui.field({ label: 'Where in the godown', name: `${namePrefix}_zone`, type: 'select', options: zones.map((z) => ({ value: z, label: zoneLabel(z) })) });
  const itemF = ui.field({ label: 'Item', name: `${namePrefix}_item`, type: 'select', options: [{ value: '', label: 'Loading...' }] });
  const lotF = ui.field({ label: 'Lot / batch', name: `${namePrefix}_lot`, type: 'select', options: [{ value: '', label: 'Choose an item first' }] });
  [zoneF, itemF, lotF].forEach((f) => { f.input.dataset.nodraft = '1'; });
  const info = el('div.pick-info', { 'aria-live': 'polite' });
  const root = el('div.stack.tight.picker', {}, zoneF.el, itemF.el, lotF.el, info);
  let items = []; let lots = []; let seq = 0;
  const fire = () => { if (onChange) onChange(get()); };
  const itemKey = (x) => `${x.item_type}:${x.item_id}`;
  function setOptions(f, options, value) {
    ui.clear(f.input);
    options.forEach((o) => f.input.append(el('option', { value: o.value, text: o.label, selected: String(o.value) === String(value ?? '') })));
    f.input.value = value ?? '';
  }
  async function loadItems() {
    const my = ++seq; const zone = zoneF.get(); const loc = locationId();
    setOptions(itemF, [{ value: '', label: 'Loading...' }], ''); setOptions(lotF, [{ value: '', label: 'Choose an item first' }], ''); lots = []; paint();
    try {
      if (mode === 'existing') {
        const rows = await api.get('/api/warehouse/stock', { location_id: loc, zone });
        if (my !== seq) return;
        items = rows.filter((r) => ui.toNum(r.qty) > 0).map((r) => ({ ...r, unit: unitOfRow(r) }));
      } else {
        const [mats, prods] = await Promise.all([refMaterials(), refProducts()]);
        if (my !== seq) return;
        items = [];
        if (RAW_ZONES.includes(zone)) mats.forEach((m) => items.push({ item_type: 'RAW', item_id: m.id, name: m.name, code: m.code, unit: m.stock_unit }));
        if (zone === 'FG_READY' || zone === 'SCRAP_YARD') prods.forEach((p) => items.push({ item_type: 'PRODUCT', item_id: p.id, name: p.name, code: p.code, unit: p.kind === 'RETURNABLE' ? 'jars' : 'crates', kind: p.kind, crates_per_pallet: p.crates_per_pallet }));
        if (zone === 'JAR_EMPTY' || zone === 'JAR_CLEAN') prods.filter((p) => p.kind === 'RETURNABLE').forEach((p) => items.push({ item_type: 'PRODUCT', item_id: p.id, name: p.name, code: p.code, unit: 'jars', kind: p.kind }));
        items.sort((a, b) => a.name.localeCompare(b.name));
      }
      setOptions(itemF, [{ value: '', label: items.length ? 'Choose an item...' : (mode === 'existing' ? 'No stock here' : 'No items for this zone') }, ...items.map((x) => ({ value: itemKey(x), label: mode === 'existing' ? `${x.name} - ${ui.fmtNum(x.qty, 4)} ${x.unit}` : x.name }))], '');
      if (items.length === 1) { itemF.input.value = itemKey(items[0]); await loadLots(); }
    } catch (e) { setOptions(itemF, [{ value: '', label: 'Could not load - tap Zone again' }], ''); ui.toastError(e); }
    paint(); fire();
  }
  async function loadLots() {
    const my = ++seq; const it = items.find((x) => itemKey(x) === itemF.get()); const zone = zoneF.get();
    lots = []; if (!it) { setOptions(lotF, [{ value: '', label: 'Choose an item first' }], ''); paint(); fire(); return; }
    setOptions(lotF, [{ value: '', label: 'Loading...' }], '');
    try {
      const d = await api.get('/api/warehouse/stock/item', { item_type: it.item_type, item_id: it.item_id, location_id: locationId() });
      if (my !== seq) return;
      lots = (d.lots || []).filter((l) => l.zone === zone).map((l) => ({ ...l, lot_id: l.lot_id === NIL_UUID ? null : l.lot_id }));
      const opts = lots.map((l) => ({ value: l.lot_id || 'none', label: lotLabel(l, it) }));
      if (mode === 'adjust' && it.item_type === 'RAW' && !lots.some((l) => !l.lot_id)) opts.push({ value: 'none', label: 'No lot (general stock)' });
      if (!opts.length) opts.push({ value: '', label: it.item_type === 'PRODUCT' && zone === 'FG_READY' ? 'No batch of this product in stock here' : 'No lot on record' });
      setOptions(lotF, opts.length > 1 ? [{ value: '', label: 'Choose the lot...' }, ...opts] : opts, opts.length === 1 && opts[0].value ? opts[0].value : '');
    } catch (e) { setOptions(lotF, [{ value: '', label: 'Could not load lots' }], ''); ui.toastError(e); }
    paint(); fire();
  }
  const lotLabel = (l, it) => {
    const name = l.lot_label || (l.lot_id ? l.lot_id.slice(0, 8) : (it.item_type === 'PRODUCT' ? 'Containers' : 'Opening stock'));
    const exp = l.expiry_date ? ` - exp ${ui.fmtDate(l.expiry_date)}` : '';
    const res = ui.toNum(l.reserved) > 0 ? ` - ${ui.fmtNum(l.reserved, 0)} held for loading` : '';
    return `${name} - ${ui.fmtNum(l.qty, 4)}${exp}${res}`;
  };
  function get() {
    const it = items.find((x) => itemKey(x) === itemF.get()); if (!it) return null;
    const v = lotF.get(); const lot = lots.find((l) => (l.lot_id || 'none') === v);
    if (!lot && !(mode === 'adjust' && it.item_type === 'RAW' && v === 'none')) return null;
    const have = lot ? ui.toNum(lot.qty) : 0; const reserved = lot ? (ui.toNum(lot.reserved) || 0) : 0;
    return {
      item_type: it.item_type, item_id: it.item_id, name: it.name, unit: it.unit, kind: it.kind, cpp: it.crates_per_pallet || null, zone: zoneF.get(),
      lot_id: lot ? lot.lot_id : null, lot_label: lot ? lot.lot_label : null, have: lot ? lot.qty : '0', reserved: String(reserved), free: String(Math.max(0, have - reserved)),
      expiry_date: lot ? lot.expiry_date : null, qc_status: lot ? lot.qc_status : null, whole: it.item_type === 'PRODUCT',
    };
  }
  function paint() {
    ui.clear(info);
    const g = get(); if (!g) return;
    const parts = [el('span', {}, 'On hand: ', el('b', { text: ui.fmtQty(g.have, g.unit, 4) }))];
    if (ui.toNum(g.reserved) > 0) parts.push(el('span.chip-s.warn', { text: `${ui.fmtNum(g.reserved, 0)} held for loading - free ${ui.fmtNum(g.free, 0)}` }));
    if (g.expiry_date) { const d = daysBetween(g.expiry_date); parts.push(el('span.chip-s', { class: d < 0 ? 'bad' : d <= 30 ? 'warn' : '', text: d < 0 ? `Expired ${ui.fmtDate(g.expiry_date)}` : `Expires ${ui.fmtDate(g.expiry_date)}` })); }
    info.append(...parts);
  }
  zoneF.input.addEventListener('change', loadItems);
  itemF.input.addEventListener('change', loadLots);
  lotF.input.addEventListener('change', () => { paint(); fire(); });
  loadItems();
  return { el: root, get, reload: loadItems, zoneInput: zoneF.input, fields: { zoneF, itemF, lotF } };
}

/* ------------------------------------------------------------------------------------------------ mini bar chart (CSS only) */
/** bars([{label, value, sub}], {max, unit}) -> horizontal CSS bars. */
export function bars(rows, { max, fmt = (v) => ui.fmtNum(v, 0), tone = '' } = {}) {
  const m = max || Math.max(1, ...rows.map((r) => ui.toNum(r.value) || 0));
  return el('div.bars', {}, rows.map((r) => {
    const v = ui.toNum(r.value) || 0; const i = el('i'); i.style.width = `${Math.max(v > 0 ? 2 : 0, Math.min(100, (v / m) * 100))}%`;
    return el('div.bar-row', { class: r.tone || tone }, el('span.bar-label', { text: r.label }), el('div.bar-track', {}, i), el('span.bar-val', {}, fmt(v), r.sub ? el('small', { text: r.sub }) : null));
  }));
}

/* ------------------------------------------------------------------------------------------------ range hints (water / QC readings) */
export function rangeChip(value, min, max, unit = '') {
  const n = ui.toNum(value);
  if (value === '' || value === null || value === undefined || !Number.isFinite(n)) return null;
  const ok = n >= ui.toNum(min) && n <= ui.toNum(max);
  return el('span.chip-s', { class: ok ? 'ok' : 'bad' }, icon(ok ? 'check-circle' : 'alert', 'sm'), ok ? `In range (${min}-${max}${unit})` : `Out of range (${min}-${max}${unit})`);
}

/** Modal error for actions that are not inside a form (void, cancel, quick actions). */
export function errorDialog(e, title = 'Could not do that') {
  if (!e || e.redirecting || e.cancelled || e.aborted) return;
  const x = explain(e);
  const ref = ui.errRef(e);
  const close = btn('OK', null, { kind: 'primary', onClick: () => dlg.close() });
  const dlg = ui.openDialog({ title, actions: [close], body: el('div.stack.tight', {},
    banner(e.network ? 'warn' : 'err', e.network ? 'wifi-off' : 'alert', el('b', { text: e.message || 'Something went wrong' }), e.hint ? el('div', { class: 'small', style: { fontWeight: 500 }, text: e.hint }) : null, ref ? el('span.ref.small', { text: ref }) : null), x) });
  return dlg;
}
/** Material option label with its unit conversion, e.g. "Cap 28 mm (bag = 5,000 pcs)". */
export const materialLabel = (m) => `${m.name} (${m.purchase_unit}${ui.toNum(m.stock_per_purchase_unit) !== 1 ? ` = ${ui.fmtNum(m.stock_per_purchase_unit, 4)} ${m.stock_unit}` : ''})`;
/** Numeric text input with a unit suffix and an echo line; returns {field, el, input, echo(text,tone), get(), set()}. */
export function unitInput({ name, label, unit, placeholder = '0', required = false, inputmode = 'decimal', help }) {
  const f = ui.field({ label, name, type: 'text', inputmode, placeholder, unit: unit || ' ', required, help });
  const echo = el('div.echo', { 'aria-live': 'polite' });
  f.el.insertBefore(echo, f.el.querySelector('.error'));
  const setEcho = (text, tone = '') => { echo.className = `echo ${tone}`.trim(); echo.textContent = text || ''; };
  return { field: f, el: f.el, input: f.input, echo: setEcho, get: () => f.get().trim(), set: (v) => f.set(v), setError: (m) => f.setError(m) };
}
/** Parse a decimal text; null when empty/invalid. */
export function dec(text) { const s = String(text ?? '').trim(); if (!/^\d{1,9}(\.\d{1,4})?$/.test(s)) return null; return Number(s); }
/** Why a quantity text is wrong ('' when fine). `whole` = integers only. */
export function qtyProblem(text, { whole = false, allowZero = false, max } = {}) {
  const s = String(text ?? '').trim();
  if (s === '') return 'Enter a quantity';
  if (!/^\d+(\.\d+)?$/.test(s)) return 'Use digits only (no minus sign, letters or commas)';
  if (whole && s.includes('.')) return 'Whole numbers only';
  if (/\.\d{5,}/.test(s)) return 'At most 4 decimal places';
  const n = Number(s);
  if (!allowZero && n <= 0) return 'Must be more than zero';
  if (n > 1e9) return 'That is too large - check the number';
  if (max !== undefined && max !== null && n > max) return `More than the maximum (${ui.fmtNum(max, 4)})`;
  return '';
}
export const isPositive = (text) => { const n = dec(text); return n !== null && n > 0; };
export const todayMinus = (days) => ui.addDays(ui.todayPlant(), -days);

export const toTop = () => { try { window.scrollTo({ top: 0, behavior: 'instant' }); } catch { window.scrollTo(0, 0); } };

export const TXN_LABEL = {
  GRN: 'Received (GRN)', GRN_VOID: 'GRN voided', RTV_OUT: 'Returned to supplier', BLOW_ISSUE: 'Issued to blow molding', BATCH_CONSUME: 'Used in batch', BATCH_WASTE: 'Batch waste',
  BATCH_OUTPUT: 'Batch output', QC_RELEASE: 'Released by QC', QC_REJECT: 'Rejected by QC', ADJUST: 'Adjustment', TRANSFER: 'Moved between zones', WRITE_OFF: 'Written off', DISPOSE: 'Disposed',
  GODOWN_TRANSFER_OUT: 'Sent to another godown', GODOWN_TRANSFER_IN: 'Received from godown', GODOWN_TRANSFER_CANCEL: 'Transfer cancelled', BLOW_OUTPUT: 'Blow output', BLOW_SCRAP: 'Blow scrap',
  TRANSIT_DAMAGE: 'Damaged in transit', TRANSFER_SHORT: 'Short in transit', GATE_PASS: 'Loaded (gate pass)', GATE_PASS_VOID: 'Gate pass voided', DISPATCH: 'Direct dispatch', DISPATCH_VOID: 'Dispatch voided',
  TRIP_RETURN: 'Returned from trip', TRIP_DAMAGED: 'Damaged on trip', JAR_PURCHASE: 'New jars bought', JAR_WASH: 'Jars washed', JAR_RECEIVE: 'Empty jars received',
};
export const txnLabel = (t) => TXN_LABEL[t] || String(t || '').replace(/_/g, ' ').toLowerCase().replace(/^\w/, (c) => c.toUpperCase());

/** Dry run -> review -> real POST for actions that are not inside a form (cancel, void). Errors open an error dialog. */
export async function dialogFlow({ flow, button, path, body, title, confirmText, danger = false, summary, units, sendConfirm = true }) {
  const sig = JSON.stringify([path, body]);
  try {
    let warnings = [];
    if (flow.retrySig !== sig) {
      const d = await api.busy(button, () => api.dryRun(path, body));
      const ok = await ui.reviewSheet({ title, effects: prepEffects(d.effects, units || await unitMap().catch(() => null)), warnings: d.warnings || [], confirmText, danger, summary });
      if (!ok) return null;
      warnings = d.warnings || [];
    }
    const payload = sendConfirm && (sendConfirm === 'always' || warnings.length || flow.retrySig === sig) ? { ...body, confirm: true } : body;
    try { const r = await api.busy(button, () => flow.act.postConfirm(path, payload)); flow.retrySig = null; return r; }
    catch (e) { if (e && e.network) flow.retrySig = sig; throw e; }
  } catch (e) { errorDialog(e, 'Not saved'); return null; }
}
