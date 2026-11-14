// maintenance.html - service schedules (grouped by urgency), vehicle document board, completion with confirm, history drawer.
import { boot, ERP } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { run, dueText, daysBetween, actionFor } from '/js/pages/loading-common.js';

const { el, icon } = ui;
await boot({ page: 'maintenance', title: 'Maintenance' });
const main = document.getElementById('main');

const state = { filter: 'ALL', tab: 'schedules', schedules: [], docs: [], loaded: false, lead: 30 };
const tone = (s) => (s === 'OVERDUE' ? 'bad' : s === 'DUE_SOON' ? 'warn' : 'ok');
const subjectIcon = (t) => (t === 'VEHICLE' ? 'truck' : 'wrench');
const DOCS = [['INSURANCE', 'Insurance'], ['FITNESS', 'Fitness'], ['PUC', 'Pollution (PUC)'], ['PERMIT', 'Permit']];

/* ---------------------------------------------------------------------------------------------- layout */
const head = el('div.page-head', {}, el('div', {}, el('p.sub#mt-sub', { text: 'Loading...' })), el('div.actions', {}));
const segFilter = ui.segmented({ label: 'Show', value: 'ALL', options: [{ value: 'ALL', label: 'All' }, { value: 'EQUIPMENT', label: 'Equipment' }, { value: 'VEHICLE', label: 'Vehicles' }], onChange: (v) => { state.filter = v; renderSchedules(); } });
const refreshBtn = el('button.btn.ghost.sm.icon', { type: 'button', 'aria-label': 'Refresh', on: { click: () => api.busy(refreshBtn, load) } }, icon('refresh', 'sm'));
head.querySelector('.actions').append(refreshBtn);
const tabs = ui.tabs({ label: 'Maintenance sections', tabs: [{ id: 'schedules', label: 'Schedules', icon: 'wrench' }, { id: 'docs', label: 'Vehicle docs', icon: 'clipboard' }], active: 'schedules', onChange: (id) => { state.tab = id; paintTab(); } });
const schedHost = el('div.stack.loose#mt-sched');
const docsHost = el('div.stack#mt-docs', { hidden: true });
const filterRow = el('div.row.between.mt-filter', {}, segFilter.el);
ui.mount(main, head, el('div.sticky-tabs', {}, tabs.el), filterRow, schedHost, docsHost);
ui.mount(schedHost, ui.skeletonCards(3));

function paintTab() {
  schedHost.hidden = state.tab !== 'schedules'; filterRow.hidden = state.tab !== 'schedules'; docsHost.hidden = state.tab !== 'docs';
  tabs.select(state.tab);
  history.replaceState(null, '', state.tab === 'docs' ? '#docs' : '#schedules');
}

/* ---------------------------------------------------------------------------------------------- data */
async function load() {
  try {
    const [s, d] = await Promise.all([api.get('/api/maintenance/schedules'), api.get('/api/maintenance/vehicle-docs')]);
    state.schedules = s || []; state.docs = d || []; state.loaded = true;
    state.lead = Number(ERP.ui.config.settings.expiry_lead_days) || 30;
    renderSchedules(); renderDocs(); summary();
  } catch (e) {
    if (e.redirecting) return;
    if (state.loaded && e.network) { ui.toast.warn('Showing the last data - server unreachable', { ms: 3000 }); return; }
    ui.mount(schedHost, ui.errorState(e, load));
  }
}
function summary() {
  const over = state.schedules.filter((x) => x.status === 'OVERDUE').length, soon = state.schedules.filter((x) => x.status === 'DUE_SOON').length;
  const docBad = state.docs.filter((x) => x.status !== 'OK').length;
  document.getElementById('mt-sub').textContent = over || soon || docBad
    ? [over ? `${over} overdue` : '', soon ? `${soon} due soon` : '', docBad ? `${docBad} vehicle document${docBad === 1 ? ' needs' : 's need'} attention` : ''].filter(Boolean).join(' - ')
    : 'Everything is on track.';
}

