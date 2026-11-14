// qc.html - quarantine queue, batch detail, lab test entry, approve / reject (final), history.
import { boot, ERP } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import * as L from '/js/pages/warehouse-lib.js';

const { el, icon } = ui;
await boot({ page: 'qc', title: 'Lab QC', godownAll: true });
const main = document.getElementById('main');
const view = el('div'); ui.mount(main, view);
const LIM = () => { const s = L.settings(); return { tdsMin: s.tds_min ?? '50', tdsMax: s.tds_max ?? '500', phMin: s.ph_min ?? '6.5', phMax: s.ph_max ?? '8.5' }; };

let mode = 'list'; let openId = null;
const refreshers = {};

/* ---------------------------------------------------------------------------------------------- list */
function ageBadge(mins) {
  const m = Number(mins);
  if (m >= 480) return ui.badge('OVERDUE', `Waiting ${ui.fmtMinutes(m)}`);
  if (m >= 120) return ui.badge('DUE_SOON', `Waiting ${ui.fmtMinutes(m)}`);
  return el('span.small.muted', { text: `Waiting ${ui.fmtMinutes(m)}` });
}
const testBadge = (r) => (r === 'PASS' ? ui.badge('PASS', 'Test passed') : r === 'FAIL' ? ui.badge('FAIL', 'Test failed') : el('span.chip-s', { text: 'No test yet' }));

function showList(initial = 'queue') {
  mode = 'list'; openId = null; L.toTop();
  try { history.replaceState(null, '', location.pathname); } catch { /* ignore */ }
  ui.clear(view);
  const tabs = L.tabbed(view, [
    { id: 'queue', label: 'To test', icon: 'flask', render: mountQueue },
    { id: 'approved', label: 'Approved', icon: 'check-circle', render: (p) => mountHistory(p, 'APPROVED') },
    { id: 'rejected', label: 'Rejected', icon: 'x-circle', render: (p) => mountHistory(p, 'REJECTED') },
  ], { initial });
  return tabs;
}
function setBadge(n) {
  const t = document.getElementById('tab-queue'); if (!t) return;
  t.querySelector('.count-pill')?.remove(); if (n > 0) t.append(el('span.count-pill', { text: String(n) }));
}
function mountQueue(panel) {
  const host = el('div'); panel.append(host);
  async function load() {
    await L.refreshInto(host, async () => {
      const d = await api.get('/api/qc/batches', { status: 'QUARANTINE', limit: 100, ...L.readParam() });
      setBadge(d.total);
      if (!d.items.length) return ui.empty({ icon: 'check-circle', title: 'Nothing waiting for QC', text: 'New batches appear here the moment production creates them.' });
      return el('div.stack', {}, d.items.some((b) => b.age_minutes >= 480) ? L.banner('err', 'alert', el('b', { text: 'Some batches have waited more than 8 hours. ' }), 'Test the oldest first - stock is locked until QC decides.') : null,
        ui.cardsTable({
          columns: [
            { label: 'Batch', cls: 'k', render: (b) => [el('b.nowrap', { text: b.batch_no }), el('small', { text: b.product_name })] },
            { label: 'Waiting', cls: 's', render: (b) => ageBadge(b.age_minutes) },
            { label: 'Quantity', cls: 'num', render: (b) => L.cratesCell(b.crates, b.crates_per_pallet, b.kind === 'RETURNABLE' ? 'jars' : 'crates') },
            { label: 'Lab test', render: (b) => [testBadge(b.last_test_result), b.tests_count > 1 ? el('small.block.muted', { text: `${b.tests_count} tests` }) : null] },
            { label: 'Made', hideMobile: true, render: (b) => `${ui.fmtDate(b.produced_on)} - ${b.shift.toLowerCase()}` }, { label: 'Godown', hideMobile: !ERP.godown.multiple, render: (b) => b.location_name },
            { label: '', cls: 'a', render: (b) => L.btn('Open and test', 'flask', { kind: 'primary', onClick: () => showDetail(b.id) }) },
          ], rows: d.items, onRow: (b) => showDetail(b.id),
        }));
    });
  }
  refreshers.queue = load; load();
}
function mountHistory(panel, status) {
  const host = el('div'); panel.append(host); let offset = 0;
  async function load() {
    await L.refreshInto(host, async () => {
      const d = await api.get('/api/qc/batches', { status, limit: 15, offset, ...L.readParam() });
      return el('div.stack.tight', {}, ui.cardsTable({
        columns: [
          { label: 'Batch', cls: 'k', render: (b) => [el('b.nowrap', { text: b.batch_no }), el('small', { text: b.product_name })] }, { label: 'Decision', cls: 's', render: (b) => ui.badge(b.qc_status) },
          { label: 'Quantity', cls: 'num', render: (b) => L.cratesCell(b.crates, b.crates_per_pallet, b.kind === 'RETURNABLE' ? 'jars' : 'crates') }, { label: 'Decided', render: (b) => (b.decided_at ? ui.fmtDateTime(b.decided_at) : '-') },
          { label: 'Made', hideMobile: true, render: (b) => ui.fmtDate(b.produced_on) },
        ], rows: d.items, onRow: (b) => showDetail(b.id),
        emptyState: { icon: status === 'APPROVED' ? 'check-circle' : 'x-circle', title: `No ${status.toLowerCase()} batches yet` },
      }), d.total > 15 ? ui.pager({ total: d.total, limit: 15, offset, onPage: (o) => { offset = o; load(); } }) : null);
    });
  }
  refreshers[status] = load; load();
}

