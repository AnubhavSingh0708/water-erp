// loading.html - floor screen: trips -> load -> gate pass -> settle, dispatch notes (walk-in sales), gate-pass print.
// Router = location.hash:  (none)/#trips  #dispatch  #trip=<id>  #settle=<id>  #gp=<id>  #dn=<id>  #dn-new
// This file: shell, router, trips list, new-trip dialog. Views live in loading-trips / -settle / -gate / -dispatch / -scan.
import { boot, ERP } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { run, explain, capacityBar, daysBetween, getVehicles, clearVehicles, actionFor } from '/js/pages/loading-common.js';
import { mountTrip } from '/js/pages/loading-trips.js';
import { mountSettle } from '/js/pages/loading-settle.js';
import { mountGatePass } from '/js/pages/loading-gate.js';
import { mountDispatchList, mountDispatchNew, mountDispatchDoc } from '/js/pages/loading-dispatch.js';

const { el, icon } = ui;
await boot({ page: 'loading', title: 'Loading & dispatch', godownAll: false, live: false });
const main = document.getElementById('main');

/* ---------------------------------------------------------------------------------------------- router */
const params = new URLSearchParams(location.search);
const ctx = {
  qrOn: () => !!ui.config.qrLabels,
  pendingQr: null,
  go(hash) { if (location.hash === '#' + hash || (!hash && !location.hash)) route(); else location.hash = hash; },
  back() { history.length > 1 && document.referrer.includes(location.host) ? history.back() : ctx.go('trips'); },
  openTrip: (id) => ctx.go(`trip=${id}`),
};
if (params.get('trip')) history.replaceState(null, '', `${location.pathname}#trip=${encodeURIComponent(params.get('trip'))}`);
if (params.get('qr')) { ctx.pendingQr = params.get('qr').trim(); history.replaceState(null, '', `${location.pathname}${location.hash}`); }

const tabs = ui.tabs({ label: 'Loading sections', tabs: [{ id: 'trips', label: 'Trips', icon: 'truck' }, { id: 'dispatch', label: 'Direct sales', icon: 'clipboard' }], active: 'trips', onChange: (id) => ctx.go(id) });
const tabBar = el('div.sticky-tabs', {}, tabs.el);
const view = el('div.ld-view');
ui.mount(main, tabBar, view);

let current = null; let seq = 0;
function parse() {
  const h = decodeURIComponent((location.hash || '').slice(1));
  const [k, v] = h.split('=');
  if (['trip', 'settle', 'gp', 'dn'].includes(k) && v) return { name: k, id: v };
  if (h === 'dn-new') return { name: 'dn-new' };
  if (h === 'dispatch') return { name: 'dispatch' };
  return { name: 'trips' };
}
async function route() {
  const r = parse(); const mine = ++seq;
  if (current && current.destroy) { try { current.destroy(); } catch { /* ignore */ } }
  current = null;
  const list = r.name === 'trips' || r.name === 'dispatch';
  tabBar.hidden = !list; if (list) tabs.select(r.name);
  ui.mount(view, ui.skeletonCards(3));
  const host = el('div.stack.loose');
  const mounts = {
    trips: () => mountTrips(host, ctx), dispatch: () => mountDispatchList(host, ctx),
    trip: () => mountTrip(host, { ...r, ctx }), settle: () => mountSettle(host, { ...r, ctx }), gp: () => mountGatePass(host, { ...r, ctx, kind: 'trip' }),
    dn: () => mountDispatchDoc(host, { ...r, ctx }), 'dn-new': () => mountDispatchNew(host, { ctx }),
  };
  try {
    const inst = await mounts[r.name]();
    if (mine !== seq) { if (inst && inst.destroy) inst.destroy(); return; }
    current = inst; ui.mount(view, host);
    window.scrollTo({ top: 0 });
  } catch (e) { if (mine === seq && !(e && e.redirecting)) ui.mount(view, ui.errorState(e, route)); }
}
window.addEventListener('hashchange', route);
ERP.godown.onChange(() => { if (['trips', 'dispatch'].includes(parse().name)) route(); });