/* ---------------------------------------------------------------------------------------------- schedules */
function scheduleCard(s) {
  const t = tone(s.status);
  const when = s.status === 'OVERDUE' ? `Overdue by ${-s.days_left} day${s.days_left === -1 ? '' : 's'}` : s.status === 'DUE_SOON' ? (s.days_left === 0 ? 'Due today' : `Due in ${s.days_left} day${s.days_left === 1 ? '' : 's'}`) : `Due in ${s.days_left} days`;
  return el('article.mt-card', { class: t, dataset: { id: s.id } },
    el('div.mt-main', {},
      el('div.mt-title', {}, el('b', { text: s.task }), ui.badge(s.status, { OVERDUE: 'Overdue', DUE_SOON: 'Due soon', OK: 'On track' }[s.status])),
      el('div.mt-subject', {}, icon(subjectIcon(s.subject_type), 'sm'), s.subject_name),
      el('div.mt-meta', {}, el('span', {}, el('b', { class: `due ${t}`, text: when }), ` - ${ui.fmtDate(s.next_due_on)}`),
        el('span.muted', { text: s.last_done_on ? `Last done ${ui.fmtDate(s.last_done_on)}${s.interval_days ? `, every ${s.interval_days} days` : ''}` : (s.interval_days ? `Every ${s.interval_days} days, never done yet` : 'Never done yet') }))),
    el('div.mt-actions', {},
      el('button.btn.sm', { type: 'button', class: s.status === 'OK' ? 'secondary' : 'primary', on: { click: () => openComplete(s) } }, icon('check-circle', 'sm'), 'Mark done'),
      el('button.btn.ghost.sm', { type: 'button', on: { click: () => openLogs(s) } }, icon('history', 'sm'), 'History')));
}
function renderSchedules() {
  const rows = state.schedules.filter((s) => state.filter === 'ALL' || s.subject_type === state.filter);
  if (!state.schedules.length) { ui.mount(schedHost, ui.empty({ icon: 'wrench', title: 'No service schedules yet', text: 'An administrator can add schedules for equipment and vehicles under Administration -> Master data.' })); return; }
  if (!rows.length) { ui.mount(schedHost, ui.empty({ icon: 'check-circle', title: `No ${state.filter === 'VEHICLE' ? 'vehicle' : 'equipment'} schedules`, text: 'Switch the filter to see everything.', action: { label: 'Show all', onClick: () => { state.filter = 'ALL'; segFilter.set('ALL'); renderSchedules(); } } })); return; }
  const groups = [['OVERDUE', 'Overdue', 'alert'], ['DUE_SOON', 'Due soon', 'clock'], ['OK', 'On track', 'check-circle']];
  const out = groups.map(([key, label, ic]) => {
    const g = rows.filter((r) => r.status === key); if (!g.length) return null;
    return el('section.mt-group', { class: tone(key) }, el('h2.mt-group-title', {}, icon(ic), label, el('span.count-pill', { text: String(g.length) })), el('div.mt-list', {}, g.map(scheduleCard)));
  });
  ui.mount(schedHost, out);
}

/* ---------------------------------------------------------------------------------------------- vehicle documents */
function renderDocs() {
  const byVeh = new Map();
  state.docs.forEach((d) => { if (!byVeh.has(d.vehicle_id)) byVeh.set(d.vehicle_id, { reg: d.reg_no, id: d.vehicle_id, docs: {} }); byVeh.get(d.vehicle_id).docs[d.doc] = d; });
  if (!byVeh.size) { ui.mount(docsHost, ui.empty({ icon: 'truck', title: 'No vehicle documents tracked', text: 'Add insurance, fitness, PUC and permit expiry dates to a vehicle (Administration -> Master data) and they appear here.' })); return; }
  const worst = (v) => Math.min(...Object.values(v.docs).map((d) => d.days_left));
  const vehicles = [...byVeh.values()].sort((a, b) => worst(a) - worst(b) || a.reg.localeCompare(b.reg));
  ui.mount(docsHost, el('div.grid.auto', {}, vehicles.map((v) => {
    const w = worst(v); const t = w < 0 ? 'bad' : w <= state.lead ? 'warn' : 'ok';
    return el('article.card.doc-card', { class: t },
      el('div.card-head', {}, el('h3', {}, icon('truck'), v.reg), ui.badge(w < 0 ? 'OVERDUE' : w <= state.lead ? 'DUE_SOON' : 'OK', w < 0 ? 'Expired' : w <= state.lead ? 'Expiring' : 'All valid')),
      el('div.doc-grid', {}, DOCS.map(([k, label]) => docTile(v.docs[k], label))));
  })));
}
function docTile(d, label) {
  if (!d) return el('div.doc-tile.none', {}, el('div.doc-name', { text: label }), el('div.doc-state.muted', {}, icon('info', 'sm'), 'Not tracked'));
  const t = tone(d.status);
  const text = d.expired ? `Expired ${-d.days_left} day${d.days_left === -1 ? '' : 's'} ago` : d.days_left === 0 ? 'Expires today' : d.status === 'DUE_SOON' ? `Expires in ${d.days_left} day${d.days_left === 1 ? '' : 's'}` : 'Valid';
  return el('div.doc-tile', { class: t }, el('div.doc-name', { text: label }),
    el('div.doc-state', { class: t }, icon(d.expired ? 'ban' : d.status === 'DUE_SOON' ? 'clock' : 'check-circle', 'sm'), text),
    el('div.doc-date', { text: `until ${ui.fmtDate(d.expiry_date)}` }));
}

