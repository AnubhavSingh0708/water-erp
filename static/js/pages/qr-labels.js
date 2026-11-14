// qr-labels.html - optional QR crate-set labels: pick a batch, generate sets (uniform / split / custom), list, print A4 sheet or thermal labels, delete unused.
// Only meaningful when features.qr_labels is on; otherwise a friendly "switched off" state (no /api/qr calls are made).
import { boot, ERP } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { el, icon, cratesNode, crates, run, explain, svgNode, actionFor } from '/js/pages/loading-common.js';

await boot({ page: 'qr-labels', title: 'QR labels' });   // guard: users with the permission always get in; the page explains when the feature is off
const main = document.getElementById('main');

function offState() {
  ui.mount(main, ui.empty({ icon: 'qr', title: 'QR labels are switched off', text: 'Loading works on stock by product and batch, so stickers are not needed. An administrator can enable them in Settings -> Features -> QR crate labels.',
    action: ERP.me.is_admin ? { label: 'Open settings', icon: 'sliders', onClick: () => { location.href = '/admin.html#settings'; } } : null }));
}
if (!ui.config.qrLabels) {
  offState();
  ERP.live.start('qr-labels', { settings: async () => { await ui.sleep(600); if (ui.config.qrLabels) location.reload(); } });
  await new Promise(() => {});
}

/* ---------------------------------------------------------------------------------------------- state */
const S = { batches: [], batch: null, sets: [], q: '', source: '', total: null };
const limit = 120;
const batchIdFromHash = () => { const m = /batch=([0-9a-f-]{36})/i.exec(location.hash); return m ? m[1] : null; };

/**
 * Batches for the picker. Preferred: GET /api/qr/batches?q=&limit= (works for label users without the production page, server-side search,
 * shows labelled / unlabelled crates). Older servers answer 404 -> fallback: production page list -> loading availability -> batches that already have sets.
 */
const can = (pg) => ERP.me.is_admin || (ERP.me.allowed_pages || []).includes(pg);
let qrListOk = true;                                    // flips to false (for the page's life) when the endpoint does not exist
async function loadBatches() {
  if (qrListOk) {
    try {
      const r = await api.get('/api/qr/batches', { q: S.q, limit });
      const items = Array.isArray(r) ? r : (r && r.items) || [];
      S.source = 'qr'; S.total = Array.isArray(r) ? items.length : (r && r.total) || items.length;
      S.batches = items.map((b) => ({ id: b.id, batch_no: b.batch_no, product_name: b.product_name, qc_status: b.qc_status, crates: b.crates, expiry_date: b.expiry_date, ...(b.crates_per_pallet !== undefined ? { cpp: b.crates_per_pallet } : {}), location_name: b.location_name, allocated: b.allocated_crates, free: b.free_crates }));
      return;
    } catch (e) { if (e.status === 404 || e.status === 405) qrListOk = false; else throw e; }
  }
  S.total = null;
  if (can('production')) try { const r = await api.get('/api/production/batches', { limit }); S.source = 'production'; S.batches = (r.items || []).map((b) => ({ id: b.id, batch_no: b.batch_no, product_name: b.product_name, qc_status: b.qc_status, crates: b.crates, expiry_date: b.expiry_date, cpp: b.crates_per_pallet, produced_on: b.produced_on })); return; }
  catch (e) { if (e.status !== 403) throw e; }
  if (can('loading')) try {
    const a = await api.get('/api/loading/availability'); S.source = 'availability';
    S.batches = a.map((b) => ({ id: b.batch_id, batch_no: b.batch_no, product_name: b.product_name, qc_status: b.qc_status, expiry_date: b.expiry_date, cpp: b.crates_per_pallet })); return;
  } catch (e) { if (e.status !== 403) throw e; }
  const r = await api.get('/api/qr/sets', { limit: 200 }); S.source = 'sets';
  const m = new Map(); r.items.forEach((x) => { if (!m.has(x.batch_id)) m.set(x.batch_id, { id: x.batch_id, batch_no: x.batch_no, product_name: x.product_name, qc_status: x.qc_status, expiry_date: x.expiry_date }); });
  S.batches = [...m.values()];
}
async function loadSets() { S.sets = S.batch ? await api.get(`/api/qr/batches/${encodeURIComponent(S.batch.id)}/sets`) : []; }