/* ---------------------------------------------------------------------------------------------- trips list */
const FILTERS = [{ value: 'OPEN', label: 'Open' }, { value: 'SETTLED', label: 'Settled' }, { value: 'CANCELLED', label: 'Cancelled' }];
async function mountTrips(host, ctx) {
  let filter = sessionStorage.getItem('ld.filter') || 'OPEN'; let offset = 0; let dead = false;
  const statsEl = el('div.ld-stats', { 'aria-live': 'polite' });
  const listEl = el('div.stack');
  const seg = ui.segmented({ label: 'Trip status', value: filter, options: FILTERS, onChange: (v) => { filter = v; offset = 0; try { sessionStorage.setItem('ld.filter', v); } catch { /* ignore */ } paint(true); } });
  const newBtn = el('button.btn.primary', { type: 'button', on: { click: () => openNewTrip(ctx) } }, el('span.btn-icon', {}, icon('plus')), 'New trip');
  const g = ERP.godown;
  const sub = g.currentName() ? `Loading from ${g.currentName()}` : 'Loading trips';
  const qrBanner = ctx.pendingQr && ctx.qrOn() ? el('div.banner.info.inline', {}, icon('qr'), el('div.grow', {}, el('b', { text: `Label ${ctx.pendingQr} scanned` }), el('div.small', { style: { fontWeight: 500 }, text: 'Open the trip you are loading to add it.' })),
    el('button.btn.ghost.sm', { type: 'button', on: { click: () => { ctx.pendingQr = null; qrBanner.remove(); } } }, 'Dismiss')) : null;
  ui.append(host, [el('div.page-head', {}, el('div', {}, el('p.sub', { text: sub })), el('div.actions', {}, newBtn)), qrBanner, statsEl, seg.el, listEl]);

  async function paintStats() {
    try {
      const s = await api.get('/api/loading/summary', { ...g.param() });
      ui.mount(statsEl,
        stat('Loading now', s.open_trips, s.open_trips ? 'warn' : ''), stat('On the road', s.on_road, s.on_road ? 'info' : ''),
        stat('Settled today', s.settled_today, 'ok'), stat('Crates out today', ui.fmtNum(Number(s.crates_dispatched_today) + Number(s.crates_direct_today), 0), ''));
    } catch (e) { if (!e.redirecting) ui.clear(statsEl); }
  }
  const stat = (label, v, tone) => el('div.ld-stat', { class: tone }, el('b', { text: String(v) }), el('span', { text: label }));

  async function fetchTrips() {
    const q = { ...g.param(), limit: 50, offset };
    if (filter === 'OPEN') {
      const [a, b] = await Promise.all([api.get('/api/loading/trips', { ...q, status: 'LOADING' }), api.get('/api/loading/trips', { ...q, status: 'DISPATCHED' })]);
      return { items: [...a.items, ...b.items].sort((x, y) => (x.status === y.status ? y.created_at.localeCompare(x.created_at) : x.status === 'LOADING' ? -1 : 1)), total: a.total + b.total };
    }
    return api.get('/api/loading/trips', { ...q, status: filter });
  }
  async function paint(skeleton) {
    if (skeleton) ui.mount(listEl, ui.skeletonCards(3));
    try {
      const { items, total } = await fetchTrips();
      if (dead) return;
      if (!items.length) {
        ui.mount(listEl, ui.empty({ icon: 'truck', title: filter === 'OPEN' ? 'No trips in progress' : filter === 'SETTLED' ? 'No settled trips yet' : 'Nothing cancelled', text: filter === 'OPEN' ? 'Start a trip when a vehicle arrives at the dock.' : '', action: filter === 'OPEN' ? { label: 'New trip', icon: 'plus', onClick: () => openNewTrip(ctx) } : null }));
      } else {
        ui.mount(listEl, el('div.ld-grid', {}, items.map((t) => tripCard(t, ctx))), filter === 'OPEN' ? null : ui.pager({ total, limit: 50, offset, onPage: (o) => { offset = o; paint(true); } }));
      }
    } catch (e) { if (!e.redirecting && !dead) { if (e.network && listEl.querySelector('.trip-card')) ui.toast.warn('Showing the last data - server unreachable', { ms: 3000 }); else ui.mount(listEl, ui.errorState(e, () => paint(true))); } }
  }
  await Promise.all([paint(false), paintStats()]);
  return { refresh: () => Promise.all([paint(false), paintStats()]), destroy() { dead = true; } };
}

