// jars.html - returnable-container fleet: overview, customers (holdings, overdue, ledger), plant actions, destroyed log.
import { boot, ERP } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { explain } from '/js/pages/loading-common.js';
import * as A from '/js/pages/jars-actions.js';

const { el, icon } = ui;
await boot({ page: 'jars', title: 'Returnable jars', godownAll: true });
const main = document.getElementById('main');

const state = { tab: (location.hash || '#overview').slice(1), productId: null, days: 30, summary: [], fleet: null, customers: [], q: '', overdue: false, all: false, writeLoc: null, destroyed: null, reason: '', dOffset: 0, loaded: false };
if (!['overview', 'customers', 'actions', 'destroyed'].includes(state.tab)) state.tab = 'overview';
const product = () => state.summary.find((p) => p.product_id === state.productId) || state.summary[0] || null;
const holdings = () => new Map((state.hold || []).map((c) => [c.customer_id, c.holding]));
const loc = () => ERP.godown.param();

/* ---------------------------------------------------------------------------------------------- shell */
const sub = el('p.sub'); const prodSel = el('select.select.prod-sel', { 'aria-label': 'Container type', hidden: true, on: { change: () => { state.productId = prodSel.value; reloadAll(); } } });
const periodSeg = ui.segmented({ label: 'Period', value: '30', options: [{ value: '7', label: '7 days' }, { value: '30', label: '30 days' }, { value: '90', label: '90 days' }], onChange: (v) => { state.days = Number(v); loadFleet().then(renderOverview); } });
const tabs = ui.tabs({ label: 'Jar sections', active: state.tab, tabs: [{ id: 'overview', label: 'Overview', icon: 'jar' }, { id: 'customers', label: 'Customers', icon: 'users' }, { id: 'actions', label: 'Actions', icon: 'wrench' }, { id: 'destroyed', label: 'Destroyed', icon: 'trash' }], onChange: (id) => show(id) });
const panes = { overview: el('div.stack.loose'), customers: el('div.stack'), actions: el('div.stack.loose'), destroyed: el('div.stack') };
ui.mount(main, el('div.page-head', {}, el('div', {}, sub), el('div.actions', {}, prodSel)), el('div.sticky-tabs', {}, tabs.el), ...Object.entries(panes).map(([k, p]) => { p.id = `pane-${k}`; p.hidden = k !== state.tab; return p; }));
ui.mount(panes[state.tab], ui.skeletonCards(3));

function show(id) {
  state.tab = id; history.replaceState(null, '', `#${id}`); tabs.select(id);
  Object.entries(panes).forEach(([k, p]) => { p.hidden = k !== id; });
  paint(id);
}
function paint(id) { ({ overview: renderOverview, customers: renderCustomers, actions: renderActions, destroyed: renderDestroyed })[id](); }

/* ---------------------------------------------------------------------------------------------- data */
async function loadSummary() {
  state.summary = await api.get('/api/jars/summary', loc());
  if (!state.productId || !state.summary.some((p) => p.product_id === state.productId)) state.productId = (state.summary[0] || {}).product_id || null;
  prodSel.hidden = state.summary.length < 2; ui.mount(prodSel, state.summary.map((p) => el('option', { value: p.product_id, selected: p.product_id === state.productId }, p.name)));
  const g = ERP.godown; sub.textContent = g.currentName() ? `Showing: ${g.currentName()}` : 'Returnable containers';
}
async function loadFleet() {
  const p = product(); if (!p) { state.fleet = null; return; }
  const to = ui.todayPlant(); const from = ui.addDays(to, -(state.days - 1));
  state.fleet = await api.get('/api/jars/fleet', { product_id: p.product_id, from, to, ...loc() });
}
async function loadCustomers() {
  const p = product(); if (!p) { state.customers = []; return; }
  if (!state.q && !state.overdue && !state.all) { state.customers = await api.get('/api/jars/customers', { product_id: p.product_id }); state.hold = state.customers; return; }
  if (!state.hold) state.hold = await api.get('/api/jars/customers', { product_id: p.product_id });
  state.customers = await api.get('/api/jars/customers', { product_id: p.product_id, q: state.q, overdue: state.overdue ? 1 : '', all: state.all ? 1 : '' });
}
async function loadDestroyed() {
  const p = product(); if (!p) { state.destroyed = { items: [], total: 0 }; return; }
  state.destroyed = await api.get('/api/jars/destroyed', { product_id: p.product_id, reason: state.reason, limit: 25, offset: state.dOffset });
}
async function reloadAll() {
  state.hold = null;
  try {
    await loadSummary();
    if (!product()) { Object.values(panes).forEach((p) => ui.mount(p, ui.empty({ icon: 'jar', title: 'No returnable container product', text: 'An administrator can add a product of kind Returnable (for example a 20 L jar) under Administration -> Master data.' }))); return; }
    await Promise.all([loadFleet(), loadCustomers(), state.tab === 'destroyed' ? loadDestroyed() : null]);
    state.loaded = true; paint(state.tab);
    if (state.tab !== 'overview') paintBadges();
  } catch (e) {
    if (e.redirecting) return;
    if (state.loaded && e.network) { ui.toast.warn('Showing the last data - server unreachable', { ms: 3000 }); return; }
    ui.mount(panes[state.tab], ui.errorState(e, reloadAll));
  }
}
function paintBadges() { /* tab badges: overdue count */ }