/* ---------------------------------------------------------------------------------------------- layout */
const searchIn = el('input.input', { type: 'search', placeholder: 'Search batch number or product', 'aria-label': 'Search batches', maxlength: 100, on: { input: ui.debounce(async () => { S.q = searchIn.value.trim(); if (S.source === 'qr') { try { await loadBatches(); } catch (e) { ui.toastError(e); return; } } paintPicker(); }, 250) } });
const pickerList = el('div.batch-list', { role: 'listbox', 'aria-label': 'Batches' });
const picker = el('section.card', {}, el('div.card-head', {}, el('h2', {}, icon('package'), '1. Choose a batch')), el('div.searchbar', {}, icon('search', 'sm'), searchIn), pickerList);
const panel = el('div.stack.loose');
const root = el('div.qr-layout', {}, picker, panel);
const headEl = el('div.page-head', {}, el('div', {}, el('p.sub', { text: 'Stickers for crate sets: scan them while loading to add exact crates to a trip.' })));
const printHost = el('div', { hidden: true });
ui.mount(main, headEl, root, printHost);
ui.mount(pickerList, ui.skeletonCards(2));

function paintPicker() {
  const needle = S.q.toLowerCase();
  const list = (S.source === 'qr' ? S.batches : S.batches.filter((b) => !needle || `${b.batch_no} ${b.product_name}`.toLowerCase().includes(needle))).slice(0, 60);
  if (!list.length) { ui.mount(pickerList, ui.empty({ icon: 'package', title: S.batches.length ? 'No batch matches' : 'No batches yet', text: S.batches.length ? 'Try another search.' : 'Batches appear here after Production creates them.' })); return; }
  ui.mount(pickerList, list.map((b) => el('button.batch-opt', { type: 'button', role: 'option', 'aria-selected': String(S.batch && S.batch.id === b.id), on: { click: () => select(b) } },
    el('span.grow', {}, el('b.nowrap', { text: b.batch_no }), el('span.small.muted', { text: `${b.product_name}${b.crates ? ` - ${crates(b.crates, b.cpp)}` : ''}` }),
      b.free !== undefined && b.free !== null ? el('span.small', { class: b.free ? 'muted' : 'ok-text', text: b.free ? `${ui.fmtNum(b.free, 0)} without label` : 'All crates labelled' }) : null,
      b.location_name && ERP.godown.multiple ? el('span.small.muted', { text: b.location_name }) : null), ui.badge(b.qc_status))));
  if (S.source === 'qr' && S.total > list.length) pickerList.append(el('p.small.muted.center', { text: `Showing ${list.length} of ${ui.fmtNum(S.total, 0)} - type to search.` }));
}
async function select(b) {
  S.batch = b; history.replaceState(null, '', `#batch=${b.id}`); paintPicker();
  ui.mount(panel, ui.skeletonCards(2));
  try { await loadSets(); if (b.crates === undefined || b.cpp === undefined) { const sh = await api.get(`/api/qr/batches/${encodeURIComponent(b.id)}/sheet`); b.crates = sh.batch.crates; b.cpp = sh.product.crates_per_pallet; b.expiry_date = sh.batch.expiry_date; b.qc_status = sh.batch.qc_status; } paintPanel(); setTimeout(() => panel.scrollIntoView({ block: 'start', behavior: 'smooth' }), 50); }
  catch (e) { if (!e.redirecting) ui.mount(panel, ui.errorState(e, () => select(b))); }
}

/* ---------------------------------------------------------------------------------------------- batch panel */
const roots = () => S.sets.filter((s) => !s.parent_set_id);
const labelled = () => roots().reduce((s, x) => s + x.original_crates, 0);
function paintPanel() {
  const b = S.batch; if (!b) { ui.clear(panel); return; }
  const lab = labelled(); const rem = Math.max(0, (b.crates || 0) - lab); const pct = b.crates ? Math.round((lab * 100) / b.crates) : 0;
  ui.mount(panel,
    el('section.card', {}, el('div.card-head', {}, el('h2', {}, icon('qr'), `${b.batch_no}`), ui.badge(b.qc_status)),
      el('p.small.muted', { text: `${b.product_name}${b.expiry_date ? ` - exp ${ui.fmtDate(b.expiry_date)}` : ''}` }),
      el('div.progress-label', {}, el('span', {}, el('b', { text: ui.fmtNum(lab, 0) }), ` of ${ui.fmtNum(b.crates || 0, 0)} crates have labels`), el('span', { text: `${pct}%` })), ui.progress(pct, rem === 0 ? 'ok' : ''),
      el('p.small', { class: rem ? '' : 'ok-text', text: rem ? `${crates(rem, b.cpp)} still ${rem === 1 ? 'has' : 'have'} no label.` : 'Every crate of this batch has a label.' }),
      b.qc_status === 'REJECTED' ? el('div.banner.err.inline', {}, icon('ban'), el('div.grow', { text: 'This batch was rejected by Lab QC - no labels can be created.' })) : null),
    rem > 0 && b.qc_status !== 'REJECTED' ? generateCard(rem) : null,
    setsCard());
}

