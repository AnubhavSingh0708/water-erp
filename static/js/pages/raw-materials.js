// raw-materials.html - Stock | Reorder | Purchase orders | Receive (GRN) | Supplier returns.
import { boot, ERP } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import * as L from '/js/pages/warehouse-lib.js';
import { mountPo } from '/js/pages/raw-materials-po.js';
import { mountGrn } from '/js/pages/raw-materials-grn.js';

const { el, icon } = ui;
await boot({ page: 'raw-materials', title: 'Raw materials', godownAll: true });
const main = document.getElementById('main');

const refreshers = {};
const ctx = {
  refreshers,
  goto: (id, arg) => { tabs.select(id); if (arg && refreshers[`${id}:open`]) refreshers[`${id}:open`](arg); },
  badge: (id, n) => {
    const t = document.getElementById(`tab-${id}`); if (!t) return;
    t.querySelector('.count-pill')?.remove();
    if (n > 0) t.append(el('span.count-pill', { text: n > 99 ? '99+' : String(n), 'aria-label': `${n} open` }));
  },
};

/* ---------------------------------------------------------------------------------------------- Stock */
const REASON = { BELOW_MIN: ['warn', 'Below minimum'], BURN_RATE: ['warn', 'Will run out soon'], BOTH: ['bad', 'Low and running out'] };
const TXN = {
  GRN: 'Received (GRN)', GRN_VOID: 'GRN voided', RTV_OUT: 'Returned to supplier', BLOW_ISSUE: 'Issued to blow molding', BATCH_CONSUME: 'Used in batch', BATCH_WASTE: 'Batch waste',
  ADJUST: 'Adjustment', TRANSFER: 'Moved between zones', WRITE_OFF: 'Written off', DISPOSE: 'Disposed', GODOWN_TRANSFER_OUT: 'Sent to another godown', GODOWN_TRANSFER_IN: 'Received from godown',
  GODOWN_TRANSFER_CANCEL: 'Transfer cancelled', BLOW_OUTPUT: 'Blow output', BLOW_SCRAP: 'Blow scrap', TRANSIT_DAMAGE: 'Damaged in transit', TRANSFER_SHORT: 'Short in transit',
};

function mountStock(panel) {
  const st = { q: '', category: '', low: false };
  const search = el('input.input', { type: 'search', placeholder: 'Search material or code', 'aria-label': 'Search materials', autocomplete: 'off', maxlength: 100 });
  const cat = ui.field({ label: 'Category', name: 'category', type: 'select', options: [{ value: '', label: 'All categories' }, ...Object.entries(L.CATEGORY_LABEL).map(([v, l]) => ({ value: v, label: l }))] });
  const low = el('input', { type: 'checkbox', name: 'low' });
  const csv = L.btn('CSV', 'download', { kind: 'secondary', sm: true, onClick: () => L.guarded(csv, () => api.download('/api/raw-materials', { query: { format: 'csv', ...L.readParam(), category: st.category, q: st.q, low: st.low ? 1 : '' } })) });
  const host = el('div');
  panel.append(
    el('div.toolbar', {}, el('div.searchbar', {}, icon('search'), search), el('div.toolbar-end', {}, csv)),
    el('div.filters', {}, cat.el, el('label.check.bare', {}, low, el('span', { text: 'Only low / out of stock' }))),
    host);
  const run = ui.debounce(() => { st.q = search.value.trim(); load(); }, 350);
  search.addEventListener('input', run);
  cat.input.addEventListener('change', () => { st.category = cat.get(); load(); });
  low.addEventListener('change', () => { st.low = low.checked; load(); });

  async function load() {
    await L.refreshInto(host, async () => {
      const rows = await api.get('/api/raw-materials', { ...L.readParam(), category: st.category, q: st.q, low: st.low ? 1 : '' });
      return ui.cardsTable({
        columns: [
          { label: 'Material', cls: 'k', render: (m) => [el('b', { text: m.name }), el('small', { text: `${m.code} - ${L.CATEGORY_LABEL[m.category] || m.category}` })] },
          { label: 'Status', cls: 's', render: (m) => [m.category === 'EMPTY_BOTTLE' ? ui.badge('INFO', 'Made in-house', 'info') : ui.badge(m.status), m.open_reorder ? el('div', { style: { marginTop: '4px', textAlign: 'right' } }, el('span.chip-s.info', {}, icon('clipboard', 'sm'), 'Reorder open')) : null] },
          { label: 'In raw store', cls: 'num', render: (m) => (m.category === 'EMPTY_BOTTLE' ? el('span', {}, L.qty(m.stock_total, m.stock_unit), el('small.block.muted', { text: 'on the blow floor' })) : L.qty(m.stock_rm_store, m.stock_unit)) },
          { label: 'Days of cover', cls: 'num', render: (m) => (m.category !== 'EMPTY_BOTTLE' && m.days_cover ? `${ui.fmtNum(m.days_cover, 1)} days` : '-') },
          { label: 'Used per day', cls: 'num', hideMobile: true, render: (m) => (ui.toNum(m.avg_daily_burn) > 0 ? L.qty(m.avg_daily_burn, m.stock_unit, 1) : '-') },
          { label: 'Minimum', cls: 'num', hideMobile: true, render: (m) => L.qty(m.min_stock, m.stock_unit) },
          { label: '', cls: 'a', render: (m) => el('div.btn-row', {}, L.btn('Details', 'list', { kind: 'secondary', sm: true, onClick: () => openDetail(m) }),
            m.status !== 'OK' && !m.open_reorder ? L.btn('Order', 'plus', { kind: 'secondary', sm: true, onClick: () => ctx.goto('po', { materialId: m.id }) }) : null) },
        ],
        rows, onRow: openDetail,
        emptyState: { icon: 'box', title: st.q || st.category || st.low ? 'No materials match' : 'No raw materials yet', text: st.q || st.category || st.low ? 'Clear the search or filters to see everything.' : 'An administrator adds materials under Administration > Master data.' },
      });
    });
  }
  refreshers.stock = load;
  load();
}