/* ---------------------------------------------------------------------------------------------- detail */
async function showDetail(id) {
  mode = 'detail'; openId = id; L.toTop();
  try { history.replaceState(null, '', `?batch=${encodeURIComponent(id)}`); } catch { /* ignore */ }
  ui.mount(view, ui.skeletonCards(3));
  let d;
  try { d = await api.get(`/api/qc/batches/${id}`); } catch (e) { ui.mount(view, L.backBar('QC queue', () => showList()), ui.errorState(e, () => showDetail(id))); return; }

  const lim = LIM();
  const headHost = el('div.stack'); const bannerHost = el('div'); const testsHost = el('div');
  const tds = L.unitInput({ name: 'tds_ppm', label: 'TDS', unit: 'ppm', help: `Allowed ${lim.tdsMin} to ${lim.tdsMax} ppm.` });
  const ph = L.unitInput({ name: 'ph', label: 'pH', unit: 'pH', help: `Allowed ${lim.phMin} to ${lim.phMax}.` });
  const oz = L.unitInput({ name: 'ozone_ppm', label: 'Ozone', unit: 'ppm' });
  const tu = L.unitInput({ name: 'turbidity_ntu', label: 'Turbidity', unit: 'NTU' });
  const tdsChip = el('div.chip-row'); const phChip = el('div.chip-row');
  tds.el.insertBefore(tdsChip, tds.el.querySelector('.error')); ph.el.insertBefore(phChip, ph.el.querySelector('.error'));
  const tri = (label, name) => { const s = ui.segmented({ options: [{ value: '', label: 'Not checked' }, { value: 'true', label: 'OK' }, { value: 'false', label: 'Not OK' }], value: '', label }); return { name, seg: s, el: el('div.field', {}, el('span.label', { text: label }), s.el) }; };
  const micro = tri('Microbial test', 'microbial_ok'), seal = tri('Cap and seal', 'seal_ok'), labl = tri('Label and date code', 'label_ok');
  const result = ui.segmented({ options: [{ value: 'PASS', label: 'PASS' }, { value: 'FAIL', label: 'FAIL' }], value: 'PASS', label: 'Overall result' });
  const remarks = ui.field({ label: 'Remarks', name: 'remarks', type: 'textarea', optional: true, rows: 2, maxlength: 500 });
  const hint = el('div.echo');
  const testForm = el('form.stack', { id: 'qc-test-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }),
    el('div.form-grid', {}, tds.el, ph.el, oz.el, tu.el), el('div.stack.tight', {}, micro.el, seal.el, labl.el),
    el('div.field', {}, el('span.label', { text: 'Overall result' }), result.el), hint, remarks.el);
  const testBtn = L.btn('Save test result', 'save', { kind: 'primary', type: 'submit', form: 'qc-test-form' });
  const formCard = el('section.card.stack', {}, el('div.card-head', {}, el('h2', {}, icon('flask'), 'Enter lab test result')), testForm, el('div.btn-row.end', {}, testBtn));

  const outOfRange = () => {
    const out = [];
    const t = L.dec(tds.get()), p = L.dec(ph.get());
    if (t !== null && (t < Number(lim.tdsMin) || t > Number(lim.tdsMax))) out.push(`TDS ${tds.get()} ppm is outside ${lim.tdsMin}-${lim.tdsMax}`);
    if (p !== null && (p < Number(lim.phMin) || p > Number(lim.phMax))) out.push(`pH ${ph.get()} is outside ${lim.phMin}-${lim.phMax}`);
    [[micro, 'Microbial test'], [seal, 'Cap and seal'], [labl, 'Label and date code']].forEach(([x, n]) => { if (x.seg.value === 'false') out.push(`${n} is Not OK`); });
    return out;
  };
  const paintHints = () => {
    ui.clear(tdsChip); ui.clear(phChip);
    const a = L.rangeChip(tds.get(), lim.tdsMin, lim.tdsMax, ' ppm'); if (a) tdsChip.append(a); const b = L.rangeChip(ph.get(), lim.phMin, lim.phMax); if (b) phChip.append(b);
    const bad = outOfRange(); hint.className = `echo ${bad.length ? 'bad' : ''}`; ui.clear(hint);
    if (bad.length) hint.append(icon('alert', 'sm'), `${bad.join('; ')} - this normally means FAIL.`);
  };
  [tds.input, ph.input].forEach((x) => x.addEventListener('input', paintHints));
  [micro, seal, labl].forEach((x) => { x.seg.el.addEventListener('click', () => setTimeout(paintHints, 0)); });

  const approveBtn = L.btn('Approve batch', 'check-circle', { kind: 'primary', cls: 'xl', onClick: () => approve() });
  const rejectBtn = L.btn('Reject batch', 'x-circle', { kind: 'danger outline', cls: 'xl', onClick: () => reject() });
  const decisionNote = el('p.small.muted.center');
  const decision = el('div.action-bar.qc-actions', {}, el('div.qc-note', {}, decisionNote), rejectBtn, approveBtn);

  function paint() {
    const ret = d.product && d.product.kind === 'RETURNABLE'; const decided = d.qc_status !== 'QUARANTINE';
    ui.mount(headHost, el('div.card.stack.tight', {}, el('div.row.between', { style: { flexWrap: 'nowrap', alignItems: 'flex-start' } }, el('div', {}, el('h2', { text: d.batch_no }), el('div.muted', { text: d.product.name })), ui.badge(d.qc_status, d.qc_status === 'QUARANTINE' ? 'Waiting for QC' : undefined)),
      L.kv([['Quantity', L.cratesCell(d.crates, d.product.crates_per_pallet, ret ? 'jars' : 'crates')], ['Made', `${ui.fmtDate(d.produced_on)} - ${d.shift.toLowerCase()} shift`], ['Expiry', ui.fmtDate(d.expiry_date)], ['Godown', d.location_name], ['Entered by', d.created_by_name], ['Production note', d.notes]])));
    ui.clear(bannerHost);
    if (decided) bannerHost.append(L.banner(d.qc_status === 'APPROVED' ? 'ok' : 'err', d.qc_status === 'APPROVED' ? 'check-circle' : 'x-circle',
      el('b', { text: `This batch was ${d.qc_status.toLowerCase()}${d.decided_by_name ? ` by ${d.decided_by_name}` : ''}${d.decided_at ? ` on ${ui.fmtDateTime(d.decided_at)}` : ''}. ` }), d.qc_notes ? `Note: ${d.qc_notes}. ` : '', 'QC decisions are final - nothing more to do here.'));
    ui.mount(testsHost, (d.tests || []).length ? ui.cardsTable({ columns: [
      { label: 'Tested', cls: 'k', render: (t) => [el('b', { text: ui.fmtDateTime(t.tested_at) }), el('small', { text: t.tested_by_name || '' })] }, { label: 'Result', cls: 's', render: (t) => ui.badge(t.result) },
      { label: 'TDS', cls: 'num', render: (t) => (t.tds_ppm ? `${ui.fmtNum(t.tds_ppm, 3)} ppm` : '-') }, { label: 'pH', cls: 'num', render: (t) => (t.ph ? ui.fmtNum(t.ph, 3) : '-') },
      { label: 'Ozone', cls: 'num', hideMobile: true, render: (t) => (t.ozone_ppm ? ui.fmtNum(t.ozone_ppm, 3) : '-') }, { label: 'Turbidity', cls: 'num', hideMobile: true, render: (t) => (t.turbidity_ntu ? ui.fmtNum(t.turbidity_ntu, 3) : '-') },
      { label: 'Checks', cls: 'w', render: (t) => ['microbial_ok:Microbial', 'seal_ok:Seal', 'label_ok:Label'].map((x) => { const [k, n] = x.split(':'); return t[k] === null || t[k] === undefined ? null : el('span.chip-s', { class: t[k] ? 'ok' : 'bad', text: `${n} ${t[k] ? 'OK' : 'not OK'}` }); }).filter(Boolean) },
      { label: 'Remarks', cls: 'w', render: (t) => t.remarks },
    ], rows: d.tests }) : el('p.muted', { text: 'No test recorded yet. Enter the lab results below.' }));
    formCard.hidden = decided; decision.hidden = decided;
    const latest = (d.tests || [])[0]; const canApprove = !decided && latest && latest.result === 'PASS';
    approveBtn.disabled = !canApprove; approveBtn.setAttribute('aria-disabled', String(!canApprove));
    decisionNote.textContent = !latest ? 'Record a passing test before approving.' : latest.result === 'FAIL' ? 'The latest test FAILED - record a passing test to approve, or reject the batch.' : `Latest test PASSED (${ui.fmtDateTime(latest.tested_at)}).`;
  }
  async function reload(stale = false) {
    try { const n = await api.get(`/api/qc/batches/${id}`); const was = d.qc_status; d = n; paint(); if (stale && was === 'QUARANTINE' && d.qc_status !== 'QUARANTINE') ui.toast.warn(`This batch was ${d.qc_status.toLowerCase()} by ${d.decided_by_name || 'someone else'}.`); } catch (e) { if (!e.redirecting && !e.network) ui.toastError(e); }
  }
  refreshers.detail = () => reload(true);

  /* test submit */
  const flow = L.makeFlow();
  testForm.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form_());
    let bad = false; const body = { result: result.value };
    const num = (f, label, hi) => { const t = f.get(); if (t === '') { f.setError(''); return; } const p = L.qtyProblem(t, { allowZero: true }); const n = L.dec(t); f.setError(p || (n !== null && n > hi ? `${label} must be at most ${hi}` : '')); if (p || (n !== null && n > hi)) bad = true; else body[f.input.name] = t.trim(); };
    num(tds, 'TDS', 10000); num(ph, 'pH', 14); num(oz, 'Ozone', 100); num(tu, 'Turbidity', 1000);
    [micro, seal, labl].forEach((x) => { if (x.seg.value !== '') body[x.name] = x.seg.value === 'true'; });
    if (Object.keys(body).length === 1) { ui.formError(testForm, { status: 422, message: 'Enter at least one reading or check before saving.', fields: {} }); return; }
    if (result.value === 'FAIL' && remarks.get().trim().length < 3) { remarks.setError('Say what failed (this helps the reject reason)'); bad = true; } else remarks.setError('');
    if (remarks.get().trim()) body.remarks = remarks.get().trim();
    if (bad) { testForm.querySelector('.has-error input, .has-error textarea')?.focus(); return; }
    const issues = outOfRange();
    if (result.value === 'PASS' && issues.length && !(await ui.confirmDialog({ title: 'You marked PASS but...', message: 'Some readings look wrong. Save PASS anyway?', warnings: issues, confirmText: 'Yes, save PASS', cancelText: 'Go back', danger: true }))) return;
    const r = await L.submitPlain({ flow, form: testForm, button: testBtn, path: `/api/qc/batches/${id}/tests`, body, onStale: () => reload(true) });
    if (r) { d = r; paint(); ui.toast.ok(`Test saved: ${r.tests[0].result}`); resetForm(); }
  });
  const form_ = () => testForm;
  function resetForm() { [tds, ph, oz, tu].forEach((f) => f.set('')); [micro, seal, labl].forEach((x) => x.seg.set('')); remarks.set(''); paintHints(); }

  /* decisions */
  async function approve() {
    const notes = ui.field({ label: 'Note', name: 'notes', type: 'textarea', optional: true, rows: 2, maxlength: 500 });
    const ok = await dialogConfirm({ title: `Approve ${d.batch_no}?`, danger: false, confirmText: 'Approve and release', body: [
      el('p', {}, `${ui.fmtNum(d.crates, 0)} ${d.product.kind === 'RETURNABLE' ? 'jars' : 'crates'} of ${d.product.name} move to finished goods and can be loaded onto trucks.`),
      L.banner('warn', 'alert', el('b', { text: 'This cannot be undone.' }), ' QC decisions are final. Only approve if the latest test passed and the batch is good.'), notes.el] });
    if (!ok) return;
    const act = flow.act;
    try {
      const r = await api.busy(approveBtn, () => act.post(`/api/qc/batches/${id}/approve`, notes.get().trim() ? { notes: notes.get().trim() } : {}));
      ui.toast.ok(`${r.batch_no} approved - released to finished goods`); showList('approved');
    } catch (e) { handleDecisionError(e); }
  }
  async function reject() {
    const reason = await ui.promptDialog({ title: `Reject ${d.batch_no}?`, label: 'Why is this batch rejected?', help: `All ${ui.fmtNum(d.crates, 0)} ${d.product.kind === 'RETURNABLE' ? 'jars' : 'crates'} go to the scrap yard. This cannot be undone.`, min: 5, max: 500, confirmText: 'Reject batch', danger: true });
    if (!reason) return;
    try {
      const r = await api.busy(rejectBtn, () => flow.act.post(`/api/qc/batches/${id}/reject`, { reason }));
      ui.toast.ok(`${r.batch_no} rejected`); showList('rejected');
    } catch (e) { handleDecisionError(e); }
  }
  function handleDecisionError(e) {
    if (!e || e.redirecting || e.cancelled) return;
    if (e.code === 'INVALID_STATE') { L.errorDialog(e, 'Not possible'); reload(true); return; }
    L.errorDialog(e, 'Decision not saved');
  }

  ui.mount(view, L.backBar('QC queue', () => showList(d.qc_status === 'APPROVED' ? 'approved' : d.qc_status === 'REJECTED' ? 'rejected' : 'queue')), bannerHost,
    el('div.stack', {}, headHost, el('section.card.stack', {}, el('div.card-head', {}, el('h2', {}, icon('list'), 'Tests so far')), testsHost), formCard), decision);
  paint();
}

function dialogConfirm({ title, body, confirmText, danger }) {
  const yes = L.btn(confirmText, 'check', { kind: danger ? 'danger' : 'primary', onClick: () => dlg.close(true) });
  const no = L.btn('Cancel', null, { kind: 'secondary', onClick: () => dlg.close(false) });
  const dlg = ui.openDialog({ title, body: el('div.stack', {}, ...body), actions: [no, yes] });
  return dlg.closed.then((r) => r === true);
}

/* ---------------------------------------------------------------------------------------------- start */
const refreshAll = async () => {
  if (mode === 'detail') { if (refreshers.detail) await refreshers.detail(); return; }
  await Promise.all(['queue', 'APPROVED', 'REJECTED'].map((k) => refreshers[k] && refreshers[k]()));
};
const q = new URLSearchParams(location.search).get('batch');
if (q) showDetail(q); else showList('queue');
ERP.godown.onChange(() => { if (mode === 'list') refreshAll(); });
ERP.live.start('qc', { qc: refreshAll, production: refreshAll });
