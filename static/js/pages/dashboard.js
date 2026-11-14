// dashboard.html - KPI tiles + lists from GET /api/dashboard; refreshes live (section 15 topics).
import { boot, ERP } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { isAllowed, PAGES } from '/js/nav.js';

const { el, icon } = ui;
const { me } = await boot({ page: 'dashboard', title: 'Dashboard', godownAll: true });
const main = document.getElementById('main');

const can = (key) => isAllowed(ERP.me, PAGES.find((p) => p.key === key));
const num = (v) => (v === null || v === undefined ? null : Number(v));

const KPIS = [
  { key: 'batches_in_quarantine', label: 'Waiting for QC', icon: 'flask', href: '/qc.html', page: 'qc', tone: (n) => (n > 0 ? 'warn' : 'ok'), sub: (n) => (n === 1 ? 'batch in quarantine' : 'batches in quarantine') },
  { key: 'low_stock_count', label: 'Low stock', icon: 'alert', href: '/raw-materials.html', page: 'raw-materials', tone: (n) => (n > 0 ? 'warn' : 'ok'), sub: () => 'items below minimum' },
  { key: 'open_reorders', label: 'Open reorders', icon: 'clipboard', href: '/raw-materials.html#reorders', page: 'raw-materials', tone: () => '', sub: () => 'suggested purchases' },
  { key: 'fg_ready_crates', label: 'Finished stock', icon: 'warehouse', href: '/warehouse.html', page: 'warehouse', tone: () => 'ok', fmt: (v) => ui.fmtNum(v, 0), sub: () => 'crates ready to load' },
  { key: 'trips_on_road', label: 'Trucks on road', icon: 'truck', href: '/loading.html', page: 'loading', tone: () => '', sub: () => 'dispatched, not settled' },
  { key: 'in_transit_transfers', label: 'Godown transfers', icon: 'transfer', href: '/godowns.html', page: 'godowns', tone: (n) => (n > 0 ? 'warn' : ''), sub: () => 'in transit' },
  { key: 'jars_overdue', label: 'Jars overdue', icon: 'jar', href: '/jars.html', page: 'jars', tone: (n) => (n > 0 ? 'bad' : 'ok'), sub: () => 'customers to chase' },
  { key: 'maintenance_due', label: 'Maintenance due', icon: 'wrench', href: '/maintenance.html', page: 'maintenance', tone: (n) => (n > 0 ? 'warn' : 'ok'), sub: () => 'tasks due or overdue' },
  { key: 'water_log_overdue', label: 'Hourly water test', icon: 'droplet', href: '/production.html#water', page: 'production', bool: true, tone: (v) => (v ? 'bad' : 'ok'), fmt: (v) => (v ? 'DUE' : 'OK'), sub: (v) => (v ? 'log a reading now' : 'logged within the hour') },
];

const kpiTile = (d, k) => {
  const raw = k.bool ? !!d.kpis[d.key] : num(d.kpis[d.key]);
  const tone = k.tone(raw);
  const inner = [el('div.k-label', {}, icon(k.icon, 'sm'), k.label), el('div.k-value', { text: k.fmt ? k.fmt(d.kpis[d.key]) : ui.fmtNum(raw, 0) }), el('div.k-sub', { text: k.sub(raw) })];
  return can(k.page) ? el('a.kpi', { class: tone, href: k.href }, inner) : el('div.kpi', { class: tone }, inner);
};

const section = (title, ic, body, { href, linkLabel } = {}) => el('section.card.pad0', {}, el('div.card-head', {}, el('h2', {}, icon(ic), title), href ? el('a.btn.ghost.sm', { href }, linkLabel || 'Open', icon('chevron-right', 'sm')) : null), el('div', { style: { padding: '0 var(--s4) var(--s3)' } }, body));
const listOf = (items, row, emptyText, ic) => (items && items.length ? el('ul.list', {}, items.map(row)) : el('p.muted.small', { style: { padding: '12px 0 8px' } }, icon(ic || 'check-circle', 'sm'), ' ', emptyText));
const link = (href, ...kids) => el('a.row-link', { href }, ...kids);