/* ---------------------------------------------------------------------------------------------- overview */
const stat = (label, v, tone, ic, hint) => el('div.kpi', { class: tone || '' }, el('div.k-label', {}, ic ? icon(ic, 'sm') : null, label), el('div.k-value', { text: ui.fmtNum(v, 0) }), hint ? el('div.k-sub', { text: hint }) : null);
function renderOverview() {
  const f = state.fleet, p = product(); if (!f || !p) return;
  const fl = f.fleet, per = f.period;
  const segs = [['At plant - empty', fl.plant_empty, 'c-empty'], ['At plant - clean', fl.plant_clean, 'c-clean'], ['Filled - ready', fl.filled_ready, 'c-filled'], ['Filled - in QC', fl.filled_quarantine, 'c-qc'], ['With customers', fl.with_customers, 'c-cust'], ['In vehicles', fl.in_vehicles, 'c-veh']];
  const tot = Math.max(1, fl.total);
  const alerts = [];
  if (f.alert === 'HIGH_DESTROY_RATE' || (f.alerts || []).includes('HIGH_DESTROY_RATE')) alerts.push(['bad', `Too many containers are being destroyed: ${f.destroy_rate_pct}% of fills in this period (warning above ${f.warn_pct}%). Check washing and handling.`]);
  if ((f.alerts || []).includes('NEAR_RETIREMENT')) alerts.push(['warn', `The fleet is close to its life limit: ${f.retirement_pct}% of ${f.max_cycles} cycles. Plan replacements.`]);
  const ret = f.retirement_pct === null || f.retirement_pct === undefined ? null : Number(f.retirement_pct);
  const maxReason = Math.max(1, ...Object.values(per.destroyed_by_reason));
  ui.mount(panes.overview,
    ...alerts.map(([t, m]) => el('div.banner.inline', { class: t }, icon('alert'), el('div.grow', { text: m }))),
    el('section.card', {}, el('div.card-head', {}, el('h2', {}, icon('jar'), `Fleet - ${p.name}`), f.alert !== 'OK' ? ui.badge(f.alert) : ui.badge('OK', 'Healthy')),
      el('div.fleet-total', {}, el('b', { text: ui.fmtNum(fl.total, 0) }), el('span', { text: 'containers in total' })),
      el('div.fleet-bar', { role: 'img', 'aria-label': segs.map(([l, v]) => `${l} ${v}`).join(', ') }, segs.filter(([, v]) => v > 0).map(([l, v, c]) => el('i', { class: c, title: `${l}: ${v}`, style: { width: `${(v * 100) / tot}%` } }))),
      el('ul.legend', {}, segs.map(([l, v, c]) => el('li', {}, el('i', { class: c }), el('span', { text: l }), el('b', { text: ui.fmtNum(v, 0) }))))),
    el('div.grid.kpis', {}, stat('Empty at plant', fl.plant_empty, '', 'jar', 'need washing'), stat('Clean', fl.plant_clean, 'ok', 'check-circle', 'ready to fill'), stat('Filled & ready', fl.filled_ready, 'ok', 'package', 'can be sold'), stat('With customers', fl.with_customers, p.overdue_customers ? 'warn' : '', 'users', p.overdue_customers ? `${p.overdue_customers} overdue` : 'none overdue')),
    el('div.cols-split', {},
      el('section.card', {}, el('div.card-head', {}, el('h2', {}, icon('alert'), 'Fleet health')),
        ui.progress(ret || 0), el('p.small', { text: ret === null ? 'Set a maximum number of cycles on the product to track retirement.' : `Average ${f.avg_cycles_estimate} of ${f.max_cycles} cycles used (${f.retirement_pct}%).` }),
        el('dl.kv', {}, el('dt', { text: 'Destroy rate' }), el('dd', {}, f.destroy_rate_pct === null ? 'No fills in this period' : `${f.destroy_rate_pct}% of fills`), el('dt', { text: 'Warning limit' }), el('dd', { text: `${f.warn_pct}%` }), el('dt', { text: 'Lifetime' }), el('dd', { text: `${ui.fmtNum(f.lifetime.fills, 0)} fills, ${ui.fmtNum(f.lifetime.destroyed, 0)} destroyed` }))),
      el('section.card', {}, el('div.card-head', {}, el('h2', {}, icon('history'), 'Activity'), periodSeg.el),
        el('p.small.muted', { text: `${ui.fmtDate(per.from)} - ${ui.fmtDate(per.to)}` }),
        el('dl.kv', {}, [['Filled', per.fills], ['Delivered to customers', per.delivered], ['Empties returned', per.empties_returned], ['New containers bought', per.new_purchased], ['Washed', per.washed], ['Destroyed', per.destroyed_total]].flatMap(([k, v]) => [el('dt', { text: k }), el('dd', { text: ui.fmtNum(v, 0) })])))),
    el('section.card', {}, el('div.card-head', {}, el('h2', {}, icon('trash'), 'Why containers were destroyed')),
      per.destroyed_total ? el('div.bars', {}, Object.entries(per.destroyed_by_reason).filter(([, v]) => v > 0).sort((a, b) => b[1] - a[1]).map(([k, v]) => el('div.bar-row', {}, el('span.bar-label', { text: A.REASONS[k] || k }), el('div.bar', {}, el('i', { style: { width: `${(v * 100) / maxReason}%` } })), el('b', { text: String(v) })))) : el('p.muted.small', {}, icon('check-circle', 'sm'), ' Nothing destroyed in this period.')),
    ERP.godown.multiple && f.by_location && f.by_location.length > 1 ? el('section.card', {}, el('div.card-head', {}, el('h2', {}, icon('pin'), 'By godown')),
      ui.cardsTable({ columns: [{ label: 'Godown', cls: 'k', render: (r) => r.name }, { label: 'Empty', cls: 'num', render: (r) => r.plant_empty }, { label: 'Clean', cls: 'num', render: (r) => r.plant_clean }, { label: 'Filled ready', cls: 'num', render: (r) => r.filled_ready }, { label: 'In QC', cls: 'num', render: (r) => r.filled_quarantine }], rows: f.by_location })) : null);
}