async function openDetail(m) {
  const body = el('div.stack', {}, ui.skeletonCards(2));
  const order = L.btn('Order more', 'plus', { kind: 'secondary', onClick: () => { dlg.close(); ctx.goto('po', { materialId: m.id }); } });
  const dlg = ui.openDialog({ title: m.name, body, actions: [order], wide: true });
  try {
    const d = await api.get(`/api/raw-materials/${m.id}`, L.readParam());
    const sup = (await L.refSuppliers().catch(() => [])).find((s) => s.id === d.default_supplier_id);
    const lots = d.lots || [];
    const firstStore = lots.findIndex((l) => l.zone === 'RM_STORE');
    const lotRows = lots.length ? ui.cardsTable({
      columns: [
        { label: 'Lot', cls: 'k', render: (l) => [el('b', { text: l.lot_no || l.grn_no || 'Opening stock' }), el('small', { text: [l.grn_no && l.lot_no ? l.grn_no : '', ERP.godown.multiple ? l.location_name : ''].filter(Boolean).join(' - ') })] },
        { label: 'Where', render: (l) => L.zoneLabel(l.zone) },
        { label: 'Quantity', cls: 'num', render: (l) => L.qty(l.qty, d.stock_unit) },
        { label: 'Expiry', render: (l) => (l.expiry_date ? [ui.fmtDate(l.expiry_date), ' ', L.daysLeftBadge(L.daysBetween(l.expiry_date))] : 'no expiry') },
        { label: 'Use order', cls: 's', render: (l) => (lots.indexOf(l) === firstStore && l.zone === 'RM_STORE' ? el('span.chip-s.info', { text: 'Used first (FEFO)' }) : null) },
      ], rows: lots }) : el('p.muted', { text: 'No stock of this material in the selected godown.' });
    const ledger = (d.recent_ledger || []).length ? ui.cardsTable({
      columns: [
        { label: 'When', cls: 'k', render: (x) => [el('b', { text: TXN[x.txn_type] || x.txn_type }), el('small', { text: ui.fmtDateTime(x.at) })] },
        { label: 'Change', cls: 'num', render: (x) => el('span', { class: ui.toNum(x.qty_delta) < 0 ? 'delta-neg' : 'delta-pos', text: `${ui.toNum(x.qty_delta) > 0 ? '+' : ''}${ui.fmtNum(x.qty_delta, 4)} ${d.stock_unit}` }) },
        { label: 'Zone', hideMobile: true, render: (x) => L.zoneLabel(x.zone) },
        { label: 'Balance', cls: 'num', hideMobile: true, render: (x) => ui.fmtNum(x.balance_after, 4) },
        { label: 'Note', cls: 'w', hideMobile: true, render: (x) => x.note },
      ], rows: d.recent_ledger }) : el('p.muted', { text: 'No movements yet.' });
    ui.mount(body, el('div.stack', {},
      L.kv([
        ['Category', L.CATEGORY_LABEL[d.category] || d.category],
        ['Stock unit', d.stock_unit], ['Bought in', `${d.purchase_unit}${ui.toNum(d.stock_per_purchase_unit) !== 1 ? ` (1 ${d.purchase_unit} = ${ui.fmtNum(d.stock_per_purchase_unit, 4)} ${d.stock_unit})` : ''}`],
        ['In raw store', L.qty(d.stock_rm_store, d.stock_unit)], ['Total in godown', L.qty(d.stock_total, d.stock_unit)], ['Minimum', L.qty(d.min_stock, d.stock_unit)],
        ['Usual supplier', sup ? sup.name : '-'],
      ]),
      el('h3', { text: 'Lots - oldest expiry used first' }), lotRows,
      el('h3', { text: 'Recent movements' }), ledger));
  } catch (e) { ui.mount(body, ui.errorState(e, () => { dlg.close(); openDetail(m); })); }
}

