// production.html - Water log | Blow molding | Batches.
import { boot, ERP } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import * as L from '/js/pages/warehouse-lib.js';
import { mountBlow } from '/js/pages/production-blow.js';
import { mountBatches } from '/js/pages/production-batches.js';

const { el, icon } = ui;
await boot({ page: 'production', title: 'Production', godownAll: false });
const main = document.getElementById('main');

const refreshers = {};
const ctx = { refreshers, site: () => L.writeLocation({ production: true }), goto: (id, arg) => { tabs.select(id); if (arg && refreshers[`${id}:open`]) refreshers[`${id}:open`](arg); } };

/* ---------------------------------------------------------------------------------------------- Water log */
const LIM = () => { const s = L.settings(); return { tdsMin: s.tds_min ?? '50', tdsMax: s.tds_max ?? '500', phMin: s.ph_min ?? '6.5', phMax: s.ph_max ?? '8.5' }; };

function waterStatusBanner(s) {
  if (!s.last_logged_at) return L.banner('bad', 'alert', el('b', { text: 'No water reading yet. ' }), 'Log the first reading before producing.');
  const mins = Number(s.minutes_since);
  const when = `${mins < 1 ? 'just now' : `${ui.fmtMinutes(mins)} ago`} (${ui.fmtTime(s.last_logged_at)})`;
  if (s.overdue) return L.banner('bad', 'clock', el('b', { text: 'Hourly water test is due. ' }), `Last log was ${when}. Log a reading now.`);
  if (s.last_out_of_range) return L.banner('warn', 'alert', el('b', { text: 'Last reading was out of range. ' }), `Logged ${when}. Re-test and log again before producing.`);
  return L.banner('ok', 'check-circle', el('b', { text: 'Water test is up to date. ' }), `Last log ${when} - within range.`);
}

