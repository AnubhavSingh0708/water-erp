// Batches tab: list + summary charts, detail/trace drawer, new batch (BOM check, waste, quality gate, dry-run review).
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { ERP } from '/js/shell.js';
import * as L from '/js/pages/warehouse-lib.js';

const { el, icon } = ui;
const QC_FILTERS = [{ value: '', label: 'All' }, { value: 'QUARANTINE', label: 'In QC' }, { value: 'APPROVED', label: 'Approved' }, { value: 'REJECTED', label: 'Rejected' }];
const BASIS = { PER_CRATE: 'per crate', PER_1000L: 'per 1,000 L of water' };

export function mountBatches(panel, ctx) {
  const host = el('div.stack'); panel.append(host);
  const st = { qc: '', q: '', offset: 0 };
  let listHost = null; let inForm = false; let formRefresh = null;

  /* ------------------------------------------------------------------------------------------ list */
  function showList() {
    inForm = false; formRefresh = null; L.toTop();
    const seg = ui.segmented({ options: QC_FILTERS, value: st.qc, label: 'QC status', onChange: (v) => { st.qc = v; st.offset = 0; load(); } });
    const search = el('input.input', { type: 'search', placeholder: 'Search batch no or product', 'aria-label': 'Search batches', maxlength: 100, value: st.q });
    search.addEventListener('input', ui.debounce(() => { st.q = search.value.trim(); st.offset = 0; load(); }, 350));
    const csv = L.btn('CSV', 'download', { kind: 'secondary', sm: true, onClick: () => L.guarded(csv, () => api.download('/api/production/batches', { query: { format: 'csv', qc_status: st.qc, q: st.q } })) });
    listHost = el('div');
    const sumHost = el('div');
    const sum = ui.collapsible('Last 14 days - production summary', sumHost, { icon: 'dashboard' });
    let sumLoaded = false;
    sum.addEventListener('toggle', () => { if (sum.open && !sumLoaded) { sumLoaded = true; loadSummary(sumHost); } });
    ui.mount(host, el('div.page-head', {}, el('div', {}, el('p.sub', { text: 'A batch turns bottles, caps, labels and minerals into crates waiting for Lab QC.' })), el('div.actions', {}, L.btn('New batch', 'plus', { onClick: () => showForm() }))),
      sum, el('div.toolbar', {}, el('div.searchbar', {}, icon('search'), search), el('div.toolbar-end', {}, csv)), el('div.seg-row', {}, seg.el), listHost);
    load();
  }
  async function loadSummary(h) {
    ui.mount(h, ui.skeletonCards(1));
    try {
      const to = ui.todayPlant(); const d = await api.get('/api/production/summary', { from: ui.addDays(to, -13), to, ...L.readParam() });
      const days = (d.days || []).map((x) => ({ label: ui.fmtDate(x.date).slice(0, -5), value: x.crates, sub: `${x.batches} ${x.batches === 1 ? 'batch' : 'batches'}`, tone: '' }));
      const scrapWarn = Number(L.settings().scrap_warn_pct ?? 5);
      const scrap = (d.days || []).filter((x) => ui.toNum(x.blow_good) + ui.toNum(x.blow_scrap) > 0).map((x) => ({ label: ui.fmtDate(x.date).slice(0, -5), value: x.scrap_pct, sub: `${ui.fmtNum(x.blow_scrap, 0)} scrap`, tone: ui.toNum(x.scrap_pct) > scrapWarn ? 'warn' : 'ok' }));
      ui.mount(h, el('div.stack', {},
        days.length ? el('div', {}, el('h3', { text: 'Crates made per day' }), L.bars(days)) : el('p.muted', { text: 'No batches in this period.' }),
        (d.by_product || []).length ? el('div', {}, el('h3', { text: 'By product' }), L.bars(d.by_product.map((p) => ({ label: p.name, value: p.crates, sub: `${ui.fmtNum(p.bottles, 0)} bottles` })))) : null,
        scrap.length ? el('div', {}, el('h3', { text: 'Blow-molding scrap % per day' }), L.bars(scrap, { max: Math.max(10, ...scrap.map((s) => ui.toNum(s.value))), fmt: (v) => `${ui.fmtNum(v, 1)} %` })) : null));
    } catch (e) { ui.mount(h, ui.errorState(e, () => loadSummary(h))); }
  }
  async function load() {
    if (inForm || !listHost) return;
    await L.refreshInto(listHost, async () => {
      const d = await api.get('/api/production/batches', { limit: 15, offset: st.offset, qc_status: st.qc, q: st.q, ...L.readParam() });
      return el('div.stack.tight', {}, ui.cardsTable({
        columns: [
          { label: 'Batch', cls: 'k', render: (b) => [el('b.nowrap', { text: b.batch_no }), el('small', { text: b.product_name })] },
          { label: 'QC', cls: 's', render: (b) => ui.badge(b.qc_status, b.qc_status === 'QUARANTINE' ? 'Waiting for QC' : undefined) },
          { label: 'Made', cls: 'num', render: (b) => L.cratesCell(b.crates, b.crates_per_pallet, b.kind === 'RETURNABLE' ? 'jars' : 'crates') },
          { label: 'Date', render: (b) => el('span.nowrap', { text: `${ui.fmtDate(b.produced_on)} - ${b.shift.toLowerCase()}` }) }, { label: 'Expiry', hideMobile: true, render: (b) => el('span.nowrap', { text: ui.fmtDate(b.expiry_date) }) },
          { label: 'Godown', hideMobile: true, render: (b) => b.location_name },
        ], rows: d.items, onRow: (b) => openBatch(b.id),
        emptyState: { icon: 'package', title: st.qc || st.q ? 'No batches match' : 'No batches yet', text: 'Press "New batch" when a filling run is done. It then waits for Lab QC to approve it.', action: st.qc || st.q ? null : { label: 'New batch', icon: 'plus', onClick: () => showForm() } },
      }), d.total > 15 ? ui.pager({ total: d.total, limit: 15, offset: st.offset, onPage: (o) => { st.offset = o; load(); } }) : null);
    });
  }

  /* ------------------------------------------------------------------------------------------ detail + trace */
  function matTable(d) {
    return ui.cardsTable({
      columns: [
        { label: 'Material', cls: 'k', render: (m) => [el('b', { text: m.name }), el('small', { text: `${L.zoneLabel(m.zone)}${m.basis ? ` - ${BASIS[m.basis] || m.basis}` : ''}` })] },
        { label: 'Used', cls: 'num', render: (m) => L.qty(m.qty, m.unit) },
        { label: 'Of which waste', cls: 'num', render: (m) => (ui.toNum(m.waste_qty) > 0 ? el('span.chip-s.warn', { text: L.qty(m.waste_qty, m.unit) }) : '-') },
      ], rows: d.materials || [], emptyState: { icon: 'box', title: d.product && d.product.kind === 'RETURNABLE' ? 'Only clean containers and consumables used' : 'No materials recorded' } });
  }
  async function openBatch(idOrObj) {
    const body = el('div.stack', {}, ui.skeletonCards(2));
    const traceBtn = L.btn('Recall / trace view', 'route', { kind: 'secondary', onClick: () => showTrace() });
    const dlg = ui.openDialog({ title: 'Batch', body, wide: true, actions: [traceBtn] });
    let d = null;
    async function show() {
      try {
        d = typeof idOrObj === 'object' && idOrObj.materials ? idOrObj : await api.get(`/api/production/batches/${typeof idOrObj === 'object' ? idOrObj.id : idOrObj}`);
        idOrObj = d.id; dlg.titleEl.textContent = d.batch_no;
        const p = d.product || {}; const ret = p.kind === 'RETURNABLE';
        ui.mount(body, el('div.stack', {},
          d.qc_status === 'REJECTED' ? L.banner('err', 'x-circle', el('b', { text: 'Rejected by QC. ' }), d.qc_notes || '') : d.qc_status === 'APPROVED' ? L.banner('ok', 'check-circle', el('b', { text: 'Approved by QC' }), ` ${d.decided_by_name ? `(${d.decided_by_name}) ` : ''}- ${ui.fmtDateTime(d.decided_at)}`) : L.banner('warn', 'clock', el('b', { text: 'Waiting for Lab QC. ' }), 'Stock is held in quarantine until it is approved.'),
          L.kv([['Product', `${p.name || d.product_name}${ret ? ' (returnable)' : ''}`], ['Quantity', L.cratesCell(d.crates, p.crates_per_pallet, ret ? 'jars' : 'crates')], ['Water filled', d.litres ? `${ui.fmtNum(d.litres, 1)} litres` : '-'],
            ['Made on', `${ui.fmtDate(d.produced_on)} - ${d.shift.toLowerCase()} shift`], ['Expiry', ui.fmtDate(d.expiry_date)], ['Godown', d.location_name], ['Entered by', d.created_by_name], ['Note', d.notes]]),
          ret ? el('p.small.muted', { text: `This batch used ${ui.fmtNum(d.crates, 0)} clean containers from the washed stock - no preforms.` }) : null,
          d.stock_by_location && d.stock_by_location.length ? el('div.chip-row', {}, d.stock_by_location.map((s) => el('span.chip-s', { text: `${L.zoneLabel(s.zone)}: ${ui.fmtNum(s.qty, 0)}` }))) : null,
          el('h3', { text: 'Materials used' }), matTable(d),
          (d.waste || []).length ? el('div.stack.tight', {}, el('h3', { text: 'Waste recorded' }), el('ul.plain', {}, d.waste.map((w) => el('li', {}, el('b', { text: w.name }), ` ${ui.fmtNum(w.qty, 4)} - ${String(w.reason || '').replace(/^.*waste: /, '')}`)))) : null,
          el('h3', { text: 'Lab tests' }),
          (d.tests || []).length ? ui.cardsTable({ columns: [
            { label: 'Tested', cls: 'k', render: (t) => [el('b', { text: ui.fmtDateTime(t.tested_at) }), el('small', { text: t.tested_by_name || '' })] }, { label: 'Result', cls: 's', render: (t) => ui.badge(t.result) },
            { label: 'TDS', cls: 'num', render: (t) => (t.tds_ppm ? ui.fmtNum(t.tds_ppm, 3) : '-') }, { label: 'pH', cls: 'num', render: (t) => (t.ph ? ui.fmtNum(t.ph, 3) : '-') },
            { label: 'Remarks', cls: 'w', render: (t) => t.remarks },
          ], rows: d.tests }) : el('p.muted', { text: 'No tests recorded yet.' }),
          el('div', { id: 'trace-slot' })));
      } catch (e) { ui.mount(body, ui.errorState(e, () => { show(); })); }
    }
    async function showTrace() {
      const slot = body.querySelector('#trace-slot'); if (!slot) return;
      ui.mount(slot, ui.skeletonCards(1));
      try {
        const t = await api.get(`/api/production/batches/${d.id}/trace`); const tot = t.totals || {};
        ui.mount(slot, el('div.stack', {}, el('h3', { text: 'Where did this batch go? (recall view)' }),
          el('div.stat-grid', {}, [['Made', tot.produced], ['Loaded on trucks', tot.loaded], ['Delivered', tot.delivered], ['Returned', tot.returned], ['Damaged', tot.damaged], ['Sold direct', tot.direct_dispatched], ['On open trips', tot.on_open_trips], ['Still in stock', tot.still_in_stock]].map(([l, v]) => el('div.stat', {}, el('div.s-label', { text: l }), el('div.s-value', { text: ui.fmtNum(v, 0) })))),
          el('h3', { text: 'Raw material lots used' }),
          ui.cardsTable({ columns: [{ label: 'Material', cls: 'k', render: (m) => [el('b', { text: m.name }), el('small', { text: m.kind === 'WASTE' ? 'waste' : 'consumed' })] }, { label: 'Qty', cls: 'num', render: (m) => ui.fmtNum(m.qty, 4) },
            { label: 'GRN', render: (m) => m.grn_no || '-' }, { label: 'Supplier', render: (m) => m.supplier_name || '-' }, { label: 'Supplier lot', hideMobile: true, render: (m) => m.lot_no || '-' }], rows: t.materials || [], emptyState: { icon: 'box', title: 'No lot information' } }),
          el('h3', { text: 'Trips and sales' }),
          ui.cardsTable({ columns: [{ label: 'Trip', cls: 'k', render: (x) => [el('b.nowrap', { text: x.trip_no }), el('small', { text: `${x.vehicle_reg || ''} ${x.route_name || ''}` })] }, { label: 'Status', cls: 's', render: (x) => ui.badge(x.status) },
            { label: 'Crates', cls: 'num', render: (x) => ui.fmtNum(x.crates, 0) }, { label: 'Delivered', cls: 'num', render: (x) => ui.fmtNum(x.delivered, 0) }, { label: 'Gate pass', hideMobile: true, render: (x) => x.gate_pass_no || '-' }],
          rows: t.trips || [], emptyState: { icon: 'truck', title: 'Not loaded on any trip yet' } }),
          (t.dispatches || []).length ? ui.cardsTable({ columns: [{ label: 'Dispatch note', cls: 'k', render: (x) => [el('b.nowrap', { text: x.dn_no }), el('small', { text: x.party })] }, { label: 'Crates', cls: 'num', render: (x) => ui.fmtNum(x.crates, 0) }, { label: 'Status', cls: 's', render: (x) => ui.badge(x.status) }], rows: t.dispatches }) : null));
        slot.scrollIntoView({ behavior: 'smooth', block: 'start' });
      } catch (e) { ui.mount(slot, ui.errorState(e, showTrace)); }
    }
    show();
  }

  /* ------------------------------------------------------------------------------------------ new batch */
  async function showForm() {
    inForm = true; L.toTop(); ui.mount(host, ui.skeletonCards(2));
    let products, wstat;
    try { [products, wstat] = await Promise.all([L.refProducts(), api.get('/api/production/water-logs/status').catch(() => null)]); } catch (e) { ui.mount(host, ui.errorState(e, showForm)); return; }
    products = products.filter((p) => p.is_active);
    const site = ctx.site();
    const prod = ui.field({ label: 'Product', name: 'product_id', type: 'select', required: true, options: [{ value: '', label: 'Choose product...' }, ...products.map((p) => ({ value: p.id, label: `${p.name}${p.kind === 'RETURNABLE' ? ' (returnable)' : ''}` }))] });
    const shift = ui.field({ label: 'Shift', name: 'shift', type: 'select', options: L.optionsOf(L.SHIFTS), value: 'DAY' });
    const hour = Number((ui.fmtTime(new Date().toISOString()) || '12:00').split(':')[0]); if (hour >= 19 || hour < 6) shift.set('NIGHT');
    const date = ui.field({ label: 'Production date', name: 'produced_on', type: 'date', value: ui.todayPlant() }); date.input.max = ui.todayPlant(); date.input.min = L.todayMinus(30);
    const notes = ui.field({ label: 'Notes', name: 'notes', type: 'textarea', optional: true, rows: 2, maxlength: 500 });
    const info = el('div.stack.tight'); const qtyHolder = el('div'); const gateHost = el('div.stack.tight'); const bomHost = el('div.stack.tight');
    const wasteHost = el('div.lines'); const wasteRows = [];
    let qty = null; let cur = null; let bom = null; let seq = 0;

    const form = el('form.stack', { id: 'batch-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }), gateHost,
      el('div.card.stack', {}, el('div.form-grid', {}, prod.el, el('div', {}, qtyHolder), shift.el, date.el), info),
      el('div.card.stack', {}, el('div.card-head', {}, el('h2', {}, icon('box'), 'Materials needed')), bomHost),
      ui.collapsible('Record waste (optional)', el('div.stack', {}, el('p.section-sub', { text: 'Spilled, damaged or thrown away during this run? Enter it here so the stock stays true. It is taken from stock on top of the normal use.' }), wasteHost,
        L.btn('Add waste', 'plus', { kind: 'secondary', sm: true, onClick: () => addWaste() })), { icon: 'trash' }),
      el('div.card', {}, notes.el));
    const submit = L.btn('Check and create batch', 'check', { kind: 'primary', type: 'submit', form: 'batch-form' });
    const flow = L.makeFlow();

    function paintGate(s) {
      ui.clear(gateHost); if (!s) return;
      if (s.overdue || !s.last_logged_at) gateHost.append(L.banner('warn', 'clock', el('b', { text: 'No water reading in the last hour. ' }), 'You can continue, but log a reading first if you can.', el('div.banner-act', {}, el('button.btn.secondary.sm', { type: 'button', on: { click: () => ctx.goto('water') } }, icon('droplet', 'sm'), 'Log water reading'))));
      if (s.last_out_of_range) gateHost.append(L.banner('err', 'alert', el('b', { text: 'The latest water reading was out of range. ' }), 'You will have to confirm before this batch is created. Re-test the water first.'));
    }
    paintGate(wstat);

    function productChanged() {
      cur = products.find((p) => p.id === prod.get()) || null;
      const old = qty ? qty.raw : '';
      ui.mount(qtyHolder, (qty = ui.qtyInput({ name: 'crates', label: cur && cur.kind === 'RETURNABLE' ? 'Jars to fill' : 'Crates to make', cratesPerPallet: cur && cur.crates_per_pallet ? cur.crates_per_pallet : undefined, min: 1, max: 1000000, required: true, value: old, onInput: () => { paintInfo(); bomDebounced(); } })).el);
      paintInfo(); refreshBom();
    }
    function paintInfo() {
      ui.clear(info); if (!cur) return;
      const n = qty && qty.valid ? qty.value : null; const ret = cur.kind === 'RETURNABLE';
      const litres = n && cur.size_ml ? (n * cur.units_per_crate * cur.size_ml) / 1000 : null;
      info.append(el('div.chip-row', {}, el('span.chip-s', { text: ret ? 'Returnable jar' : `${cur.units_per_crate} bottles per crate` }), cur.size_ml ? el('span.chip-s', { text: `${ui.fmtNum(cur.size_ml, 0)} ml` }) : null, cur.crates_per_pallet ? el('span.chip-s', { text: `${cur.crates_per_pallet} crates per pallet` }) : null,
        n ? el('span.chip-s.info', { text: ret ? `Uses ${ui.fmtNum(n, 0)} clean containers - no preforms` : `${ui.fmtNum(n * cur.units_per_crate, 0)} bottles` }) : null, litres ? el('span.chip-s.info', { text: `${ui.fmtNum(litres, 1)} litres of water` }) : null),
      ret ? el('p.small.muted', { text: 'Washed (clean) containers are used up as they are filled. Wash more under Jars if the stock is short.' }) : el('p.small.muted', { text: 'Empty bottles come from the blow floor (see the Blow molding tab); caps, labels, cartons and minerals come from the raw store.' }));
      if (!cur.size_ml) info.append(L.banner('warn', 'alert', 'This product has no size set, so mineral dosing per 1,000 L cannot be calculated. Ask an administrator to fix the product.'));
    }
    const wasteTotals = () => { const m = new Map(); wasteRows.forEach((r) => { const q = L.dec(r.q.get()); if (r.mat.get() && q) m.set(r.mat.get(), (m.get(r.mat.get()) || 0) + q); }); return m; };
    function paintBom() {
      ui.clear(bomHost);
      if (!cur) { bomHost.append(el('p.muted', { text: 'Choose the product and how many crates to see what is needed and what is in stock.' })); return; }
      if (!qty || !qty.valid) { bomHost.append(el('p.muted', { text: 'Enter the number of crates.' })); return; }
      if (!bom) { bomHost.append(ui.skeletonCards(1)); return; }
      if (bom.error) { bomHost.append(L.banner('err', 'alert', bom.error)); return; }
      const w = wasteTotals();
      const lines = bom.lines.map((l) => { const extra = l.raw_material_id ? (w.get(l.raw_material_id) || 0) : 0; const need = ui.toNum(l.required) + extra; return { ...l, extra, need, short: ui.toNum(l.available) < need }; });
      const shorts = lines.filter((l) => l.short);
      bomHost.append(shorts.length ? L.banner('err', 'ban', el('b', { text: `Short on ${shorts.length} ${shorts.length === 1 ? 'item' : 'items'}: ` }), shorts.map((l) => l.name).join(', '), el('div.small', { style: { fontWeight: 500 }, text: 'Receive stock, make fewer crates, or ask the store. The batch cannot be created until everything is available.' }))
        : L.banner('ok', 'check-circle', el('b', { text: 'Everything is in stock ' }), `for ${ui.fmtNum(bom.crates, 0)} ${bom.kind === 'RETURNABLE' ? 'jars' : 'crates'}${bom.litres ? ` (${ui.fmtNum(bom.litres, 1)} L of water)` : ''}.`),
      ui.cardsTable({ columns: [
        { label: 'Material', cls: 'k', render: (l) => [el('b', { text: l.name }), el('small', { text: l.container ? 'washed jars in stock' : `${ui.fmtNum(l.qty_per, 4)} ${l.unit} ${BASIS[l.basis] || ''}` })] },
        { label: 'Status', cls: 's', render: (l) => (l.short ? ui.badge('OUT', `Short ${ui.fmtNum(l.need - ui.toNum(l.available), 4)}`) : ui.badge('OK', 'Enough')) },
        { label: 'Needed', cls: 'num', render: (l) => [L.qty(l.need, l.unit), l.extra ? el('small.block.muted', { text: `incl. ${ui.fmtNum(l.extra, 4)} waste` }) : null] },
        { label: 'In stock', cls: 'num', render: (l) => L.qty(l.available, l.unit) },
      ], rows: lines }));
    }
    async function refreshBom() {
      if (!cur || !qty || !qty.valid) { bom = null; paintBom(); fillWasteOptions(); return; }
      const my = ++seq; bom = null; paintBom();
      try { const r = await api.get('/api/production/bom-check', { product_id: cur.id, crates: qty.value, ...(site ? { location_id: site.id } : {}) }); if (my !== seq) return; bom = r; }
      catch (e) { if (my !== seq || e.redirecting) return; bom = { error: e.message || 'Could not check the materials' }; }
      paintBom(); fillWasteOptions();
    }
    const bomDebounced = ui.debounce(refreshBom, 450);
    formRefresh = async () => { try { paintGate(await api.get('/api/production/water-logs/status')); } catch { /* keep */ } if (cur && qty && qty.valid) await refreshBom(); };

    function addWaste(v = {}) {
      const mat = ui.field({ label: 'Material wasted', name: 'waste_material', type: 'select', options: [{ value: '', label: 'Choose material...' }] });
      const q = L.unitInput({ name: 'waste_qty', label: 'Quantity wasted', unit: '' });
      const reason = ui.field({ label: 'What happened?', name: 'waste_reason', type: 'text', maxlength: 200, placeholder: 'e.g. spilled while loading' });
      const row = { want: v.mat || '', mat, q, reason, card: null, get: () => ({ mat: mat.get(), qty: q.get(), reason: reason.get() }) };
      row.card = el('div.line-card', {}, el('div.line-head', {}, el('span', { text: 'Waste' }), el('button.btn.ghost.sm.icon', { type: 'button', 'aria-label': 'Remove waste line', on: { click: () => { wasteRows.splice(wasteRows.indexOf(row), 1); row.card.remove(); paintBom(); form.dispatchEvent(new Event('input', { bubbles: true })); } } }, icon('trash'))), mat.el, q.el, reason.el);
      mat.input.addEventListener('change', () => { row.want = mat.get(); const l = bom && bom.lines && bom.lines.find((x) => x.raw_material_id === mat.get()); q.field.el.querySelector('.unit').textContent = l ? l.unit : ' '; paintBom(); });
      q.input.addEventListener('input', paintBom);
      wasteRows.push(row); wasteHost.append(row.card); q.set(v.qty || ''); reason.set(v.reason || '');
      fillOptions(row); return row;
    }
    function fillOptions(row) {
      const lines = bom && bom.lines ? bom.lines.filter((l) => !l.container && l.raw_material_id && l.zone !== 'BLOW_FLOOR') : [];
      ui.clear(row.mat.input);
      row.mat.input.append(el('option', { value: '', text: lines.length ? 'Choose material...' : 'Enter crates first' }), ...lines.map((l) => el('option', { value: l.raw_material_id, text: l.name })));
      row.mat.input.value = lines.some((l) => l.raw_material_id === row.want) ? row.want : '';
      const l = lines.find((x) => x.raw_material_id === row.mat.get()); const u = row.q.field.el.querySelector('.unit'); if (u) u.textContent = l ? l.unit : ' ';
    }
    function fillWasteOptions() { wasteRows.forEach(fillOptions); }

    prod.input.addEventListener('change', productChanged);
    const getState = () => ({ product: prod.get(), crates: qty ? qty.raw : '', shift: shift.get(), date: date.get(), notes: notes.get(), waste: wasteRows.map((r) => r.get()) });
    const setState = (s) => { prod.set(s.product || ''); productChanged(); if (qty) qty.set(s.crates || ''); shift.set(s.shift || 'DAY'); date.set(s.date || ui.todayPlant()); notes.set(s.notes || ''); wasteRows.splice(0).forEach((r) => r.card.remove()); (s.waste || []).forEach((w) => addWaste({ mat: w.mat, qty: w.qty, reason: w.reason })); paintInfo(); refreshBom(); };
    const draft = L.stateDraft({ page: 'production', id: 'batch', form, get: getState, set: setState });
    productChanged(); draft.restore();

    form.addEventListener('submit', async (ev) => {
      ev.preventDefault(); ui.clearFormError(form);
      let bad = false;
      if (!cur) { prod.setError('Choose the product'); bad = true; } else prod.setError('');
      if (!qty || !qty.valid) { if (qty) ui.setFieldError(qty.el, qty.raw === '' ? 'Enter how many crates' : 'Enter a whole number of crates (1 or more)'); bad = true; } else ui.setFieldError(qty.el, '');
      const waste = [];
      const seen = new Set();
      wasteRows.forEach((r) => {
        r.mat.setError(''); r.q.setError(''); r.reason.setError('');
        const s = r.get(); if (s.mat === '' && s.qty === '' && s.reason === '') return;
        if (!s.mat) { r.mat.setError('Choose the material'); bad = true; } else if (seen.has(s.mat)) { r.mat.setError('Each material once - add the quantities together'); bad = true; } else seen.add(s.mat);
        const p = L.qtyProblem(s.qty); if (p) { r.q.setError(p); bad = true; }
        if (s.reason.trim().length < 3) { r.reason.setError('Say what happened (at least 3 letters)'); bad = true; }
        if (s.mat && !p && s.reason.trim().length >= 3) waste.push({ raw_material_id: s.mat, qty: s.qty.trim(), reason: s.reason.trim() });
      });
      if (bom && bom.lines && !bad) {
        const w = wasteTotals(); const shorts = bom.lines.filter((l) => ui.toNum(l.available) < ui.toNum(l.required) + (l.raw_material_id ? (w.get(l.raw_material_id) || 0) : 0));
        if (shorts.length) { ui.formError(form, { status: 409, code: 'INSUFFICIENT_STOCK', message: 'Not enough material for this batch.', details: { shortages: shorts.map((l) => ({ name: l.name, required: ui.toNum(l.required) + (l.raw_material_id ? (w.get(l.raw_material_id) || 0) : 0), available: l.available, zone: l.zone })) } }); const x = L.explain({ code: 'INSUFFICIENT_STOCK', details: { shortages: shorts.map((l) => ({ name: l.name, required: ui.toNum(l.required), available: l.available, zone: l.zone })) } }); form.querySelector('.form-error').append(x); bad = true; }
      }
      if (bad) { form.querySelector('.has-error input, .has-error select')?.focus(); return; }
      const body = { product_id: cur.id, crates: qty.value, shift: shift.get(), ...(site ? { location_id: site.id } : {}), ...(date.get() ? { produced_on: date.get() } : {}), ...(notes.get().trim() ? { notes: notes.get().trim() } : {}), ...(waste.length ? { waste } : {}) };
      const b = await L.submitDocument({ flow, form, button: submit, path: '/api/production/batches', body, title: 'Review batch', confirmText: 'Create batch',
        summary: `${ui.fmtNum(qty.value, 0)} ${cur.kind === 'RETURNABLE' ? 'jars' : 'crates'} of ${cur.name}. These materials are used now and the crates wait in QC quarantine.`,
        onStale: () => refreshBom() });
      if (b) {
        draft.clear(); ui.toast.ok(`Created ${b.batch_no} - waiting for QC`); (b.warnings || []).forEach((w) => ui.toast.warn(w));
        showList(); openBatch(b);
      }
    });
    ui.mount(host, L.backBar('Batches', () => showList()), el('h2', { text: 'New batch' }), form, L.actionBar(submit));
  }

  ctx.refreshers.batches = async () => { if (inForm) { if (formRefresh) await formRefresh(); } else await load(); };
  showList();
}