/* ---------------------------------------------------------------------------------------------- Reorder */
function mountReorder(panel) {
  const host = el('div.stack');
  const runBtn = L.btn('Check now', 'refresh', { kind: 'secondary', sm: true, onClick: () => L.guarded(runBtn, async () => { await api.post('/api/raw-materials/reorder-recommendations/run', {}); ui.toast.ok('Stock checked against minimum levels and usage'); await load(); }) });
  panel.append(el('div.page-head', {}, el('div', {}, el('p.sub', { text: 'Suggested purchases from current stock, daily usage and supplier lead time.' })), el('div.actions', {}, runBtn)), host);

  async function load() {
    await L.refreshInto(host, async () => {
      const recs = await api.get('/api/raw-materials/reorder-recommendations', { status: 'OPEN' });
      ctx.badge('reorder', recs.length);
      if (!recs.length) return ui.empty({ icon: 'check-circle', title: 'Nothing to reorder', text: 'All materials have enough stock for their lead time. Press "Check now" to re-evaluate.' });
      return el('div.grid.auto', {}, recs.map(recCard));
    });
  }
  function recCard(r) {
    const [tone, label] = REASON[r.reason] || ['warn', r.reason];
    const suggested = `${ui.fmtNum(r.suggested_qty_purchase, 4)} ${r.purchase_unit}`;
    const asStock = `${ui.fmtNum(ui.toNum(r.suggested_qty_purchase) * ui.toNum(r.stock_per_purchase_unit), 4)} ${r.stock_unit}`;
    const create = L.btn('Create PO', 'plus', { kind: 'primary', onClick: () => quickPo(r, create) });
    const edit = L.btn('Edit first', 'edit', { kind: 'secondary', onClick: () => ctx.goto('po', { materialId: r.raw_material_id, qty: r.suggested_qty_purchase, supplierId: r.suggested_supplier_id }) });
    const dismiss = L.btn('Dismiss', 'x', { kind: 'ghost', onClick: () => dismissRec(r, dismiss) });
    return el('article.card.stack.tight', {},
      el('div.row.between', { style: { flexWrap: 'nowrap', alignItems: 'flex-start' } }, el('div', {}, el('b', { text: r.material_name }), el('div.small.muted', { text: r.material_code })), el('span.badge', { class: tone }, icon('alert'), label)),
      L.kv([
        ['In stock now', L.qty(r.current_stock, r.stock_unit)], ['Minimum', L.qty(r.min_stock, r.stock_unit)],
        ['Used per day', ui.toNum(r.avg_daily_burn) > 0 ? L.qty(r.avg_daily_burn, r.stock_unit, 1) : '-'], ['Days of cover', r.days_cover ? `${ui.fmtNum(r.days_cover, 1)} days` : '-'],
        ['Supplier lead time', `${r.lead_time_days} days (+${r.safety_days} safety)`], ['Supplier', r.supplier_name || el('span.chip-s.warn', { text: 'None set - choose one' })],
      ]),
      el('div.banner.inline.info', {}, icon('clipboard'), el('div.grow', {}, el('b', { text: `Suggested order: ${suggested}` }), el('div.small', { text: `= ${asStock} - enough for the lead time plus safety stock` }))),
      (r.stock_by_location || []).length > 1 ? el('div.chip-row', {}, r.stock_by_location.map((x) => el('span.chip-s', { text: `${x.location_name}: ${ui.fmtNum(x.qty, 0)}` }))) : null,
      el('div.btn-row', {}, create, edit, dismiss));
  }
  async function quickPo(r, button) {
    const suppliers = await L.refSuppliers().catch(() => []);
    const f = ui.field({ label: 'Supplier', name: 'supplier_id', type: 'select', required: true, options: [{ value: '', label: 'Choose supplier...' }, ...suppliers.map((s) => ({ value: s.id, label: s.name }))], value: r.suggested_supplier_id || '' });
    const form = el('form.stack', { id: 'quickpo', novalidate: true }, el('div.form-error', { role: 'alert' }),
      el('p', {}, 'Create a purchase order for ', el('b', { text: `${ui.fmtNum(r.suggested_qty_purchase, 4)} ${r.purchase_unit} of ${r.material_name}` }), '?'), f.el);
    const ok = L.btn('Create purchase order', 'check', { kind: 'primary', type: 'submit', form: 'quickpo' });
    const cancel = L.btn('Cancel', null, { kind: 'secondary', onClick: () => dlg.close() });
    const dlg = ui.openDialog({ title: 'New purchase order', body: form, actions: [cancel, ok] });
    const flow = L.makeFlow();
    form.addEventListener('submit', async (ev) => {
      ev.preventDefault();
      if (!f.get()) { f.setError('Choose a supplier'); return; }
      const po = await L.submitPlain({ flow, form, button: ok, path: `/api/raw-materials/reorder-recommendations/${r.id}/create-po`, body: { supplier_id: f.get() } });
      if (po) { dlg.close(); ui.toast.ok(`Created ${po.po_no}`, { action: { label: 'Open', onClick: () => ctx.goto('po', { open: po.id }) } }); load(); refreshers.po && refreshers.po(); }
    });
  }
  async function dismissRec(r, button) {
    if (!(await ui.confirmDialog({ title: 'Dismiss this suggestion?', message: `${r.material_name} will not be suggested again for 24 hours unless stock drops further.`, confirmText: 'Dismiss' }))) return;
    await L.guarded(button, async () => { await api.action().post(`/api/raw-materials/reorder-recommendations/${r.id}/dismiss`, {}); ui.toast.ok('Dismissed'); await load(); });
  }
  refreshers.reorder = load;
  load();
}

