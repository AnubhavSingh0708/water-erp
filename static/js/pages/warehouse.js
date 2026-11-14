// warehouse.html - Overview | Stock | Ledger | Actions (adjust, move, write-off, physical count).
import { boot, ERP } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import * as L from '/js/pages/warehouse-lib.js';
import { mountActions } from '/js/pages/warehouse-actions.js';

const { el, icon } = ui;
await boot({ page: 'warehouse', title: 'Warehouse', godownAll: true });
const main = document.getElementById('main');
const refreshers = {};
const ctx = { refreshers, goto: (id, arg) => { tabs.select(id); if (arg && refreshers[`${id}:open`]) refreshers[`${id}:open`](arg); } };

const scopeNote = () => (ERP.godown.multiple ? el('p.section-sub', { text: ERP.godown.current() === 'all' ? 'Showing all godowns combined (read-only totals).' : `Godown: ${ERP.godown.currentName()}` }) : null);

/* ---------------------------------------------------------------------------------------------- Overview */
function mountOverview(panel) {
  const host = el('div'); panel.append(host);
  async function load() {
    await L.refreshInto(host, async () => {
      const s = await api.get('/api/warehouse/stock/summary', L.readParam());
      const gname = (id) => (ERP.godown.list.find((g) => g.id === id) || {}).name || 'Godown';
      const zoneTiles = (s.zones || []).map((z) => el('button.stat.zone-tile', { type: 'button', on: { click: () => ctx.goto('stock', { zone: z.zone }) }, 'aria-label': `${L.zoneLabel(z.zone)}: ${z.items} items. Open stock list` },
        el('div.s-label', { text: L.zoneLabel(z.zone) }), el('div.s-value', { text: ui.fmtNum(z.items, 0) }), el('div.s-sub', { text: z.items === 1 ? 'item here' : 'items here' })));
      const prods = (s.products || []).filter((p) => ['quarantine', 'fg_ready', 'scrap', 'jar_empty', 'jar_clean'].some((k) => ui.toNum(p[k]) > 0));
      return el('div.stack', {}, scopeNote(),
        el('div.stat-grid', {}, zoneTiles),
        el('section.card.pad0', {}, el('div.card-head', {}, el('h2', {}, icon('package'), 'Finished goods')),
          el('div', { style: { padding: '0 var(--s4) var(--s4)' } }, ui.cardsTable({
            columns: [
              { label: 'Product', cls: 'k', render: (p) => [el('b', { text: p.name }), el('small', { text: p.kind === 'RETURNABLE' ? 'Returnable jar' : 'Bottled' })] },
              { label: 'Ready to load', cls: 'num', render: (p) => L.cratesCell(p.fg_ready, p.crates_per_pallet, p.kind === 'RETURNABLE' ? 'jars' : 'crates') },
              { label: 'In QC', cls: 'num', render: (p) => (ui.toNum(p.quarantine) > 0 ? ui.fmtNum(p.quarantine, 0) : '-') },
              { label: 'Empty / clean jars', cls: 'num', hideMobile: true, render: (p) => (p.kind === 'RETURNABLE' ? `${ui.fmtNum(p.jar_empty, 0)} / ${ui.fmtNum(p.jar_clean, 0)}` : '-') },
              { label: 'Scrap', cls: 'num', hideMobile: true, render: (p) => (ui.toNum(p.scrap) > 0 ? ui.fmtNum(p.scrap, 0) : '-') },
            ],
            rows: prods, emptyState: { icon: 'package', title: 'No finished goods yet', text: 'Approved batches appear here, ready to load.' },
          }))),
        ERP.godown.multiple && (s.by_location || []).length > 1 ? el('section.card.pad0', {}, el('div.card-head', {}, el('h2', {}, icon('pin'), 'By godown')), el('div', { style: { padding: '0 var(--s4) var(--s4)' } }, ui.cardsTable({
          columns: [{ label: 'Godown', cls: 'k', render: (g) => [el('b', { text: g.name }), el('small', { text: g.kind === 'PLANT' ? 'Plant' : 'Godown' })] }, { label: 'Raw items', cls: 'num', render: (g) => ui.fmtNum(g.raw_items, 0) },
            { label: 'Finished goods', cls: 'num', render: (g) => `${ui.fmtNum(g.fg_ready_crates, 0)} crates` }, { label: 'In QC', cls: 'num', render: (g) => `${ui.fmtNum(g.quarantine_crates, 0)}` }], rows: s.by_location }))) : null,
        (s.in_transit || []).length ? el('section.card.pad0', {}, el('div.card-head', {}, el('h2', {}, icon('transfer'), 'On the road between godowns'), el('a.btn.ghost.sm', { href: '/godowns.html' }, 'Transfers', icon('chevron-right', 'sm'))), el('div', { style: { padding: '0 var(--s4) var(--s3)' } },
          el('ul.list', {}, s.in_transit.map((t) => { const h = (Date.now() - new Date(t.created_at).getTime()) / 3600000; return el('li', {}, el('div.grow', {}, el('b', { text: t.tf_no }), el('span.small.muted', { text: `${gname(t.from_location_id)} to ${gname(t.to_location_id)} - sent ${ui.fmtRelative(t.created_at)}` })), h > 48 ? ui.badge('OVERDUE', 'Over 48 h') : ui.badge('IN_TRANSIT')); })))) : null);
    });
  }
  refreshers.overview = load; load();
}