/* ---------------------------------------------------------------------------------------------- customers */
function renderCustomers() {
  const p = product(); if (!p) return;
  const search = el('input.input', { type: 'search', value: state.q, placeholder: 'Search customer or phone', 'aria-label': 'Search customers', maxlength: 100, on: { input: ui.debounce(async () => { state.q = search.value.trim(); await refreshCustomerList(); }, 350) } });
  const od = el('button.chip-btn', { type: 'button', 'aria-pressed': String(state.overdue), on: { click: async () => { state.overdue = !state.overdue; od.setAttribute('aria-pressed', String(state.overdue)); await refreshCustomerList(); } } }, 'Overdue only');
  const allb = el('button.chip-btn', { type: 'button', 'aria-pressed': String(state.all), on: { click: async () => { state.all = !state.all; allb.setAttribute('aria-pressed', String(state.all)); await refreshCustomerList(); } } }, 'Include zero balance');
  const open = el('button.btn.secondary', { type: 'button', on: { click: () => A.openOpening({ product: p, done: reloadAll }) } }, icon('plus', 'sm'), 'Opening balance');
  const list = el('div#cust-list');
  ui.mount(panes.customers, el('div.toolbar', {}, el('div.searchbar', {}, icon('search', 'sm'), search), el('div.toolbar-end', {}, open)), el('div.chips', {}, od, allb), list);
  paintCustomerList(list);
}
async function refreshCustomerList() { try { await loadCustomers(); } catch (e) { explain(e); return; } const l = document.getElementById('cust-list'); if (l) paintCustomerList(l); }
function paintCustomerList(host) {
  const p = product();
  ui.mount(host, ui.cardsTable({
    columns: [
      { label: 'Customer', cls: 'k', render: (c) => [el('b', { text: c.name }), el('small', { text: c.phone || '' })] },
      { label: 'Status', cls: 's', render: (c) => (c.overdue ? ui.badge('OVERDUE', `Overdue ${c.days_outstanding} d`) : c.holding ? ui.badge('OK', 'On time') : ui.badge('INACTIVE', 'None held')) },
      { label: 'Holding', cls: 'num', render: (c) => el('b.big', { text: ui.fmtNum(c.holding, 0) }) },
      { label: 'Oldest outstanding', render: (c) => (c.holding ? `${ui.fmtDate(c.oldest_outstanding_date)} (${c.days_outstanding} d)` : '-') },
      { label: '', cls: 'a', render: (c) => el('div.btn-row', {}, el('button.btn.secondary.sm', { type: 'button', on: { click: () => openLedger(c) } }, icon('list', 'sm'), 'Ledger'), el('button.btn.secondary.sm', { type: 'button', disabled: !c.holding, on: { click: () => A.openReceiveEmpties({ product: p, state, customer: c.customer_id, holdings: holdings(), done: reloadAll }) } }, icon('upload', 'sm'), 'Receive empties')) },
    ], rows: state.customers, onRow: openLedger,
    emptyState: { icon: 'users', title: state.q || state.overdue ? 'No customers match' : 'No customer holds containers', text: 'When containers are issued or delivered, the customer shows up here. Existing balances can be entered with "Opening balance".' },
  }));
}
const TXN = { OPENING: 'Opening balance', ISSUE_FILLED: 'Filled containers given', RETURN_EMPTY: 'Empties returned', DAMAGED: 'Damaged', LOST: 'Lost', WRITE_OFF: 'Written off' };
async function openLedger(c) {
  const p = product(); const body = el('div.stack.tight', {}, ui.skeletonCards(2)); let offset = 0; const rows = [];
  const more = el('button.btn.secondary', { type: 'button', hidden: true }, 'Show older');
  const dlg = ui.openDialog({ title: c.name, body, actions: [el('button.btn.secondary', { type: 'button', on: { click: () => dlg.close() } }, 'Close'), el('button.btn.primary', { type: 'button', on: { click: () => { dlg.close(); A.openReceiveEmpties({ product: p, state, customer: c.customer_id, holdings: holdings(), done: reloadAll }); } } }, icon('upload', 'sm'), 'Receive empties')], wide: true });
  async function load() {
    try {
      const r = await api.get(`/api/jars/customers/${encodeURIComponent(c.customer_id)}/ledger`, { product_id: p.product_id, limit: 30, offset });
      rows.push(...r.items); offset += r.items.length;
      ui.mount(body, el('div.row', {}, el('b.big', { text: `Holds ${ui.fmtNum(c.holding, 0)}` }), c.overdue ? ui.badge('OVERDUE', `Overdue ${c.days_outstanding} d`) : null),
        rows.length ? ui.cardsTable({ columns: [{ label: 'Date', cls: 'k', render: (x) => [el('b', { text: ui.fmtDate(x.txn_date) }), el('small', { text: x.trip_no ? `Trip ${x.trip_no}` : x.created_by_name || '' })] }, { label: 'What', render: (x) => TXN[x.txn_type] || x.txn_type },
          { label: 'Change', cls: 'num', render: (x) => el('b', { class: x.delta < 0 ? 'neg' : 'pos', text: `${x.delta > 0 ? '+' : ''}${x.delta}` }) }, { label: 'Holds after', cls: 'num', render: (x) => x.balance_after }, { label: 'Note', cls: 'w', hideMobile: true, render: (x) => x.note }], rows })
          : ui.empty({ icon: 'history', title: 'No movements yet' }),
        offset < r.total ? el('div.row.end', {}, more) : null);
      more.hidden = false; more.onclick = load;
    } catch (e) { ui.mount(body, ui.errorState(e, load)); }
  }
  load();
}