function tripCard(t, ctx) {
  const when = t.status === 'DISPATCHED' ? `Left ${ui.fmtRelative(t.gate_pass_issued_at)}` : t.status === 'SETTLED' ? `Settled ${ui.fmtRelative(t.settled_at)}` : t.status === 'CANCELLED' ? (t.voided_at ? `Voided ${ui.fmtRelative(t.voided_at)}` : 'Cancelled') : `Started ${ui.fmtRelative(t.created_at)}`;
  const open = () => ctx.go(t.status === 'DISPATCHED' ? `trip=${t.id}` : `trip=${t.id}`);
  const primary = t.status === 'LOADING' ? el('button.btn.sm.primary', { type: 'button', on: { click: (e) => { e.stopPropagation(); open(); } } }, icon('truck', 'sm'), 'Continue loading')
    : t.status === 'DISPATCHED' ? el('button.btn.sm.primary', { type: 'button', on: { click: (e) => { e.stopPropagation(); ctx.go(`settle=${t.id}`); } } }, icon('check-circle', 'sm'), 'Settle trip') : null;
  return el('article.trip-card', { class: t.status.toLowerCase(), tabindex: 0, role: 'link', 'aria-label': `Trip ${t.trip_no} ${t.vehicle_reg}`, on: { click: open, keydown: (e) => { if (e.key === 'Enter') open(); } } },
    el('div.tc-top', {}, el('div.grow', {}, el('b.tc-veh', { text: t.vehicle_reg }), el('div.small.muted', { text: [t.trip_no, t.route_name, t.driver_name].filter(Boolean).join(' - ') })), ui.badge(t.status, t.status === 'DISPATCHED' ? 'On the road' : undefined)),
    t.status === 'CANCELLED' ? el('p.small.muted', { text: t.void_reason ? `Voided: ${t.void_reason}` : 'This trip was cancelled.' }) : capacityBar(t.loaded_crates, t.capacity_crates),
    el('div.tc-foot', {}, el('span.small.muted', {}, icon('clock', 'sm'), ' ' + when), primary));
}