/* ---------------------------------------------------------------------------------------------- complete a task */
function openComplete(s) {
  const today = ui.todayPlant();
  const act = actionFor(`mt:${s.id}`);
  const nextFor = (done) => (s.interval_days && done ? ui.addDays(done, s.interval_days) : '');
  const done = ui.field({ label: 'Done on', name: 'done_on', type: 'date', value: today, required: true, help: 'Cannot be in the future.' });
  done.input.max = today;
  const next = ui.field({ label: 'Next due', name: 'next_due_on', type: 'date', value: nextFor(today), required: !s.interval_days,
    help: s.interval_days ? `Filled in for you: done date + ${s.interval_days} days. Change it if the next service is planned for another day.` : 'This task has no fixed interval - please choose the next due date.' });
  const notes = ui.field({ label: 'Notes', name: 'notes', type: 'textarea', optional: true, maxlength: 500, placeholder: 'What was done, parts replaced, who did it...' });
  const form = el('form.stack', { id: 'mt-complete', novalidate: true },
    el('div.form-error', { role: 'alert' }),
    el('div.mt-subject', {}, icon(subjectIcon(s.subject_type)), el('b', { text: `${s.task} - ${s.subject_name}` })),
    el('p.small.muted', { text: `Currently due ${ui.fmtDate(s.next_due_on)} (${dueText(s.days_left)}).` }),
    done.el, next.el, notes.el);
  let nextTouched = false;
  next.input.addEventListener('input', () => { nextTouched = true; });
  done.input.addEventListener('change', () => { if (!nextTouched && s.interval_days) next.set(nextFor(done.get())); });
  const draft = ERP.drafts.bind(form, { page: 'maintenance', id: `complete-${s.id}` });
  const submit = el('button.btn.primary', { type: 'submit', form: 'mt-complete' }, el('span.btn-icon', {}, icon('check-circle')), 'Mark as done');
  const cancel = el('button.btn.secondary', { type: 'button', on: { click: () => dlg.close() } }, 'Cancel');
  const dlg = ui.openDialog({ title: 'Mark task as done', body: form, actions: [cancel, submit] });
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    const v = ui.readForm(form);
    let bad = false;
    if (!v.done_on) { done.setError('Choose the date it was done'); bad = true; } else if (v.done_on > today) { done.setError('The date cannot be in the future'); bad = true; }
    if (!v.next_due_on && !s.interval_days) { next.setError('Choose the next due date'); bad = true; }
    if (v.next_due_on && v.done_on && v.next_due_on <= v.done_on) { next.setError('Must be after the date it was done'); bad = true; }
    if (bad) return;
    const nd = v.next_due_on || nextFor(v.done_on);
    const ok = await ui.confirmDialog({ title: 'Record this as done?', message: `${s.task} on ${s.subject_name}`, warnings: [`Done on ${ui.fmtDate(v.done_on)}`, `Next due ${ui.fmtDate(nd)} (${daysBetween(v.done_on, nd)} days later)`], confirmText: 'Yes, record it', cancelText: 'Back' });
    if (!ok) return;
    const body = { done_on: v.done_on, notes: v.notes || '' };
    if (v.next_due_on) body.next_due_on = v.next_due_on;
    const r = await run(submit, () => act.post(`/api/maintenance/schedules/${encodeURIComponent(s.id)}/complete`, body), { form, refresh: load });
    if (r) { draft.clear(); dlg.close(true); ui.toast.ok(`${s.task} recorded`, { sub: `Next due ${ui.fmtDate(r.next_due_on)}` }); load(); }
  });
}

/* ---------------------------------------------------------------------------------------------- history drawer */
async function openLogs(s) {
  const body = el('div.stack.tight', {}, ui.skeletonCards(2));
  ui.openDialog({ title: `History - ${s.task}`, body, actions: [] });
  try {
    const logs = await api.get(`/api/maintenance/schedules/${encodeURIComponent(s.id)}/logs`);
    ui.mount(body, logs.length ? el('ul.list', {}, logs.map((l) => el('li', {}, el('div.grow', {}, el('b', { text: ui.fmtDate(l.done_on) }), el('span.small.muted', { text: [l.done_by_name ? `by ${l.done_by_name}` : '', l.next_due_on ? `next due ${ui.fmtDate(l.next_due_on)}` : ''].filter(Boolean).join(' - ') }), l.notes ? el('span.small', { text: l.notes }) : null)))) : ui.empty({ icon: 'history', title: 'Nothing recorded yet', text: 'When this task is marked done it is logged here.' }));
  } catch (e) { ui.mount(body, ui.errorState(e)); }
}

/* ---------------------------------------------------------------------------------------------- go */
const h = location.hash.slice(1); if (h === 'docs') { state.tab = 'docs'; }
paintTab();
await load();
ERP.live.start('maintenance', { maintenance: load });