function render(d) {
  const tiles = KPIS.filter((k) => d.kpis && d.kpis[k.key] !== null && d.kpis[k.key] !== undefined).map((k) => kpiTile({ ...d, key: k.key }, k));
  const cards = [];
  if (can('qc') || can('production')) cards.push(section('Waiting for QC', 'flask', listOf(d.pending_qc, (b) => el('li', {}, link(`/qc.html?batch=${encodeURIComponent(b.id)}`,
    el('div.grow', {}, el('b', { text: b.batch_no }), el('span.small.muted', { text: `${b.product_name} - ${ui.fmtNum(b.crates, 0)} crates` })),
    el('div.right', {}, ui.badge('QUARANTINE'), el('div.small.muted', { text: `waiting ${ui.fmtMinutes(b.age_minutes)}` })))), 'Nothing waiting - all batches are decided.'), { href: '/qc.html', linkLabel: 'QC queue' }));
  if (can('raw-materials')) cards.push(section('Low raw materials', 'box', listOf(d.low_stock, (m) => el('li', {}, link('/raw-materials.html',
    el('div.grow', {}, el('b', { text: m.name }), el('span.small.muted', { text: `${ui.fmtQty(m.stock, m.unit)} of ${ui.fmtQty(m.min_stock, m.unit)} minimum${m.days_cover ? ` - ${ui.fmtNum(m.days_cover, 1)} days left` : ''}` })), ui.badge(m.status))), 'All materials are above their minimum.'), { href: '/raw-materials.html', linkLabel: 'Materials' }));
  if (can('loading')) cards.push(section('Trips in progress', 'truck', listOf(d.active_trips, (t) => {
    const pct = t.capacity_crates ? (Number(t.loaded_crates) / Number(t.capacity_crates)) * 100 : 0;
    return el('li', {}, link(`/loading.html?trip=${encodeURIComponent(t.id)}`, el('div.grow', {}, el('div.row.between', { style: { flexWrap: 'nowrap' } }, el('b', { text: t.vehicle_reg }), ui.badge(t.status)),
      el('span.small.muted', { text: [t.trip_no, t.route_name, t.driver_name].filter(Boolean).join(' - ') }), el('div', { style: { marginTop: '6px' } }, ui.progress(pct), el('span.small.muted', { text: `${ui.fmtNum(t.loaded_crates, 0)} / ${ui.fmtNum(t.capacity_crates, 0)} crates` })))));
  }, 'No open trips.', 'truck'), { href: '/loading.html', linkLabel: 'Loading' }));
  if (can('warehouse') && d.in_transit_warnings && d.in_transit_warnings.length) cards.push(section('Transfers taking long', 'transfer', listOf(d.in_transit_warnings, (t) => el('li', {}, link('/godowns.html',
    el('div.grow', {}, el('b', { text: t.tf_no }), el('span.small.muted', { text: `in transit for ${ui.fmtMinutes(Number(t.hours_in_transit) * 60)}` })), ui.badge('OVERDUE', 'Over 48 h'))), ''), { href: '/godowns.html', linkLabel: 'Transfers' }));
  if (ERP.godown.multiple && d.by_location && d.by_location.length > 1) cards.push(section('By godown', 'pin', listOf(d.by_location, (g) => el('li', {}, el('div.grow', {}, el('b', { text: g.name }), el('span.small.muted', { text: g.code })),
    el('div.right', {}, el('b', { text: `${ui.fmtNum(g.fg_ready_crates, 0)} crates` }), Number(g.low_stock_items) ? el('div', {}, ui.badge('LOW', `${g.low_stock_items} low`)) : null)), '')));
  cards.push(section('Recent alerts', 'bell', listOf(d.recent_alerts, (a) => {
    const tone = ui.SEVERITY_TONE[String(a.severity).toUpperCase()] || 'info';
    const u = ui.safeNext(a.url || '', '');
    const body = [el('span.badge.plain', { class: tone }, icon(tone === 'info' ? 'info' : 'alert')), el('div.grow', {}, el('b', { text: a.title }), a.body ? el('span.small.muted', { text: a.body }) : null, el('span.small.muted', { text: ui.fmtRelative(a.created_at) + (a.resolved ? ' - resolved' : '') }))];
    return el('li', {}, u ? link(u, ...body) : el('div.row.nowrap.grow', {}, ...body));
  }, 'No alerts. All quiet.', 'check-circle')));
  return el('div.stack.loose', {}, tiles.length ? el('div.grid.kpis', {}, tiles) : null, el('div.cols-split', {}, cards));
}

const body = el('div'); const stamp = el('span.small.muted', { 'aria-live': 'polite' });
const refreshBtn = el('button.btn.secondary.sm', { type: 'button' }, icon('refresh', 'sm'), 'Refresh');
ui.mount(main, el('div.page-head', {}, el('div', {}, el('p.sub', { id: 'scope' }), stamp), el('div.actions', {}, refreshBtn)), body);
const scope = () => { document.getElementById('scope').textContent = ERP.godown.multiple ? `Showing: ${ERP.godown.currentName()}` : ERP.godown.currentName() ? `Godown: ${ERP.godown.currentName()}` : ''; };

let first = true;
async function load() {
  scope();
  if (first) ui.mount(body, ui.skeletonCards(4));
  try {
    const d = await api.get('/api/dashboard', { ...ERP.godown.param() });
    ui.mount(body, render(d)); first = false;
    stamp.textContent = `Updated ${ui.fmtTime(new Date().toISOString())}`;
  } catch (e) {
    if (e.redirecting) return;
    if (!first && e.network) { stamp.textContent = 'Showing the last data - server unreachable'; return; }   // keep what is on screen
    ui.mount(body, e.status === 404 ? ui.empty({ icon: 'clock', title: 'Dashboard is not ready yet', text: 'The dashboard service is not available on this server yet. Use the menu to open a page directly.', action: { label: 'Try again', icon: 'refresh', onClick: load } }) : ui.errorState(e, load));
  }
}
refreshBtn.addEventListener('click', () => api.busy(refreshBtn, load));
ERP.godown.onChange(load);
await load();
ERP.live.start('dashboard', { '*': load, alerts: load });