/* ---------------------------------------------------------------------------------------------- new trip */
async function openNewTrip(ctx) {
  let vlist, routes;
  try { [vlist, routes] = await Promise.all([getVehicles(true), api.get('/api/ref/routes')]); } catch (e) { explain(e); return; }
  if (!vlist.length) { ui.toast.warn('No vehicles are set up yet', { sub: 'An administrator can add vehicles under Administration -> Master data.' }); return; }
  const today = ui.todayPlant();
  const lead = Number(ui.config.settings.expiry_lead_days) || 30;
  const g = ERP.godown;
  const vf = ui.field({ label: 'Vehicle', name: 'vehicle_id', type: 'select', required: true, options: [{ value: '', label: 'Choose a vehicle...' }, ...vlist.map((v) => ({ value: v.id, label: `${v.reg_no} - ${v.name} (${v.capacity_crates} crates)` }))] });
  const rf = ui.field({ label: 'Route', name: 'route_id', type: 'select', optional: true, options: [{ value: '', label: 'No fixed route' }, ...routes.map((r) => ({ value: r.id, label: r.name }))] });
  const df = ui.field({ label: 'Driver', name: 'driver_name', type: 'text', optional: true, maxlength: 120, placeholder: 'Driver name', help: 'Filled from the vehicle - change it if someone else is driving.' });
  const nf = ui.field({ label: 'Notes', name: 'notes', type: 'textarea', optional: true, maxlength: 500, rows: 2 });
  const info = el('div.stack.tight.veh-info', { 'aria-live': 'polite' });
  let driverTouched = false;
  df.input.addEventListener('input', () => { driverTouched = true; });
  const docs = (v) => [['insurance_expiry', 'Insurance'], ['fitness_expiry', 'Fitness'], ['puc_expiry', 'PUC'], ['permit_expiry', 'Permit']].filter(([k]) => v[k]).map(([k, l]) => ({ l, left: daysBetween(today, v[k]), date: v[k] }));
  const paintInfo = () => {
    const v = vlist.find((x) => x.id === vf.get()); ui.clear(info); if (!v) return;
    if (!driverTouched && !df.get()) df.set(v.driver_name || '');
    const d = docs(v); const bad = d.filter((x) => x.left < 0);
    info.append(el('div.chips', {}, d.map((x) => ui.badge(x.left < 0 ? 'OVERDUE' : x.left <= lead ? 'DUE_SOON' : 'OK', `${x.l}: ${x.left < 0 ? 'expired' : x.left <= lead ? `${x.left} d left` : 'valid'}`))));
    if (bad.length) info.append(el('div.banner.err.inline', {}, icon('ban'), el('div.grow', {}, el('b', { text: `${bad.map((x) => x.l).join(', ')} expired` }), el('div.small', { style: { fontWeight: 500 }, text: 'You can load, but the gate pass will be blocked until the document is renewed (Maintenance).' }))));
  };
  vf.input.addEventListener('change', paintInfo);
  const form = el('form.stack', { id: 'new-trip', novalidate: true }, el('div.form-error', { role: 'alert' }),
    g.currentName() ? el('div.row', {}, icon('pin', 'sm'), el('span.small', {}, 'Loading from ', el('b', { text: g.currentName() }))) : null, vf.el, info, rf.el, df.el, nf.el);
  const draft = ERP.drafts.bind(form, { page: 'loading', id: 'new-trip' });
  paintInfo();
  const act = actionFor('new-trip');
  const submit = el('button.btn.primary', { type: 'submit', form: 'new-trip' }, el('span.btn-icon', {}, icon('truck')), 'Start trip');
  const cancel = el('button.btn.secondary', { type: 'button', on: { click: () => dlg.close() } }, 'Cancel');
  const dlg = ui.openDialog({ title: 'New loading trip', body: form, actions: [cancel, submit] });
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    const v = ui.readForm(form);
    if (!v.vehicle_id) { vf.setError('Choose the vehicle that is being loaded'); vf.input.focus(); return; }
    const body = { vehicle_id: v.vehicle_id, ...g.param() };
    if (v.route_id) body.route_id = v.route_id; if (v.driver_name.trim()) body.driver_name = v.driver_name.trim(); if (v.notes.trim()) body.notes = v.notes.trim();
    if (body.location_id === 'all') delete body.location_id;
    const t = await run(submit, () => act.postConfirm('/api/loading/trips', body, { confirmTitle: 'This vehicle may still be on the road' }), { form, openTrip: (id) => { dlg.close(); ctx.openTrip(id); } });
    if (t) { draft.clear(); dlg.close(true); ui.toast.ok(`Trip ${t.trip_no} started`, { sub: 'Now add the stock to load.' }); ctx.go(`trip=${t.id}`); }
  });
}

/* ---------------------------------------------------------------------------------------------- live + go */
const refreshCurrent = () => (current && current.refresh ? current.refresh() : null);
await route();
ERP.live.start('loading', { qr: refreshCurrent, trips: refreshCurrent, qc: refreshCurrent, stock: refreshCurrent, master: () => { clearVehicles(); return refreshCurrent(); } });