/* ---------------------------------------------------------------------------------------------- Stock */
function mountStock(panel) {
  const st = { q: '', type: '', zone: '', low: false, view: 'stock', days: '60' };
  const search = el('input.input', { type: 'search', placeholder: 'Search item or code', 'aria-label': 'Search stock', maxlength: 100, autocomplete: 'off' });
  const typeSeg = ui.segmented({ options: [{ value: '', label: 'All' }, { value: 'RAW', label: 'Materials' }, { value: 'PRODUCT', label: 'Products' }], value: '', label: 'Item type', onChange: (v) => { st.type = v; load(); } });
  const zone = ui.field({ label: 'Zone', name: 'zone', type: 'select', options: [{ value: '', label: 'All zones' }, ...Object.entries(L.ZONE_LABEL).filter(([k]) => k !== 'TRANSIT').map(([v, l]) => ({ value: v, label: l }))] });
  const days = ui.field({ label: 'Expiring within', name: 'days', type: 'select', value: '60', options: [{ value: '30', label: '30 days' }, { value: '60', label: '60 days' }, { value: '90', label: '90 days' }, { value: '180', label: '180 days' }] });
  const low = el('input', { type: 'checkbox', name: 'low' });
  const viewSeg = ui.segmented({ options: [{ value: 'stock', label: 'Stock' }, { value: 'expiring', label: 'Expiring soon' }], value: 'stock', label: 'View', onChange: (v) => { st.view = v; sync(); load(); } });
  const csv = L.btn('CSV', 'download', { kind: 'secondary', sm: true, onClick: () => L.guarded(csv, () => api.download('/api/warehouse/stock', { query: { format: 'csv', ...L.readParam(), zone: st.zone, item_type: st.type, q: st.q, low: st.low ? 1 : '' } })) });
  const host = el('div');
  const filters = el('div.filters', {}, zone.el, days.el, el('label.check.bare', { class: 'low-wrap' }, low, el('span', { text: 'Only low stock' })));
  panel.append(el('div.seg-row', {}, viewSeg.el), el('div.toolbar', {}, el('div.searchbar', {}, icon('search'), search), el('div.toolbar-end', {}, csv)), el('div.seg-row', {}, typeSeg.el), filters, host);
  const sync = () => { days.el.hidden = st.view !== 'expiring'; zone.el.hidden = st.view === 'expiring' && false; filters.querySelector('.low-wrap').hidden = st.view === 'expiring'; csv.hidden = st.view === 'expiring'; typeSeg.el.parentElement.hidden = st.view === 'expiring'; search.parentElement.hidden = st.view === 'expiring'; };
  sync();
  search.addEventListener('input', ui.debounce(() => { st.q = search.value.trim(); load(); }, 350));
  zone.input.addEventListener('change', () => { st.zone = zone.get(); load(); }); days.input.addEventListener('change', () => { st.days = days.get(); load(); }); low.addEventListener('change', () => { st.low = low.checked; load(); });

  async function load() {
    host.dataset.loaded = host.dataset.loaded && host._view === st.view ? host.dataset.loaded : ''; if (!host.dataset.loaded) delete host.dataset.loaded; host._view = st.view;
    await L.refreshInto(host, st.view === 'expiring' ? expiring : stock);
  }
  async function stock() {
    const rows = await api.get('/api/warehouse/stock', { ...L.readParam(), zone: st.zone, item_type: st.type, q: st.q, low: st.low ? 1 : '' });
    const multi = ERP.godown.current() === 'all';
    return el('div.stack.tight', {}, scopeNote(), ui.cardsTable({
      columns: [
        { label: 'Item', cls: 'k', render: (r) => [el('b', { text: r.name }), el('small', { text: `${r.code}${multi ? ` - ${r.location_name}` : ''}` })] },
        { label: 'Zone', cls: 's', render: (r) => el('span.chip-s', { text: L.zoneLabel(r.zone) }) },
        { label: 'Quantity', cls: 'num', render: (r) => (r.item_type === 'PRODUCT' ? L.cratesCell(r.qty, r.crates_per_pallet, r.kind === 'RETURNABLE' ? 'jars' : 'crates') : L.qty(r.qty, r.unit)) },
        { label: 'Lots', cls: 'num', hideMobile: true, render: (r) => `${r.lots}` },
        { label: 'Godown', hideMobile: true, render: (r) => (multi ? r.location_name : '') },
        { label: '', cls: 'a', render: (r) => L.btn('Lots and details', 'list', { kind: 'secondary', sm: true, onClick: () => openItem(r) }) },
      ], rows, onRow: openItem,
      emptyState: { icon: 'warehouse', title: st.q || st.zone || st.type || st.low ? 'Nothing matches' : 'No stock here yet', text: st.q || st.zone || st.type || st.low ? 'Clear the search or filters.' : 'Stock appears here once goods are received or produced.' },
    }));
  }
  async function expiring() {
    const rows = await api.get('/api/warehouse/expiring', { ...L.readParam(), days: st.days });
    return el('div.stack.tight', {}, rows.some((r) => r.days_left < 0) ? L.banner('err', 'alert', el('b', { text: 'Some stock has expired. ' }), 'Check it and write it off if it cannot be used.') : null, ui.cardsTable({
      columns: [
        { label: 'Item', cls: 'k', render: (r) => [el('b', { text: r.name }), el('small', { text: `${r.lot_label || 'lot'}${ERP.godown.multiple ? ` - ${r.location_name}` : ''}` })] },
        { label: 'Expiry', cls: 's', render: (r) => L.daysLeftBadge(r.days_left) },
        { label: 'Quantity', cls: 'num', render: (r) => (r.item_type === 'PRODUCT' ? `${ui.fmtNum(r.qty, 0)} crates` : ui.fmtNum(r.qty, 4)) }, { label: 'Zone', render: (r) => L.zoneLabel(r.zone) }, { label: 'Date', render: (r) => ui.fmtDate(r.expiry_date) },
      ], rows, emptyState: { icon: 'check-circle', title: 'Nothing is about to expire', text: `No lots expire within ${st.days} days.` } }));
  }
  async function openItem(r) {
    const body = el('div.stack', {}, ui.skeletonCards(1));
    const dlg = ui.openDialog({ title: r.name, body, wide: true });
    try {
      const d = await api.get('/api/warehouse/stock/item', { item_type: r.item_type, item_id: r.item_id, ...L.readParam() });
      const unit = r.item_type === 'PRODUCT' ? 'crates' : d.unit;
      ui.mount(body, el('div.stack', {}, L.kv([['Item', `${d.name} (${d.code})`], ['Total (without scrap)', L.qty(d.total, unit)]]),
        el('h3', { text: 'Lots - first to expire is used first' }),
        ui.cardsTable({ columns: [
          { label: 'Lot / batch', cls: 'k', render: (l) => [el('b', { text: l.lot_label || (r.item_type === 'PRODUCT' ? 'Containers' : 'Opening stock') }), el('small', { text: `${ERP.godown.multiple ? `${l.location_name} - ` : ''}${L.zoneLabel(l.zone)}` })] },
          { label: 'Quantity', cls: 'num', render: (l) => (r.item_type === 'PRODUCT' && r.crates_per_pallet ? L.cratesCell(l.qty, r.crates_per_pallet) : L.qty(l.qty, unit)) },
          { label: 'Expiry', render: (l) => (l.expiry_date ? [ui.fmtDate(l.expiry_date), ' ', L.daysLeftBadge(L.daysBetween(l.expiry_date))] : 'no expiry') },
          { label: 'QC', cls: 's', render: (l) => (l.qc_status ? ui.badge(l.qc_status) : null) },
          { label: 'Held for loading', cls: 'num', hideMobile: true, render: (l) => (ui.toNum(l.reserved) > 0 ? ui.fmtNum(l.reserved, 0) : '-') },
        ], rows: d.lots, emptyState: { icon: 'box', title: 'No lots' } })));
    } catch (e) { ui.mount(body, ui.errorState(e, () => { dlg.close(); openItem(r); })); }
  }
  refreshers.stock = load;
  refreshers['stock:open'] = (a) => { if (a.zone !== undefined) { st.zone = a.zone; zone.set(a.zone); st.view = 'stock'; viewSeg.set('stock'); sync(); load(); } };
  load();
}