/* generate */
function generateCard(rem) {
  const b = S.batch; const cpp = b.cpp || 0; const act = actionFor(`qr-gen:${b.id}`);
  const mode = ui.segmented({ label: 'How to split', value: 'UNIFORM', options: [{ value: 'UNIFORM', label: 'Same size' }, { value: 'COUNT', label: 'Number of sets' }, { value: 'CUSTOM', label: 'Custom sizes' }], onChange: (v) => { modeIn.value = v; paintMode(); } });
  const modeIn = el('input', { type: 'hidden', name: 'mode', value: 'UNIFORM' });
  const size = ui.field({ label: 'Crates per set', name: 'set_size', type: 'int', value: cpp ? String(cpp) : '', help: cpp ? `One pallet = ${cpp} crates. The last set may be smaller.` : 'The last set may be smaller.' });
  const count = ui.field({ label: 'Number of sets', name: 'count', type: 'int', value: '', help: 'The crates are shared out as evenly as possible.' });
  const custom = ui.field({ label: 'Sizes, separated by commas', name: 'sizes', type: 'text', value: '', placeholder: cpp ? `e.g. ${cpp}, ${cpp}, 20` : 'e.g. 60, 60, 20', inputmode: 'text', help: 'Each number is one sticker. They may add up to less than the unlabelled crates.' });
  const preview = el('div.qr-preview', { role: 'status', 'aria-live': 'polite' });
  const form = el('form.stack', { id: 'qr-gen', novalidate: true }, el('div.form-error', { role: 'alert' }), modeIn, size.el, count.el, custom.el, preview);
  const draft = ERP.drafts.bind(form, { page: 'qr-labels', id: `gen-${b.id}` });
  mode.set(modeIn.value || 'UNIFORM');
  const plan = () => {
    const m = modeIn.value;
    if (m === 'UNIFORM') { const n = Number(size.get()); if (!Number.isInteger(n) || n < 1) return { err: 'Enter how many crates go on one sticker' }; const a = new Array(Math.floor(rem / n)).fill(n); if (rem % n) a.push(rem % n); return { sizes: a, body: { mode: m, set_size: n } }; }
    if (m === 'COUNT') { const n = Number(count.get()); if (!Number.isInteger(n) || n < 1) return { err: 'Enter how many sets' }; if (n > rem) return { err: `At most ${rem} sets (one crate each)` }; const base = Math.floor(rem / n), ex = rem % n; return { sizes: Array.from({ length: n }, (_, i) => base + (i < ex ? 1 : 0)), body: { mode: m, count: n } }; }
    const parts = custom.get().split(/[\s,;+]+/).filter(Boolean); if (!parts.length) return { err: 'Type at least one size' };
    const a = parts.map(Number); if (a.some((x) => !Number.isInteger(x) || x < 1)) return { err: 'Every size must be a whole number of 1 or more' };
    return { sizes: a, body: { mode: m, sizes: a } };
  };
  function paintMode() { size.el.hidden = modeIn.value !== 'UNIFORM'; count.el.hidden = modeIn.value !== 'COUNT'; custom.el.hidden = modeIn.value !== 'CUSTOM'; paintPreview(); }
  function paintPreview() {
    const p = plan(); ui.clear(preview);
    if (p.err) { preview.append(el('span.muted.small', { text: p.err })); return; }
    const sum = p.sizes.reduce((s, x) => s + x, 0); const over = sum > rem;
    const groups = []; p.sizes.forEach((x) => { const g = groups[groups.length - 1]; if (g && g.n === x) g.k++; else groups.push({ n: x, k: 1 }); });
    preview.append(el('div.chk', { class: over ? 'bad' : 'ok' }, icon(over ? 'alert' : 'check-circle', 'sm'), over ? ` ${sum} crates is more than the ${rem} unlabelled - remove ${sum - rem}.` : ` ${p.sizes.length} ${p.sizes.length === 1 ? 'sticker' : 'stickers'}, ${sum} of ${rem} unlabelled crates covered${sum < rem ? ` (${rem - sum} stay unlabelled)` : ''}.`),
      el('div.chips', {}, groups.slice(0, 12).map((g) => el('span.chip-sm', { text: `${g.k} x ${g.n}` })), groups.length > 12 ? el('span.chip-sm', { text: '...' }) : null));
  }
  [size.input, count.input, custom.input].forEach((i) => i.addEventListener('input', paintPreview));
  const submit = el('button.btn.primary', { type: 'submit', form: 'qr-gen' }, el('span.btn-icon', {}, icon('qr')), 'Create labels');
  paintMode();
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    const p = plan(); if (p.err) { ui.formBanner(form, p.err, 'err'); return; }
    const sum = p.sizes.reduce((s, x) => s + x, 0); if (sum > rem) { ui.formBanner(form, `The sizes add up to ${sum}, but only ${rem} crates have no label.`, 'err'); return; }
    const ok = await ui.confirmDialog({ title: `Create ${p.sizes.length} label${p.sizes.length === 1 ? '' : 's'}?`, message: `${b.batch_no}: ${p.sizes.length} stickers covering ${ui.fmtNum(sum, 0)} crates (${crates(sum, cpp)}).`, warnings: b.qc_status !== 'APPROVED' ? ['The batch is not approved yet. Labels start in QUARANTINE and become loadable when QC approves.'] : [], confirmText: 'Create labels', cancelText: 'Go back' });
    if (!ok) return;
    const r = await run(submit, () => act.post(`/api/qr/batches/${encodeURIComponent(b.id)}/sets`, p.body), { form, refresh: () => select(b) });
    if (r) { draft.clear(); ui.toast.ok(`${r.length} label${r.length === 1 ? '' : 's'} created`, { sub: 'Print the sheet and stick them on the pallets.', action: { label: 'Print', onClick: () => printView() } }); await loadSets(); paintPanel(); }
  });
  return el('section.card', {}, el('div.card-head', {}, el('h2', {}, icon('plus'), '2. Create labels')), mode.el, form, el('div.action-bar', {}, submit));
}