/* ---------------------------------------------------------------------------------------------- plant actions */
function renderActions() {
  const p = product(); if (!p) return;
  const needLoc = ERP.godown.current() === 'all' || !ERP.godown.current();
  const locSel = ERP.godown.current() === 'all' && ERP.godown.list.length ? ui.field({ label: 'Do these actions in godown', name: 'wl', type: 'select', value: state.writeLoc || A.writeLocation(state), options: ERP.godown.list.map((g) => ({ value: g.id, label: g.name })) }) : null;
  if (locSel) { state.writeLoc = locSel.get(); locSel.input.addEventListener('change', () => { state.writeLoc = locSel.get(); }); }
  const tile = (ic, title, text, onClick, tone) => el('button.act-tile', { type: 'button', class: tone || '', on: { click: onClick } }, el('span.act-ico', {}, icon(ic)), el('span.act-txt', {}, el('b', { text: title }), el('span', { text })));
  const common = { product: p, state, done: reloadAll };
  void needLoc;
  ui.mount(panes.actions,
    el('div.stack.tight', {}, el('div.row', {}, icon('pin', 'sm'), el('span', {}, 'Godown: ', el('b', { text: ERP.godown.current() === 'all' ? '' : ERP.godown.currentName() || 'Main plant' }))), locSel ? locSel.el : null),
    el('div.grid.kpis', {}, stat('Empty', p.plant_empty, '', 'jar'), stat('Clean', p.plant_clean, 'ok', 'check-circle'), stat('Filled ready', p.filled_ready, 'ok', 'package'), stat('Filled in QC', p.filled_quarantine, '', 'clock')),
    el('div.act-grid', {},
      tile('download', 'Receive new containers', 'Bought from a supplier - they go to the empty store.', () => A.openReceiveNew(common)),
      tile('droplet', 'Record washing', 'Empty -> clean, with the count that failed inspection.', () => A.openWash(common)),
      tile('upload', 'Receive empties', 'A customer brings containers back.', () => A.openReceiveEmpties({ ...common, holdings: holdings() })),
      tile('jar', 'Counter issue', 'Hand filled containers to a customer at the counter.', () => A.openIssue(common)),
      tile('trash', 'Destroy containers', 'Broken, leaking, lost or too old - permanent.', () => A.openDestroy(common), 'danger')));
}