/* ---------------------------------------------------------------------------------------------- Ledger */
function mountLedger(panel) {
  const st = { item: '', zone: '', txn: '', from: '', to: '', offset: 0 };
  const host = el('div'); const limit = 25;
  const item = ui.field({ label: 'Item', name: 'item', type: 'select', options: [{ value: '', label: 'All items' }] });
  Promise.all([L.refMaterials(), L.refProducts()]).then(([m, p]) => { [...m.map((x) => ['RAW', x]), ...p.map((x) => ['PRODUCT', x])].forEach(([t, x]) => item.input.append(el('option', { value: `${t}:${x.id}`, text: x.name }))); }).catch(() => {});
  const zone = ui.field({ label: 'Zone', name: 'ledger_zone', type: 'select', options: [{ value: '', label: 'All zones' }, ...Object.entries(L.ZONE_LABEL).filter(([k]) => k !== 'TRANSIT').map(([v, l]) => ({ value: v, label: l }))] });
  const txn = ui.field({ label: 'What happened', name: 'txn', type: 'select', options: [{ value: '', label: 'Everything' }, ...['GRN', 'GRN_VOID', 'RTV_OUT', 'BLOW_ISSUE', 'BATCH_CONSUME', 'BATCH_WASTE', 'BATCH_OUTPUT', 'QC_RELEASE', 'QC_REJECT', 'ADJUST', 'TRANSFER', 'WRITE_OFF', 'DISPOSE', 'GATE_PASS', 'DISPATCH', 'GODOWN_TRANSFER_OUT', 'GODOWN_TRANSFER_IN'].map((t) => ({ value: t, label: L.txnLabel(t) }))] });
  const from = ui.field({ label: 'From date', name: 'from', type: 'date' }); const to = ui.field({ label: 'To date', name: 'to', type: 'date' });
  const csv = L.btn('CSV', 'download', { kind: 'secondary', sm: true, onClick: () => L.guarded(csv, () => api.download('/api/warehouse/ledger', { query: { format: 'csv', ...query() } })) });
  const clear = L.btn('Clear filters', 'x', { kind: 'ghost', sm: true, onClick: () => { [item, zone, txn, from, to].forEach((f) => f.set('')); apply(); } });
  panel.append(el('div.filters', {}, item.el, zone.el, txn.el, from.el, to.el), el('div.btn-row.end', {}, clear, csv), host);
  const apply = () => { st.item = item.get(); st.zone = zone.get(); st.txn = txn.get(); st.from = from.get(); st.to = to.get(); st.offset = 0; load(); };
  [item, zone, txn, from, to].forEach((f) => f.input.addEventListener('change', apply));
  const query = () => { const [t, id] = st.item ? st.item.split(':') : ['', '']; return { ...L.readParam(), item_type: t, item_id: id, zone: st.zone, txn_type: st.txn, from: st.from, to: st.to }; };
  async function load() {
    await L.refreshInto(host, async () => {
      const d = await api.get('/api/warehouse/ledger', { ...query(), limit, offset: st.offset });
      const multi = ERP.godown.current() === 'all';
      return el('div.stack.tight', {}, scopeNote(), ui.cardsTable({
        columns: [
          { label: 'Item', cls: 'k', render: (r) => [el('b', { text: r.item_name }), el('small', { text: `${ui.fmtDateTime(r.at)}${multi ? ` - ${r.location_name}` : ''}` })] },
          { label: 'Change', cls: 's', render: (r) => el('b', { class: ui.toNum(r.qty_delta) < 0 ? 'delta-neg' : 'delta-pos', text: `${ui.toNum(r.qty_delta) > 0 ? '+' : ''}${ui.fmtNum(r.qty_delta, 4)}` }) },
          { label: 'What', render: (r) => L.txnLabel(r.txn_type) }, { label: 'Zone', render: (r) => L.zoneLabel(r.zone) },
          { label: 'Balance after', cls: 'num', hideMobile: true, render: (r) => ui.fmtNum(r.balance_after, 4) }, { label: 'By', hideMobile: true, render: (r) => r.user_name },
          { label: 'Note', cls: 'w', hideMobile: true, render: (r) => r.note },
        ], rows: d.items, onRow: ledgerDetail, emptyState: { icon: 'history', title: 'No movements match', text: 'Change or clear the filters above.' },
      }), ui.pager({ total: d.total, limit, offset: st.offset, onPage: (o) => { st.offset = o; load(); window.scrollTo({ top: 0 }); } }));
    });
  }
  function ledgerDetail(r) {
    ui.openDialog({ title: L.txnLabel(r.txn_type), body: L.kv([['Item', r.item_name], ['When', ui.fmtDateTime(r.at)], ['Godown', r.location_name], ['Zone', L.zoneLabel(r.zone)], ['Change', `${ui.toNum(r.qty_delta) > 0 ? '+' : ''}${ui.fmtNum(r.qty_delta, 4)}`], ['Balance after', ui.fmtNum(r.balance_after, 4)],
      ['Lot', r.lot_id && r.lot_id !== L.NIL_UUID ? el('span.mono', { text: r.lot_id.slice(0, 8) }) : '-'], ['Document', r.ref_type ? `${r.ref_type}` : '-'], ['Done by', r.user_name], ['Note', r.note]]) });
  }
  refreshers.ledger = load; load();
}

/* ---------------------------------------------------------------------------------------------- tabs */
const ids = ['overview', 'stock', 'ledger', 'actions'];
const hashId = (location.hash || '').slice(1);
const tabs = L.tabbed(main, [
  { id: 'overview', label: 'Overview', icon: 'dashboard', render: mountOverview },
  { id: 'stock', label: 'Stock', icon: 'warehouse', render: mountStock },
  { id: 'ledger', label: 'Ledger', icon: 'history', render: mountLedger },
  { id: 'actions', label: 'Actions', icon: 'edit', render: (p) => mountActions(p, ctx) },
], { initial: ids.includes(hashId) ? hashId : 'overview' });

const refreshAll = () => Promise.all(Object.entries(refreshers).filter(([k]) => !k.includes(':')).map(([, f]) => f()));
ERP.godown.onChange(refreshAll);
ERP.live.start('warehouse', { stock: refreshAll, production: refreshAll, qc: refreshAll, master: () => { L.invalidateRef(); return refreshAll(); } });