function mountWater(panel) {
  const statusHost = el('div'); const histHost = el('div.compact'); let offset = 0;
  const lim = LIM();
  const tds = L.unitInput({ name: 'tds_ppm', label: 'TDS', unit: 'ppm', placeholder: 'e.g. 140', required: true, help: `Allowed ${lim.tdsMin} to ${lim.tdsMax} ppm.` });
  const ph = L.unitInput({ name: 'ph', label: 'pH', unit: 'pH', placeholder: 'e.g. 7.2', required: true, help: `Allowed ${lim.phMin} to ${lim.phMax}.` });
  const oz = L.unitInput({ name: 'ozone_ppm', label: 'Ozone', unit: 'ppm', placeholder: 'e.g. 0.3', help: 'Optional - residual ozone after the ozonator.' });
  const tdsChip = el('div.chip-row'); const phChip = el('div.chip-row');
  tds.el.insertBefore(tdsChip, tds.el.querySelector('.error')); ph.el.insertBefore(phChip, ph.el.querySelector('.error'));
  const uv = el('input', { type: 'checkbox', name: 'uv_ok', checked: true });
  const uvRow = el('label.switch', {}, uv, el('span.track'), el('span', { text: 'UV lamp is working' }));
  const minHost = el('div.lines'); const minRows = [];
  const notesF = ui.field({ label: 'Notes', name: 'notes', type: 'textarea', optional: true, rows: 2, maxlength: 500 });
  const minList = el('datalist', { id: 'mineral-names' });
  L.refMaterials().then((m) => m.filter((x) => x.category === 'MINERAL').forEach((x) => minList.append(el('option', { value: x.name })))).catch(() => {});

  function addMineral(v = {}) {
    const name = ui.field({ label: 'Mineral', name: 'mineral_name', type: 'text', maxlength: 60, placeholder: 'e.g. Calcium chloride' }); name.input.setAttribute('list', 'mineral-names'); name.input.value = v.name || '';
    const val = L.unitInput({ name: 'mineral_value', label: 'Reading', unit: 'ppm' }); val.set(v.value || '');
    const row = { name, val, card: null };
    row.card = el('div.line-card', {}, el('div.line-head', {}, el('span', { text: 'Mineral reading' }), el('button.btn.ghost.sm.icon', { type: 'button', 'aria-label': 'Remove mineral reading', on: { click: () => { minRows.splice(minRows.indexOf(row), 1); row.card.remove(); form.dispatchEvent(new Event('input', { bubbles: true })); } } }, icon('trash'))), name.el, val.el);
    minRows.push(row); minHost.append(row.card);
  }
  const form = el('form.stack', { id: 'water-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }),
    el('div.card.stack', {}, el('div.card-head', {}, el('h2', {}, icon('droplet'), 'New reading')),
      el('div.form-grid', {}, tds.el, ph.el, oz.el, el('div.field', {}, el('span.label', { text: 'UV sterilizer' }), uvRow))),
    el('div.card.stack', {}, el('div.card-head', {}, el('h2', {}, icon('flask'), 'Mineral dosing readings')), el('p.section-sub', { text: 'Optional - note what the mineral dosing pump shows.' }), minHost, minList,
      L.btn('Add mineral reading', 'plus', { kind: 'secondary', sm: true, onClick: () => addMineral() })),
    el('div.card', {}, notesF.el));
  const submit = L.btn('Save reading', 'check', { kind: 'primary', type: 'submit', form: 'water-form' });
  const flow = L.makeFlow();

  const paintRange = () => {
    ui.clear(tdsChip); ui.clear(phChip);
    const c1 = L.rangeChip(tds.get(), lim.tdsMin, lim.tdsMax, ' ppm'); if (c1) tdsChip.append(c1);
    const c2 = L.rangeChip(ph.get(), lim.phMin, lim.phMax); if (c2) phChip.append(c2);
  };
  tds.input.addEventListener('input', paintRange); ph.input.addEventListener('input', paintRange);
  const getState = () => ({ tds: tds.get(), ph: ph.get(), oz: oz.get(), uv: uv.checked, notes: notesF.get(), minerals: minRows.map((r) => ({ name: r.name.get(), value: r.val.get() })) });
  const setState = (s) => { tds.set(s.tds || ''); ph.set(s.ph || ''); oz.set(s.oz || ''); uv.checked = s.uv !== false; notesF.set(s.notes || ''); minRows.splice(0).forEach((r) => r.card.remove()); (s.minerals || []).forEach(addMineral); paintRange(); };
  const draft = L.stateDraft({ page: 'production', id: 'water', form, get: getState, set: setState });
  draft.restore(); paintRange();

  const problem = (t, label, lo, hi, optional) => {
    if (optional && t === '') return '';
    const p = L.qtyProblem(t, { allowZero: true }); if (p) return p;
    if (/\.\d{4,}/.test(t)) return 'At most 3 decimal places';
    const n = Number(t); if (n < lo || n > hi) return `${label} must be between ${lo} and ${hi}`;
    return '';
  };
  form.addEventListener('submit', L.once(async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    const e1 = problem(tds.get(), 'TDS', 0, 10000), e2 = problem(ph.get(), 'pH', 0, 14), e3 = problem(oz.get(), 'Ozone', 0, 100, true);
    tds.setError(e1); ph.setError(e2); oz.setError(e3);
    const minerals = {}; let bad = !!(e1 || e2 || e3);
    minRows.forEach((r) => {
      r.name.setError(''); r.val.setError('');
      if (r.name.get().trim() === '' && r.val.get() === '') return;
      const n = r.name.get().trim(); const p = L.qtyProblem(r.val.get(), { allowZero: true });
      if (!n) { r.name.setError('Name the mineral'); bad = true; } else if (minerals[n] !== undefined) { r.name.setError('Already entered'); bad = true; }
      if (p) { r.val.setError(p); bad = true; }
      if (n && !p) minerals[n] = r.val.get().trim();
    });
    if (bad) { form.querySelector('.has-error input')?.focus(); return; }
    const warns = [];
    if (Number(tds.get()) < Number(lim.tdsMin) || Number(tds.get()) > Number(lim.tdsMax)) warns.push(`TDS ${tds.get()} ppm is outside ${lim.tdsMin}-${lim.tdsMax}.`);
    if (Number(ph.get()) < Number(lim.phMin) || Number(ph.get()) > Number(lim.phMax)) warns.push(`pH ${ph.get()} is outside ${lim.phMin}-${lim.phMax}.`);
    if (!uv.checked) warns.push('The UV lamp is marked as not working.');
    if (warns.length && !(await ui.confirmDialog({ title: 'This reading looks wrong', message: 'Please re-check the meter. If the numbers are right, save them - an alert goes to the managers.', warnings: warns, confirmText: 'Yes, save as entered', cancelText: 'Re-check', danger: true }))) return;
    const body = { tds_ppm: tds.get(), ph: ph.get(), uv_ok: uv.checked, minerals, ...(oz.get() !== '' ? { ozone_ppm: oz.get() } : {}), ...(notesF.get().trim() ? { notes: notesF.get().trim() } : {}) };
    const r = await L.submitPlain({ flow, form, button: submit, path: '/api/production/water-logs', body });
    if (r) {
      draft.clear(); ui.toast.ok('Water reading saved'); (r.warnings || []).forEach((w) => ui.toast.warn(w));
      setState({}); loadStatus(); loadHist();
    }
  }));

  async function loadStatus() {
    try { const s = await api.get('/api/production/water-logs/status'); ui.mount(statusHost, waterStatusBanner(s)); } catch (e) { if (!e.redirecting) ui.mount(statusHost, L.banner('warn', 'wifi-off', 'Cannot read the water test status right now.')); }
  }
  async function loadHist() {
    await L.refreshInto(histHost, async () => {
      const d = await api.get('/api/production/water-logs', { limit: 10, offset });
      return el('div.stack.tight', {}, ui.cardsTable({
        columns: [
          { label: 'Logged', cls: 'k', render: (w) => [el('b', { text: ui.fmtTime(w.logged_at) }), el('small', { text: `${ui.fmtDate(w.logged_at)} - ${w.logged_by_name || ''}` })] },
          { label: 'Result', cls: 's', render: (w) => (w.out_of_range ? ui.badge('FAIL', 'Out of range', 'bad') : ui.badge('OK', 'In range')) },
          { label: 'TDS', cls: 'num', render: (w) => `${ui.fmtNum(w.tds_ppm, 3)} ppm` }, { label: 'pH', cls: 'num', render: (w) => ui.fmtNum(w.ph, 3) },
          { label: 'Ozone', cls: 'num', render: (w) => (w.ozone_ppm !== null && w.ozone_ppm !== undefined ? `${ui.fmtNum(w.ozone_ppm, 3)} ppm` : '-') },
          { label: 'UV', render: (w) => (w.uv_ok ? 'OK' : el('span.chip-s.bad', { text: 'Not OK' })) },
          { label: 'Minerals', cls: 'w', hideMobile: true, render: (w) => Object.entries(w.minerals || {}).map(([k, v]) => `${k}: ${v}`).join(', ') },
        ], rows: d.items,
        emptyState: { icon: 'droplet', title: 'No readings yet', text: 'Log the first reading with the form above.' },
      }), d.total > 10 ? ui.pager({ total: d.total, limit: 10, offset, onPage: (o) => { offset = o; loadHist(); } }) : null);
    });
  }
  ui.mount(panel, statusHost, form, L.actionBar(submit), el('h2', { text: 'Recent readings' }), histHost);
  refreshers.water = () => Promise.all([loadStatus(), loadHist()]);
  refreshers.water();
}