/* ---------------------------------------------------------------------------------------------- destroyed log */
async function renderDestroyed() {
  const p = product(); if (!p) return;
  if (!state.destroyed) { ui.mount(panes.destroyed, ui.skeletonCards(3)); try { await loadDestroyed(); } catch (e) { ui.mount(panes.destroyed, ui.errorState(e, renderDestroyed)); return; } }
  const sel = ui.field({ label: 'Reason', name: 'reason', type: 'select', value: state.reason, options: [{ value: '', label: 'All reasons' }, ...Object.entries(A.REASONS).map(([value, label]) => ({ value, label }))] });
  sel.input.addEventListener('change', async () => { state.reason = sel.get(); state.dOffset = 0; try { await loadDestroyed(); } catch (e) { explain(e); return; } renderDestroyed(); });
  const d = state.destroyed;
  ui.mount(panes.destroyed, el('div.toolbar', {}, el('div.grow', { style: { maxWidth: '18rem' } }, sel.el)),
    ui.cardsTable({ columns: [
      { label: 'When', cls: 'k', render: (x) => [el('b', { text: ui.fmtDateTime(x.created_at) }), el('small', { text: x.created_by_name || '' })] },
      { label: 'Reason', cls: 's', render: (x) => ui.badge('CANCELLED', A.REASONS[x.reason_code] || x.reason_code || '-', 'neutral') },
      { label: 'Containers', cls: 'num', render: (x) => el('b', { text: String(x.qty) }) },
      { label: 'Where', render: (x) => ({ PLANT_EMPTY: 'Plant - empty', PLANT_CLEAN: 'Plant - clean', CUSTOMER: x.customer_name ? `At ${x.customer_name}` : 'At customer', VEHICLE: x.trip_no ? `In vehicle (${x.trip_no})` : 'In vehicle' })[x.location] || x.location || '-' },
      { label: 'Note', cls: 'w', hideMobile: true, render: (x) => x.note },
    ], rows: d.items, emptyState: { icon: 'check-circle', title: 'Nothing destroyed', text: state.reason ? 'No records for this reason.' : 'Destroyed containers are logged here with the reason.' } }),
    d.total > 25 ? ui.pager({ total: d.total, limit: 25, offset: state.dOffset, onPage: async (o) => { state.dOffset = o; await loadDestroyed(); renderDestroyed(); } }) : null);
}

/* ---------------------------------------------------------------------------------------------- go */
ERP.godown.onChange(() => { state.destroyed = null; reloadAll(); });
await reloadAll();
const live = async () => { if (document.querySelector('dialog[open]')) return; A.clearRefCache(); state.destroyed = null; await reloadAll(); };
ERP.live.start('jars', { jars: live, stock: live, master: live });