/* sets list */
function setsCard() {
  const sets = S.sets;
  return el('section.card.pad0', {}, el('div.card-head', {}, el('h2', {}, icon('list'), `3. Labels${sets.length ? ` (${sets.length})` : ''}`), sets.length ? el('div.btn-row', {}, el('button.btn.primary.sm', { type: 'button', on: { click: () => printView() } }, icon('print', 'sm'), 'Print sheet')) : null),
    el('div', { style: { padding: '0 var(--s4) var(--s4)' } }, ui.cardsTable({ columns: [
      { label: 'Label', cls: 'k', render: (s) => [el('b', { text: s.set_label }), el('small', { text: s.code })] },
      { label: 'Status', cls: 's', render: (s) => ui.badge(s.status) },
      { label: 'Crates', cls: 'num', render: (s) => [cratesNode(s.crates, S.batch.cpp || (s.pallets !== null && s.pallets !== undefined && s.pallets > 0 ? Math.round(s.crates / s.pallets) : 0)), s.trip_no ? el('small.muted.nowrap', { style: { display: 'block' }, text: s.trip_no }) : null] },
      { label: '', cls: 'a', render: (s) => el('div.btn-row', {}, el('button.btn.secondary.sm.compact', { type: 'button', 'aria-label': `Print label ${s.set_label}`, on: { click: () => printView([s.id]) } }, icon('print', 'sm'), el('span.btn-text', { text: 'Print' })),
        !s.parent_set_id && ['QUARANTINE', 'AVAILABLE'].includes(s.status) ? el('button.btn.danger.outline.sm.compact', { type: 'button', 'aria-label': `Delete label ${s.set_label}`, on: { click: () => del(s) } }, icon('trash', 'sm'), el('span.btn-text', { text: 'Delete' })) : null) },
    ], rows: sets, emptyState: { icon: 'qr', title: 'No labels for this batch yet', text: 'Create them above, then print and stick them on the pallets.' } })));
}
async function del(s) {
  const ok = await ui.confirmDialog({ title: `Delete label ${s.set_label}?`, message: `${crates(s.crates)} become unlabelled again. Only labels that were never used on a trip can be deleted.`, confirmText: 'Delete label', danger: true });
  if (!ok) return;
  const act = actionFor(`qr-del:${s.id}`);
  const r = await run(null, async () => { await act.del(`/api/qr/sets/${encodeURIComponent(s.id)}`); return true; }, { refresh: () => select(S.batch) });
  if (r) { ui.toast.ok(`Label ${s.set_label} deleted`); await loadSets(); paintPanel(); }
}