/* ---------------------------------------------------------------------------------------------- tabs */
const ids = ['water', 'blow', 'batches'];
const hashId = (location.hash || '').slice(1);
const tabs = L.tabbed(main, [
  { id: 'water', label: 'Water log', icon: 'droplet', render: mountWater },
  { id: 'blow', label: 'Blow molding', icon: 'factory', render: (p) => mountBlow(p, ctx) },
  { id: 'batches', label: 'Batches', icon: 'package', render: (p) => mountBatches(p, ctx) },
], { initial: ids.includes(hashId) ? hashId : 'water' });
if (!ctx.site()) ui.mount(main, ui.empty({ icon: 'ban', title: 'No production site for your account', text: 'Production is only recorded at a production site (such as the main plant). Ask an administrator to give you access to it.' }));
else if (ERP.godown.multiple && ERP.godown.current() !== ctx.site().id) main.prepend(L.banner('info', 'pin', el('b', { text: `Production is recorded at ${ctx.site().name}. ` }), 'Other godowns do not produce.'));
// water status in the tab label even before it is opened
const refreshAll = () => Promise.all(Object.entries(refreshers).filter(([k]) => !k.includes(':')).map(([, f]) => f()));
ERP.godown.onChange(refreshAll);
ERP.live.start('production', { production: refreshAll, stock: refreshAll, qc: refreshAll, materials: refreshAll, master: () => { L.invalidateRef(); return refreshAll(); } });