/* ---------------------------------------------------------------------------------------------- tabs */
const ids = ['stock', 'reorder', 'po', 'grn', 'returns'];
const hashId = (location.hash || '').slice(1);
const tabs = L.tabbed(main, [
  { id: 'stock', label: 'Stock', icon: 'box', render: mountStock },
  { id: 'reorder', label: 'Reorder', icon: 'clipboard', render: mountReorder },
  { id: 'po', label: 'Purchase orders', icon: 'list', render: (p) => mountPo(p, ctx) },
  { id: 'grn', label: 'Receive goods', icon: 'download', render: (p) => mountGrn(p, ctx, 'grn') },
  { id: 'returns', label: 'Returns', icon: 'arrow-right', render: (p) => mountGrn(p, ctx, 'returns') },
], { initial: hashId === 'reorders' ? 'reorder' : (ids.includes(hashId) ? hashId : 'stock') });
// reorder badge even before the tab is opened
api.get('/api/raw-materials/reorder-recommendations', { status: 'OPEN' }).then((r) => ctx.badge('reorder', r.length)).catch(() => {});

const refreshAll = () => Promise.all(Object.entries(refreshers).filter(([k]) => !k.includes(':')).map(([, f]) => f()));
ERP.godown.onChange(() => { refreshAll(); });
ERP.live.start('raw-materials', { materials: refreshAll, stock: refreshAll, master: () => { L.invalidateRef(); return refreshAll(); } });