/* ---------------------------------------------------------------------------------------------- print preview */
async function printView(onlyIds) {
  const b = S.batch;
  headEl.hidden = true; root.hidden = true; printHost.hidden = false; ui.mount(printHost, ui.spinner('Preparing labels...'));
  let sh;
  try { sh = await api.get(`/api/qr/batches/${encodeURIComponent(b.id)}/sheet`); } catch (e) { explain(e); back(); return; }
  let sets = sh.sets; if (onlyIds) sets = sets.filter((x) => onlyIds.includes(x.id));
  let unusedOnly = !onlyIds;
  const grid = el('div.sheet');
  const paint = () => {
    const list = unusedOnly ? sets.filter((x) => ['QUARANTINE', 'AVAILABLE'].includes(x.status)) : sets;
    ui.mount(grid, list.map((s) => sticker(s, sh)));
    count.textContent = `${list.length} sticker${list.length === 1 ? '' : 's'}`;
  };
  const count = el('span.small.muted');
  const toggle = onlyIds ? null : el('label.check.bare', {}, el('input', { type: 'checkbox', checked: true, on: { change: (e) => { unusedOnly = e.target.checked; paint(); } } }), 'Only labels not yet used on a trip');
  const doPrint = (thermal) => { document.querySelectorAll('style.print-page').forEach((n) => n.remove()); const ps = thermal ? Object.assign(document.createElement('style'), { className: 'print-page', textContent: '@page { size: 80mm 297mm; margin: 2mm; }' }) : null; if (ps) document.head.append(ps); document.body.classList.toggle('print-thermal-mode', thermal); grid.classList.toggle('thermal', thermal); const done = () => { if (ps) ps.remove(); document.body.classList.remove('print-thermal-mode'); grid.classList.remove('thermal'); window.removeEventListener('afterprint', done); }; window.addEventListener('afterprint', done); window.print(); setTimeout(done, 1500); };
  ui.mount(printHost, el('div.stack', {},
    el('div.no-print.stack', {}, el('button.btn.ghost.sm.back', { type: 'button', on: { click: back } }, icon('chevron-left', 'sm'), 'Back to labels'),
      el('div.page-head', {}, el('div', {}, el('b.big', { text: `Labels - ${sh.batch.batch_no}` }), el('div.small.muted', { text: sh.product.name })), count),
      toggle, el('div.btn-row', {}, el('button.btn.primary', { type: 'button', on: { click: () => doPrint(false) } }, el('span.btn-icon', {}, icon('print')), 'Print A4 sheet'), el('button.btn.secondary', { type: 'button', on: { click: () => doPrint(true) } }, icon('print'), 'Thermal labels (one per label)'))),
    ui.printHeader({ title: 'QR crate labels', docNo: sh.batch.batch_no, subtitle: sh.product.name }), grid));
  paint(); window.scrollTo({ top: 0 });
  function back() { printHost.hidden = true; ui.clear(printHost); headEl.hidden = false; root.hidden = false; window.scrollTo({ top: 0 }); }
}
function sticker(s, sh) {
  const svg = svgNode(s.svg); const cpp = sh.product.crates_per_pallet;
  return el('div.sticker', {}, el('div.st-qr', {}, svg || null), el('div.st-txt', {}, el('b.st-label', { text: s.set_label }), el('div.st-prod', { text: sh.product.name }), el('div', { text: `Batch ${sh.batch.batch_no}` }),
    el('div', { text: `${crates(s.crates, cpp)}` }), el('div', { text: `Exp ${ui.fmtDate(sh.batch.expiry_date)}` }), el('div.mono.st-code', { text: s.code }), el('div.st-plant', { text: sh.plant_name || ui.config.plantName })));
}

/* ---------------------------------------------------------------------------------------------- go */
try { await loadBatches(); } catch (e) { ui.mount(pickerList, ui.errorState(e, () => location.reload())); }
paintPicker();
const startId = batchIdFromHash();
if (startId) { const found = S.batches.find((x) => x.id === startId); if (found) await select(found); }
else ui.mount(panel, el('div.empty', {}, el('p.muted', { text: 'Choose a batch to create or print its labels.' })));
const refresh = async () => { if (document.querySelector('dialog[open]')) return; try { await loadBatches(); paintPicker(); if (S.batch) { const f = S.batches.find((x) => x.id === S.batch.id); if (f) Object.assign(S.batch, f); await loadSets(); if (!panel.querySelector('form:focus-within')) paintPanel(); } } catch { /* shown on next action */ } };
ERP.live.start('qr-labels', { qr: refresh, qc: refresh, production: refresh, settings: async () => { await ui.sleep(600); if (!ui.config.qrLabels) location.reload(); } });
