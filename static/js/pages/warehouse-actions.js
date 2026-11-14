// Warehouse "Actions" tab: adjustments, zone moves, write-offs (with voucher), physical stock counts.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { ERP } from '/js/shell.js';
import * as L from '/js/pages/warehouse-lib.js';

const { el, icon } = ui;
const PAGE = 'warehouse';
const loc = () => { const c = ERP.godown.current(); return c && c !== 'all' ? c : null; };
const withLoc = () => (loc() ? { location_id: loc() } : {});
const WO_LABEL = Object.fromEntries(L.WO_REASONS);

export function mountActions(panel, ctx) {
  const host = el('div.stack'); panel.append(host);
  let view = 'home'; let listsHost = null;

  const formView = (title, form, submit, sub) => { view = 'form'; L.toTop(); ui.mount(host, L.backBar('Actions', home), el('h2', { text: title }), sub ? el('p.section-sub', { text: sub }) : null, form, L.actionBar(submit)); };
  const godownLine = () => el('div.chip-row', {}, el('span.chip-s.info', {}, icon('pin', 'sm'), `Godown: ${ERP.godown.currentName() || 'default'}`));

  /* ------------------------------------------------------------------------------------------ home */
  function home() {
    view = 'home';
    const all = ERP.godown.current() === 'all';
    const tile = (label, sub, ic, fn) => el('button.action-tile', { type: 'button', disabled: all, on: { click: fn } }, el('span.ico', {}, icon(ic)), el('span.t', {}, el('b', { text: label }), el('small', { text: sub })), icon('chevron-right', 'sm'));
    listsHost = el('div.stack');
    ui.mount(host, all ? L.allGodownsNotice() : godownLine(),
      el('div.action-tiles', {}, tile('Adjust stock', 'Fix a quantity after counting, finding or an entry mistake', 'edit', adjustForm), tile('Move between zones', 'Raw store to blow floor, or stock to the scrap yard', 'transfer', moveForm),
        tile('Write off / destroy', 'Broken, expired, spilled or lost goods - with a printable voucher', 'trash', writeOffForm), tile('Physical stock count', 'Count a zone on a phone and post the differences', 'clipboard', () => newCount())),
      listsHost);
    loadLists();
  }
  async function loadLists() {
    if (!listsHost) return;
    const woHost = el('div'); const cntHost = el('div');
    ui.mount(listsHost, el('div.row.between', {}, el('h2', { text: 'Stock counts' })), cntHost, el('div.row.between', {}, el('h2', { text: 'Write-offs' }), (() => { const b = L.btn('CSV', 'download', { kind: 'secondary', sm: true, onClick: () => L.guarded(b, () => api.download('/api/warehouse/write-offs', { query: { format: 'csv', ...L.readParam() } })) }); return b; })()), woHost);
    const run = async (h, fn) => L.refreshInto(h, fn);
    await Promise.all([
      run(cntHost, async () => {
        const d = await api.get('/api/warehouse/counts', { limit: 6, ...L.readParam() });
        return ui.cardsTable({ columns: [{ label: 'Count', cls: 'k', render: (c) => [el('b', { text: c.cnt_no }), el('small', { text: `${L.zoneLabel(c.zone)} - ${ui.fmtDate(c.created_at)}` })] }, { label: 'Status', cls: 's', render: (c) => ui.badge(c.status === 'POSTED' ? 'DONE' : c.status === 'DRAFT' ? 'DRAFT' : 'CANCELLED', c.status === 'DRAFT' ? 'In progress' : undefined) },
          { label: 'Counted', cls: 'num', render: (c) => `${c.counted_lines} of ${c.total_lines} lines` }], rows: d.items, onRow: (c) => openCount(c.id), emptyState: { icon: 'clipboard', title: 'No counts yet', text: 'Start a count to check what is really on the shelves.' } });
      }),
      run(woHost, async () => {
        const d = await api.get('/api/warehouse/write-offs', { limit: 8, ...L.readParam() });
        return ui.cardsTable({ columns: [{ label: 'Voucher', cls: 'k', render: (w) => [el('b', { text: w.wo_no }), el('small', { text: `${w.item_name} - ${ui.fmtDate(w.created_at)}` })] }, { label: 'Reason', cls: 's', render: (w) => el('span.chip-s', { text: WO_LABEL[w.reason_code] || w.reason_code }) },
          { label: 'Quantity', cls: 'num', render: (w) => `${ui.fmtNum(w.qty, 4)}${w.item_type === 'PRODUCT' ? ' crates' : ''}` }, { label: 'From', render: (w) => L.zoneLabel(w.zone) }, { label: 'By', hideMobile: true, render: (w) => w.created_by_name }],
        rows: d.items, onRow: (w) => openVoucher(w.id), emptyState: { icon: 'trash', title: 'No write-offs', text: 'Written-off stock is listed here with its reason.' } });
      }),
    ]);
  }
  ctx.refreshers.actions = async () => { if (view === 'home') await loadLists(); };
  ERP.godown.onChange(() => { if (view !== 'count') home(); else home(); });

  /* ------------------------------------------------------------------------------------------ adjust */
  function adjustForm() {
    const picker = L.stockPicker({ locationId: loc, zones: ['RM_STORE', 'BLOW_FLOOR', 'FG_READY', 'SCRAP_YARD', 'JAR_EMPTY', 'JAR_CLEAN'], mode: 'adjust', onChange: upd });
    const dir = ui.segmented({ options: [{ value: 'add', label: 'Add stock' }, { value: 'remove', label: 'Remove stock' }], value: 'add', label: 'Add or remove', onChange: () => upd() });
    const q = L.unitInput({ name: 'qty', label: 'How much?', unit: '', required: true });
    const code = ui.field({ label: 'Reason', name: 'reason_code', type: 'select', required: true, options: [{ value: '', label: 'Choose a reason...' }, ...L.optionsOf(L.ADJ_REASONS)] });
    const reason = ui.field({ label: 'Explain what happened', name: 'reason', type: 'textarea', required: true, rows: 2, maxlength: 300, help: 'At least 5 letters, e.g. "Counted 118, system showed 120".' });
    const form = el('form.stack', { id: 'adj-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }), godownLine(), picker.el,
      el('div.card.stack', {}, el('div.field', {}, el('span.label', { text: 'Add or remove' }), dir.el), q.el, code.el, reason.el));
    const submit = L.btn('Review adjustment', 'check', { kind: 'primary', type: 'submit', form: 'adj-form' });
    const flow = L.makeFlow();
    function upd() {
      const g = picker.get(); const unitEl = q.field.el.querySelector('.unit'); unitEl.textContent = g ? g.unit : ' ';
      const n = L.dec(q.get());
      if (!g) { q.echo(''); return; }
      if (q.get() === '' || n === null) { q.echo(`On hand now: ${ui.fmtQty(g.have, g.unit, 4)}`); return; }
      const after = dir.value === 'add' ? ui.toNum(g.have) + n : ui.toNum(g.have) - n;
      q.echo(`${ui.fmtNum(g.have, 4)} -> ${ui.fmtNum(after, 4)} ${g.unit}${after < 0 ? ' - not enough stock' : ''}`, after < 0 ? 'bad' : '');
    }
    q.input.addEventListener('input', upd);
    form.addEventListener('submit', async (ev) => {
      ev.preventDefault(); ui.clearFormError(form);
      const g = picker.get(); let bad = false;
      if (!g) { ui.formError(form, { status: 422, message: 'Choose the zone, item and lot first.', fields: {} }); return; }
      const p = L.qtyProblem(q.get(), { whole: g.whole }); q.setError(p); if (p) bad = true;
      const n = L.dec(q.get());
      if (!p && dir.value === 'remove' && n > ui.toNum(g.have)) { q.setError(`Only ${ui.fmtNum(g.have, 4)} ${g.unit} on hand`); bad = true; }
      if (!p && dir.value === 'remove' && ui.toNum(g.reserved) > 0 && ui.toNum(g.have) - n < ui.toNum(g.reserved)) { q.setError(`${ui.fmtNum(g.reserved, 0)} crates are held for loading trips - only ${ui.fmtNum(g.free, 0)} are free to remove`); bad = true; }
      if (!code.get()) { code.setError('Choose a reason'); bad = true; } else code.setError('');
      if (reason.get().trim().length < 5) { reason.setError('Please explain (at least 5 letters)'); bad = true; } else reason.setError('');
      if (bad) { form.querySelector('.has-error input, .has-error select, .has-error textarea')?.focus(); return; }
      const body = { ...withLoc(), item_type: g.item_type, item_id: g.item_id, zone: g.zone, ...(g.lot_id ? { lot_id: g.lot_id } : {}), delta: `${dir.value === 'remove' ? '-' : ''}${q.get().trim()}`, reason: reason.get().trim(), reason_code: code.get() };
      const r = await L.submitDocument({ flow, form, button: submit, path: '/api/warehouse/adjustments', body, title: 'Review adjustment', confirmText: 'Save adjustment', summary: 'Adjustments are permanent entries in the stock ledger - they are corrected with another entry, never deleted.' });
      if (r) { ui.toast.ok(`${r.item}: balance is now ${ui.fmtNum(r.balance_after, 4)}`); (r.warnings || []).forEach((w) => ui.toast.warn(w)); home(); }
    });
    formView('Adjust stock', form, submit, 'Use this to correct the system after a count or when an entry was wrong. Stock held by QC cannot be adjusted here.');
  }

  /* ------------------------------------------------------------------------------------------ move */
  function moveForm() {
    const picker = L.stockPicker({ locationId: loc, zones: ['RM_STORE', 'BLOW_FLOOR', 'FG_READY', 'JAR_EMPTY', 'JAR_CLEAN'], mode: 'existing', onChange: upd });
    const to = ui.field({ label: 'Move to', name: 'to_zone', type: 'select', required: true, options: [{ value: '', label: 'Choose an item first' }] });
    const q = L.unitInput({ name: 'qty', label: 'How much?', unit: '', required: true });
    const note = ui.field({ label: 'Note', name: 'note', type: 'textarea', optional: true, rows: 2, maxlength: 500, help: 'Required (5+ letters) when moving to the scrap yard.' });
    const form = el('form.stack', { id: 'move-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }), godownLine(), picker.el, el('div.card.stack', {}, to.el, q.el, note.el));
    const submit = L.btn('Review move', 'check', { kind: 'primary', type: 'submit', form: 'move-form' });
    const flow = L.makeFlow();
    function dests(g) { if (!g) return []; if (g.item_type === 'PRODUCT') return ['SCRAP_YARD']; return g.zone === 'RM_STORE' ? ['BLOW_FLOOR', 'SCRAP_YARD'] : g.zone === 'BLOW_FLOOR' ? ['RM_STORE', 'SCRAP_YARD'] : ['SCRAP_YARD']; }
    let lastKey = '';
    function upd() {
      const g = picker.get(); const key = g ? `${g.item_id}${g.zone}` : ''; const unitEl = q.field.el.querySelector('.unit'); unitEl.textContent = g ? g.unit : ' ';
      if (key !== lastKey) { lastKey = key; ui.clear(to.input); const d = dests(g); to.input.append(el('option', { value: '', text: g ? 'Choose where it goes...' : 'Choose an item first' }), ...d.map((z) => el('option', { value: z, text: L.zoneLabel(z) }))); if (d.length === 1) to.input.value = d[0]; }
      const n = L.dec(q.get());
      if (!g || q.get() === '' || n === null) { q.echo(g ? `Free to move: ${ui.fmtQty(g.free, g.unit, 4)}` : ''); return; }
      q.echo(n > ui.toNum(g.free) ? `More than the ${ui.fmtNum(g.free, 4)} that can be moved` : `Moving ${ui.fmtNum(n, 4)} of ${ui.fmtNum(g.free, 4)} ${g.unit}`, n > ui.toNum(g.free) ? 'bad' : '');
    }
    q.input.addEventListener('input', upd);
    form.addEventListener('submit', async (ev) => {
      ev.preventDefault(); ui.clearFormError(form);
      const g = picker.get(); if (!g) { ui.formError(form, { status: 422, message: 'Choose the zone, item and lot first.', fields: {} }); return; }
      let bad = false; const p = L.qtyProblem(q.get(), { whole: g.whole, max: ui.toNum(g.free) }); q.setError(p); if (p) bad = true;
      if (!to.get()) { to.setError('Choose where the stock goes'); bad = true; } else to.setError('');
      if (to.get() === 'SCRAP_YARD' && note.get().trim().length < 5) { note.setError('Say why it goes to scrap (at least 5 letters)'); bad = true; } else note.setError('');
      if (bad) { form.querySelector('.has-error input, .has-error select, .has-error textarea')?.focus(); return; }
      const body = { ...withLoc(), item_type: g.item_type, item_id: g.item_id, ...(g.lot_id ? { lot_id: g.lot_id } : {}), from_zone: g.zone, to_zone: to.get(), qty: q.get().trim(), ...(note.get().trim() ? { note: note.get().trim() } : {}) };
      const r = await L.submitDocument({ flow, form, button: submit, path: '/api/warehouse/transfers', body, title: 'Review move', confirmText: 'Move stock', danger: to.get() === 'SCRAP_YARD' });
      if (r) { ui.toast.ok(`Moved ${ui.fmtNum(r.qty, 4)} of ${r.item} to ${L.zoneLabel(r.to_zone)}`); home(); }
    });
    formView('Move between zones', form, submit, 'Raw materials can move between the raw store and the blow floor. Anything can be moved to the scrap yard with a reason.');
  }

  /* ------------------------------------------------------------------------------------------ write-off */
  function voucherDoc(w) {
    return el('div.stack.tight', {}, L.kv([['Voucher', w.wo_no], ['Godown', w.location_name], ['Item', `${w.item_name}${w.item_code ? ` (${w.item_code})` : ''}`], ['Taken from', L.zoneLabel(w.zone)], ['Lot / batch', w.lot_label], ['Quantity', `${ui.fmtNum(w.qty, 4)} ${w.unit || ''}`],
      ['Reason', WO_LABEL[w.reason_code] || w.reason_code], ['Explanation', w.note], ['Result', w.disposed ? 'Removed from the books (disposed)' : 'Moved to the scrap yard'], ['Written off by', w.created_by_name], ['Date', ui.fmtDateTime(w.created_at)]]),
    L.signatureBlock(['Store keeper', 'Approved by', 'Witness']));
  }
  const showVoucher = (w) => L.showDocument({ title: 'Write-off voucher', docNo: w.wo_no, subtitle: w.location_name, build: () => voucherDoc(w) });
  async function openVoucher(id) { try { showVoucher(await api.get(`/api/warehouse/write-offs/${id}`)); } catch (e) { ui.toastError(e); } }

  function writeOffForm() {
    const picker = L.stockPicker({ locationId: loc, zones: ['RM_STORE', 'BLOW_FLOOR', 'FG_READY', 'SCRAP_YARD', 'JAR_EMPTY', 'JAR_CLEAN'], mode: 'existing', onChange: upd });
    const q = L.unitInput({ name: 'qty', label: 'How much is destroyed?', unit: '', required: true });
    const code = ui.field({ label: 'Reason', name: 'reason_code', type: 'select', required: true, options: [{ value: '', label: 'Choose a reason...' }, ...L.optionsOf(L.WO_REASONS)] });
    const note = ui.field({ label: 'Explain what happened', name: 'note', type: 'textarea', required: true, rows: 2, maxlength: 500, help: 'At least 5 letters. This goes on the voucher.' });
    const disp = el('input', { type: 'checkbox', name: 'dispose' }); const dispRow = el('label.check', {}, disp, el('span', {}, 'Remove it from the books completely', el('span.sub', { text: 'Tick when the goods are thrown away. Otherwise they stay in the scrap yard until disposed.' })));
    const warn = el('div');
    const form = el('form.stack', { id: 'wo-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }), godownLine(), picker.el, warn, el('div.card.stack', {}, q.el, code.el, note.el, dispRow));
    const submit = L.btn('Review write-off', 'trash', { kind: 'danger', type: 'submit', form: 'wo-form' });
    const flow = L.makeFlow();
    function upd() {
      const g = picker.get(); const unitEl = q.field.el.querySelector('.unit'); unitEl.textContent = g ? g.unit : ' '; ui.clear(warn);
      if (g && g.zone === 'SCRAP_YARD') { disp.checked = true; disp.disabled = true; } else disp.disabled = false;
      if (g && ui.toNum(g.reserved) > 0) warn.append(L.banner('warn', 'lock', el('b', { text: `${ui.fmtNum(g.reserved, 0)} crates of this batch are held for loading trips. `}), `Only ${ui.fmtNum(g.free, 0)} are free to write off.`));
      const n = L.dec(q.get());
      if (!g || q.get() === '' || n === null) { q.echo(g ? `Free: ${ui.fmtQty(g.free, g.unit, 4)}` : ''); return; }
      q.echo(n > ui.toNum(g.free) ? `More than the ${ui.fmtNum(g.free, 4)} that is free` : `Writing off ${ui.fmtNum(n, 4)} of ${ui.fmtNum(g.free, 4)} ${g.unit}`, n > ui.toNum(g.free) ? 'bad' : 'warn');
    }
    q.input.addEventListener('input', upd);
    form.addEventListener('submit', async (ev) => {
      ev.preventDefault(); ui.clearFormError(form);
      const g = picker.get(); if (!g) { ui.formError(form, { status: 422, message: 'Choose the zone, item and lot first.', fields: {} }); return; }
      let bad = false; const p = L.qtyProblem(q.get(), { whole: g.whole, max: ui.toNum(g.free) }); q.setError(p); if (p) bad = true;
      if (!code.get()) { code.setError('Choose a reason'); bad = true; } else code.setError('');
      if (note.get().trim().length < 5) { note.setError('Please explain (at least 5 letters)'); bad = true; } else note.setError('');
      if (bad) { form.querySelector('.has-error input, .has-error select, .has-error textarea')?.focus(); return; }
      const body = { ...withLoc(), item_type: g.item_type, item_id: g.item_id, zone: g.zone, ...(g.lot_id ? { lot_id: g.lot_id } : {}), qty: q.get().trim(), reason_code: code.get(), note: note.get().trim(), ...(disp.checked ? { dispose: true } : {}) };
      const r = await L.submitDocument({ flow, form, button: submit, path: '/api/warehouse/write-offs', body, title: 'Review write-off', confirmText: 'Write off - cannot be undone', danger: true, summary: 'Written-off stock cannot be brought back. Check the item, lot and quantity.' });
      if (r) { ui.toast.ok(`Written off - ${r.wo_no}`); home(); showVoucher(r); }
    });
    formView('Write off / destroy stock', form, submit, 'Use this for broken, expired, contaminated, spilled or lost goods. A voucher is created that you can print.');
  }

  /* ------------------------------------------------------------------------------------------ physical count */
  function newCount() {
    const zone = ui.field({ label: 'Which zone are you counting?', name: 'zone', type: 'select', required: true, options: [{ value: '', label: 'Choose a zone...' }, ...['RM_STORE', 'BLOW_FLOOR', 'FG_READY', 'SCRAP_YARD', 'JAR_EMPTY', 'JAR_CLEAN'].map((z) => ({ value: z, label: L.zoneLabel(z) }))], help: 'A count sheet lists everything the system thinks is in the zone right now.' });
    const note = ui.field({ label: 'Note', name: 'note', type: 'text', optional: true, maxlength: 500 });
    const form = el('form.stack', { id: 'cnt-new', novalidate: true }, el('div.form-error', { role: 'alert' }), godownLine(), el('div.card.stack', {}, zone.el, note.el));
    const submit = L.btn('Create count sheet', 'check', { kind: 'primary', type: 'submit', form: 'cnt-new' });
    const flow = L.makeFlow();
    form.addEventListener('submit', async (ev) => {
      ev.preventDefault(); if (!zone.get()) { zone.setError('Choose the zone'); return; } zone.setError('');
      const r = await L.submitPlain({ flow, form, button: submit, path: '/api/warehouse/counts', body: { ...withLoc(), zone: zone.get(), ...(note.get().trim() ? { note: note.get().trim() } : {}) } });
      if (r) { ui.toast.ok(`Created ${r.cnt_no}`); openCount(r.id, r); }
    });
    formView('New stock count', form, submit);
  }

  const countDoc = (c) => el('div.stack.tight', {}, L.kv([['Count sheet', c.cnt_no], ['Godown', c.location_name], ['Zone', L.zoneLabel(c.zone)], ['Date', ui.fmtDate(c.created_at)], ['Counted by', '']]),
    L.docTable([{ h: 'Item', v: (l) => l.name }, { h: 'Lot', v: (l) => l.lot_label || '' }, { h: 'System qty', num: true, v: (l) => `${ui.fmtNum(l.expected_qty, 4)} ${l.unit}` }, { h: 'Counted', num: true, v: (l) => (c.status === 'DRAFT' ? '' : l.counted_qty === null ? '-' : ui.fmtNum(l.counted_qty, 4)) }, { h: 'Note', v: (l) => l.note || '' }], c.lines),
    L.signatureBlock(['Counted by', 'Checked by', 'Approved by']));

  async function openCount(id, pre) {
    view = 'count'; L.toTop(); ui.mount(host, ui.skeletonCards(2));
    let c; try { c = pre || await api.get(`/api/warehouse/counts/${id}`); } catch (e) { ui.mount(host, L.backBar('Actions', home), ui.errorState(e, () => openCount(id))); return; }
    const pct = Number(L.settings().large_adjust_pct ?? 20);
    const printBtn = L.btn('Print sheet', 'print', { kind: 'secondary', sm: true, onClick: () => L.showDocument({ title: 'Stock count sheet', docNo: c.cnt_no, subtitle: `${L.zoneLabel(c.zone)} - ${c.location_name}`, build: () => countDoc(c) }) });
    if (c.status !== 'DRAFT') {
      ui.mount(host, L.backBar('Actions', home, printBtn), el('h2', { text: c.cnt_no }), el('div.chip-row', {}, ui.badge(c.status === 'POSTED' ? 'DONE' : 'CANCELLED', c.status === 'POSTED' ? 'Posted' : 'Cancelled'), el('span.chip-s', { text: L.zoneLabel(c.zone) }), el('span.chip-s', { text: c.location_name })),
        ui.cardsTable({ columns: [{ label: 'Item', cls: 'k', render: (l) => [el('b', { text: l.name }), el('small', { text: l.lot_label || '' })] }, { label: 'System', cls: 'num', render: (l) => `${ui.fmtNum(l.expected_qty, 4)} ${l.unit}` }, { label: 'Counted', cls: 'num', render: (l) => (l.counted_qty === null ? 'not counted' : ui.fmtNum(l.counted_qty, 4)) },
          { label: 'Difference', cls: 'num', render: (l) => (l.variance === null || l.variance === undefined ? '-' : el('b', { class: ui.toNum(l.variance) === 0 ? '' : ui.toNum(l.variance) < 0 ? 'delta-neg' : 'delta-pos', text: `${ui.toNum(l.variance) > 0 ? '+' : ''}${ui.fmtNum(l.variance, 4)}` })) }, { label: 'Note', cls: 'w', render: (l) => l.note }], rows: c.lines }));
      return;
    }
    const form = el('form', { id: 'count-form', novalidate: true, autocomplete: 'off', class: 'stack' }, el('div.form-error', { role: 'alert' }));
    const rows = c.lines.map((l) => {
      const whole = l.item_type === 'PRODUCT';
      const inp = el('input.input', { type: 'text', inputmode: whole ? 'numeric' : 'decimal', name: 'counted', autocomplete: 'off', 'aria-label': `Counted quantity of ${l.name}`, placeholder: 'Counted', value: l.counted_qty !== null && l.counted_qty !== undefined ? String(ui.fmtNum(l.counted_qty, 4)).replace(/,/g, '') : '' });
      const same = el('button.btn.secondary.sm', { type: 'button', on: { click: () => { inp.value = String(ui.fmtNum(l.expected_qty, 4)).replace(/,/g, ''); inp.dispatchEvent(new Event('input', { bubbles: true })); } } }, 'Same as system');
      const nt = el('input.input', { type: 'text', name: 'line_note', maxlength: 300, 'aria-label': `Note for ${l.name}`, placeholder: 'Why is it different?', value: l.note || '' });
      const ntWrap = el('div.field', { hidden: true }, el('label', { text: 'Note for the difference' }), nt, el('p.error'));
      const chipHost = el('div.chip-row');
      const card = el('div.line-card.count-line', { dataset: { name: l.name.toLowerCase() } }, el('div.row.between', { style: { flexWrap: 'nowrap', alignItems: 'flex-start' } }, el('div', {}, el('b', { text: l.name }), el('div.small.muted', { text: l.lot_label || '' })), el('div.right', {}, el('div.small.muted', { text: 'System' }), el('b', { text: `${ui.fmtNum(l.expected_qty, 4)} ${l.unit}` }))),
        el('div.count-input', {}, inp, same), chipHost, ntWrap);
      const row = { l, inp, nt, ntWrap, card, whole, problem: '', large: false,
        calc() {
          ui.clear(chipHost); row.problem = ''; row.large = false; const t = inp.value.trim();
          if (t === '') { ntWrap.hidden = nt.value === ''; return; }
          const p = L.qtyProblem(t, { whole, allowZero: true }); if (p) { row.problem = p; chipHost.append(el('span.chip-s.bad', {}, icon('alert', 'sm'), p)); ntWrap.hidden = true; return; }
          const v = Number(t) - ui.toNum(l.expected_qty);
          if (v === 0) { chipHost.append(el('span.chip-s.ok', {}, icon('check-circle', 'sm'), 'Matches the system')); ntWrap.hidden = nt.value === ''; return; }
          const e = ui.toNum(l.expected_qty); row.large = e <= 0 || Math.abs(v) * 100 > e * pct;
          chipHost.append(el('span.chip-s', { class: row.large ? 'bad' : 'warn' }, icon('alert', 'sm'), `${v > 0 ? '+' : ''}${ui.fmtNum(v, 4)}${e > 0 ? ` (${ui.fmtNum((v / e) * 100, 1)} %)` : ''}`), row.large ? el('span.small', { text: 'Large difference - re-count, then add a note.' }) : null);
          ntWrap.hidden = false; ntWrap.querySelector('label').textContent = row.large ? 'Note for the difference (required)' : 'Note for the difference';
        } };
      inp.addEventListener('input', () => { row.calc(); progress(); }); nt.addEventListener('input', () => { ntWrap.classList.remove('has-error'); });
      row.calc(); return row;
    });
    const progBar = el('div.count-progress'); const progText = el('span.small');
    function progress() { const n = rows.filter((r) => r.inp.value.trim() !== '').length; progText.textContent = `${n} of ${rows.length} counted`; ui.clear(progBar); progBar.append(ui.progress(rows.length ? (n / rows.length) * 100 : 0, 'ok')); }
    const filter = el('input.input', { type: 'search', placeholder: 'Find an item on this sheet', 'aria-label': 'Find an item', autocomplete: 'off' });
    filter.addEventListener('input', () => { const t = filter.value.trim().toLowerCase(); rows.forEach((r) => { r.card.hidden = t !== '' && !r.card.dataset.name.includes(t); }); });
    rows.forEach((r) => form.append(r.card)); progress();
    const draft = L.stateDraft({ page: PAGE, id: `count-${id}`, form, get: () => Object.fromEntries(rows.map((r) => [r.l.line_id, [r.inp.value, r.nt.value]])), set: (s) => { rows.forEach((r) => { const v = s[r.l.line_id]; if (v) { r.inp.value = v[0]; r.nt.value = v[1]; r.calc(); } }); progress(); } });
    draft.restore();
    const saveBtn = L.btn('Save progress', 'save', { kind: 'secondary' }); const postBtn = L.btn('Post counts', 'check', { kind: 'primary', cls: 'xl' });
    const flowSave = L.makeFlow(); const flowPost = L.makeFlow();
    const gather = () => {
      let bad = false; const lines = [];
      rows.forEach((r) => { r.ntWrap.classList.remove('has-error'); const t = r.inp.value.trim(); if (t === '') return;
        if (r.problem) { bad = true; r.inp.focus(); return; }
        lines.push({ line_id: r.l.line_id, counted_qty: t, ...(r.nt.value.trim() ? { note: r.nt.value.trim() } : {}), _large: r.large, _row: r }); });
      return { bad, lines };
    };
    async function save(quiet) {
      const { bad, lines } = gather(); if (bad) { ui.formError(form, { status: 422, message: 'Fix the numbers marked in red first.', fields: {} }); return false; }
      if (!lines.length) { if (!quiet) ui.toast.warn('Nothing counted yet.'); return true; }
      const r = await L.submitPlain({ flow: flowSave, form, button: quiet ? null : saveBtn, path: `/api/warehouse/counts/${id}/lines`, method: 'put', body: { lines: lines.map(({ line_id, counted_qty, note }) => ({ line_id, counted_qty, ...(note ? { note } : {}) })) }, onStale: () => openCount(id) });
      if (r) { c = r; draft.clear(); if (!quiet) ui.toast.ok('Progress saved'); return true; } return false;
    }
    saveBtn.addEventListener('click', () => save(false));
    postBtn.addEventListener('click', async () => {
      ui.clearFormError(form);
      const g = gather(); if (g.bad) { ui.formError(form, { status: 422, message: 'Fix the numbers marked in red first.', fields: {} }); return; }
      if (!g.lines.length) { ui.formError(form, { status: 422, message: 'Count at least one line before posting.', fields: {} }); return; }
      const missing = g.lines.filter((x) => x._large && !x.note); if (missing.length) { missing.forEach((m) => { m._row.ntWrap.hidden = false; m._row.ntWrap.classList.add('has-error'); m._row.ntWrap.querySelector('.error').textContent = 'A note is needed for a large difference'; }); missing[0]._row.nt.focus(); missing[0]._row.card.scrollIntoView({ block: 'center', behavior: 'smooth' }); return; }
      if (!(await save(true))) return;
      const r = await L.submitDocument({ flow: flowPost, form, button: postBtn, path: `/api/warehouse/counts/${id}/post`, body: {}, title: `Post ${c.cnt_no}`, confirmText: 'Post - cannot be undone', danger: true, summary: 'Posting changes the stock to what you counted. Lines you did not count stay as they are.', onStale: () => openCount(id) });
      if (r) { draft.clear(); ui.toast.ok(`${r.cnt_no} posted`); openCount(id, r); }
    });
    const cancelBtn = L.btn('Cancel this count', 'ban', { kind: 'danger outline', sm: true, onClick: async () => {
      if (!(await ui.confirmDialog({ title: `Cancel ${c.cnt_no}?`, message: 'Nothing changes in stock. The sheet is closed.', confirmText: 'Cancel the count', cancelText: 'Keep counting', danger: true }))) return;
      try { await api.busy(cancelBtn, () => api.action().post(`/api/warehouse/counts/${id}/cancel`, {})); draft.clear(); ui.toast.ok('Count cancelled'); home(); } catch (e) { L.errorDialog(e); }
    } });
    ui.mount(host, L.backBar('Actions', home, printBtn, cancelBtn), el('h2', { text: c.cnt_no }), el('div.chip-row', {}, el('span.chip-s', { text: L.zoneLabel(c.zone) }), el('span.chip-s', { text: c.location_name }), ui.badge('DRAFT', 'In progress')),
      el('p.section-sub', { text: 'Count what is really on the shelf and type it next to each line. Save any time - you can finish later. Differences become stock adjustments only when you post.' }),
      el('div.card.stack.tight', {}, filter, el('div.row.between', {}, progText), progBar), form,
      el('div.action-bar', {}, saveBtn, postBtn));
  }
  ctx.refreshers['actions:open'] = () => {};
  home();
}
