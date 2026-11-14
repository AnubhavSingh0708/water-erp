// godowns.html - inter-godown transfers: list, create, receive (sent / received / damaged / short), cancel, print.
import { boot, ERP } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import * as L from '/js/pages/warehouse-lib.js';

const { el, icon } = ui;
await boot({ page: 'godowns', title: 'Godown transfers', godownAll: false, livePage: 'warehouse' });
const main = document.getElementById('main');
const view = el('div'); ui.mount(main, view);
const PAGE = 'godowns';
let mode = 'list'; let openId = null; let detailRefresh = null;
const refreshers = {};
const gName = (id, fallback = 'Godown') => L.anyGodownName(id, fallback);
const mine = (id) => ERP.godown.list.some((g) => g.id === id);
const ZONES = ['RM_STORE', 'BLOW_FLOOR', 'FG_READY', 'JAR_EMPTY', 'JAR_CLEAN'];

/* ---------------------------------------------------------------------------------------------- list */
function showList(initial = 'IN_TRANSIT') {
  mode = 'list'; openId = null; detailRefresh = null; L.toTop();
  ui.clear(view);
  view.append(el('div.page-head', {}, el('div', {}, el('p.sub', { text: 'Move goods between godowns. They stay "in transit" until the other godown confirms what arrived.' })),
    el('div.actions', {}, L.btn('New transfer', 'plus', { onClick: () => newTransfer() }))));
  const mk = (status) => (p) => mountTransfers(p, status);
  L.tabbed(view, [{ id: 'IN_TRANSIT', label: 'On the road', icon: 'truck', render: mk('IN_TRANSIT') }, { id: 'RECEIVED', label: 'Received', icon: 'check-circle', render: mk('RECEIVED') },
    { id: 'CANCELLED', label: 'Cancelled', icon: 'ban', render: mk('CANCELLED') }, { id: 'ALL', label: 'All', icon: 'list', render: mk('') }], { initial });
}
function mountTransfers(panel, status) {
  const host = el('div'); panel.append(host); let offset = 0;
  async function load() {
    await L.refreshInto(host, async () => {
      const d = await api.get('/api/godowns/transfers', { status, limit: 15, offset });
      if (status === 'IN_TRANSIT') { const t = document.getElementById('tab-IN_TRANSIT'); if (t) { t.querySelector('.count-pill')?.remove(); if (d.total) t.append(el('span.count-pill', { text: String(d.total) })); } }
      return el('div.stack.tight', {}, d.items.some((t) => t.overdue) ? L.banner('err', 'alert', el('b', { text: 'Some transfers have been on the road for more than 48 hours. ' }), 'Call the driver or the receiving godown.') : null,
        ui.cardsTable({
          columns: [
            { label: 'Transfer', cls: 'k', render: (t) => [el('b.nowrap', { text: t.tf_no }), el('small', { text: `${t.from_name} to ${t.to_name}` })] },
            { label: 'Status', cls: 's', render: (t) => [ui.badge(t.status), t.overdue ? el('div', { style: { marginTop: '4px', textAlign: 'right' } }, ui.badge('OVERDUE', 'Over 48 h')) : null] },
            { label: 'Sent', render: (t) => [ui.fmtDate(t.created_at), el('small.block.muted', { text: ui.fmtRelative(t.created_at) })] }, { label: 'Items', cls: 'num', render: (t) => `${t.lines_count} ${Number(t.lines_count) === 1 ? 'line' : 'lines'}` },
            { label: 'Vehicle', hideMobile: true, render: (t) => t.vehicle_no || '-' }, { label: 'Received', hideMobile: true, render: (t) => (t.received_at ? ui.fmtDateTime(t.received_at) : '-') },
            { label: '', cls: 'a', render: (t) => el('div.btn-row', {}, L.btn(t.status === 'IN_TRANSIT' && mine(t.to_location_id) ? 'Open and receive' : 'Open', 'list', { kind: t.status === 'IN_TRANSIT' && mine(t.to_location_id) ? 'primary' : 'secondary', sm: true, onClick: () => openTransfer(t.id) })) },
          ], rows: d.items, onRow: (t) => openTransfer(t.id),
          emptyState: { icon: 'transfer', title: status === 'IN_TRANSIT' ? 'Nothing on the road' : 'No transfers here', text: status === 'IN_TRANSIT' ? 'Create a transfer when goods leave for another godown.' : 'Transfers appear here once they are received or cancelled.' },
        }), d.total > 15 ? ui.pager({ total: d.total, limit: 15, offset, onPage: (o) => { offset = o; load(); } }) : null);
    });
  }
  refreshers[status || 'ALL'] = load; load();
}

/* ---------------------------------------------------------------------------------------------- document */
const lineQty = (l, v) => `${ui.fmtNum(v, 4)} ${l.unit}`;
function transferDoc(t) {
  const done = t.status === 'RECEIVED';
  const cols = [{ h: 'Item', v: (l) => l.name }, { h: 'Lot', v: (l) => l.lot_label || '' }, { h: 'From zone', v: (l) => L.zoneLabel(l.zone) }, { h: 'Sent', num: true, v: (l) => lineQty(l, l.qty_sent) }];
  if (done) cols.push({ h: 'Received', num: true, v: (l) => lineQty(l, l.qty_received) }, { h: 'Damaged', num: true, v: (l) => lineQty(l, l.qty_damaged || 0) }, { h: 'Short', num: true, v: (l) => (ui.toNum(l.qty_short) > 0 ? lineQty(l, l.qty_short) : '-') });
  else cols.push({ h: 'Received (fill in)', num: true, v: () => '' });
  return el('div.stack.tight', {}, L.kv([['Transfer', t.tf_no], ['From', t.from_name], ['To', t.to_name], ['Vehicle', t.vehicle_no], ['Driver', t.driver_name], ['Sent on', ui.fmtDateTime(t.created_at)], ['Sent by', t.created_by_name], ['Status', t.status === 'IN_TRANSIT' ? 'On the road' : t.status.toLowerCase()],
    done ? ['Received', `${ui.fmtDateTime(t.received_at)}${t.received_by_name ? ` by ${t.received_by_name}` : ''}`] : null, t.cancel_reason ? ['Cancelled because', t.cancel_reason] : null, ['Note', t.note]]),
  L.docTable(cols, t.lines), L.signatureBlock(['Sent by', 'Driver', 'Received by']));
}
const showNote = (t) => L.showDocument({ title: 'Godown transfer note', docNo: t.tf_no, subtitle: `${t.from_name} to ${t.to_name}`, build: () => transferDoc(t) });

/* ---------------------------------------------------------------------------------------------- detail */
async function openTransfer(id, pre) {
  mode = 'detail'; openId = id; L.toTop(); ui.mount(view, ui.skeletonCards(2));
  let t;
  try { t = pre || await api.get(`/api/godowns/transfers/${id}`); } catch (e) { ui.mount(view, L.backBar('Transfers', () => showList()), ui.errorState(e, () => openTransfer(id))); return; }
  const printBtn = L.btn('Print note', 'print', { kind: 'secondary', sm: true, onClick: () => showNote(t) });
  const canRecv = t.status === 'IN_TRANSIT' && mine(t.to_location_id); const canCancel = t.status === 'IN_TRANSIT' && (mine(t.from_location_id) || ERP.me.is_admin);
  const hours = (Date.now() - new Date(t.created_at).getTime()) / 3600000;
  const recvBtn = L.btn('Receive goods', 'download', { kind: 'primary', cls: 'xl', onClick: () => receiveView(t) });
  const cancelBtn = L.btn('Cancel transfer', 'ban', { kind: 'danger outline', cls: 'xl', onClick: () => cancelTransfer(t, cancelBtn) });
  const lines = ui.cardsTable({ columns: [
    { label: 'Item', cls: 'k', render: (l) => [el('b', { text: l.name }), el('small', { text: `${l.lot_label || ''}${l.lot_label ? ' - ' : ''}${L.zoneLabel(l.zone)}` })] },
    { label: 'Sent', cls: 'num', render: (l) => lineQty(l, l.qty_sent) },
    ...(t.status === 'RECEIVED' ? [{ label: 'Received', cls: 'num', render: (l) => lineQty(l, l.qty_received) }, { label: 'Damaged', cls: 'num', render: (l) => (ui.toNum(l.qty_damaged) > 0 ? el('span.chip-s.warn', { text: lineQty(l, l.qty_damaged) }) : '-') },
      { label: 'Short', cls: 'num', render: (l) => (ui.toNum(l.qty_short) > 0 ? el('span.chip-s.bad', { text: lineQty(l, l.qty_short) }) : el('span.chip-s.ok', { text: 'None' })) }, { label: 'Note', cls: 'w', render: (l) => l.note }] : []),
  ], rows: t.lines });
  ui.mount(view, L.backBar('Transfers', () => showList(t.status === 'IN_TRANSIT' ? 'IN_TRANSIT' : t.status)),
    el('div.stack', {}, el('div.card.stack.tight', {}, el('div.row.between', { style: { flexWrap: 'nowrap', alignItems: 'flex-start' } }, el('div', {}, el('h2', { text: t.tf_no }), el('div.muted', { text: `${t.from_name} to ${t.to_name}` })), ui.badge(t.status)),
      L.kv([['Sent', `${ui.fmtDateTime(t.created_at)}${t.created_by_name ? ` by ${t.created_by_name}` : ''}`], ['Vehicle', t.vehicle_no], ['Driver', t.driver_name], ['Note', t.note], t.status === 'RECEIVED' ? ['Received', `${ui.fmtDateTime(t.received_at)}${t.received_by_name ? ` by ${t.received_by_name}` : ''}`] : null, t.cancel_reason ? ['Cancelled because', t.cancel_reason] : null]),
      el('div.btn-row', {}, printBtn)),
    t.status === 'IN_TRANSIT' && hours > 48 ? L.banner('err', 'alert', el('b', { text: `On the road for ${ui.fmtMinutes(hours * 60)}. ` }), 'Check with the driver and the receiving godown.') : null,
    t.status === 'IN_TRANSIT' && !canRecv ? L.banner('info', 'info', `Only staff of ${t.to_name} can receive this transfer.`) : null,
    el('section.card.stack', {}, el('div.card-head', {}, el('h2', {}, icon('box'), 'Goods on this transfer')), lines)),
    canRecv || canCancel ? el('div.action-bar', {}, canCancel ? cancelBtn : null, canRecv ? recvBtn : null) : null);
  detailRefresh = async () => { try { const n = await api.get(`/api/godowns/transfers/${id}`); if (n.status !== t.status) { ui.toast.warn(`This transfer is now ${n.status.toLowerCase().replace('_', ' ')}.`); openTransfer(id, n); } } catch { /* keep */ } };
}
async function cancelTransfer(t, button) {
  const reason = await ui.promptDialog({ title: `Cancel ${t.tf_no}?`, label: 'Why is it cancelled?', help: `All goods go back to ${t.from_name}. Use this only if the goods never left or are being brought back.`, min: 5, max: 300, confirmText: 'Review cancellation', danger: true });
  if (!reason) return;
  const r = await L.dialogFlow({ flow: L.makeFlow(), button, path: `/api/godowns/transfers/${t.id}/cancel`, body: { reason }, title: `Cancel ${t.tf_no}`, confirmText: 'Cancel and return the goods', danger: true, sendConfirm: 'always', summary: `The goods go back into ${t.from_name}.` });
  if (r) { ui.toast.ok(`${t.tf_no} cancelled`); openTransfer(t.id, r); }
}

/* ---------------------------------------------------------------------------------------------- receive */
function receiveView(t) {
  mode = 'receive'; L.toTop();
  const form = el('form.stack', { id: 'recv-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }));
  const rows = t.lines.map((l) => {
    const whole = l.item_type === 'PRODUCT';
    const rec = L.unitInput({ name: 'qty_received', label: 'Received', unit: l.unit, inputmode: whole ? 'numeric' : 'decimal', required: true }); rec.set(String(ui.fmtNum(l.qty_sent, 4)).replace(/,/g, ''));
    const dmg = L.unitInput({ name: 'qty_damaged', label: 'Damaged', unit: l.unit, inputmode: whole ? 'numeric' : 'decimal' }); dmg.set('0');
    const note = ui.field({ label: 'Note (what is missing?)', name: 'note', type: 'text', maxlength: 300 });
    const noteWrap = note.el; noteWrap.hidden = true;
    const out = el('div.chip-row');
    const card = el('div.line-card', {}, el('div.row.between', { style: { flexWrap: 'nowrap', alignItems: 'flex-start' } }, el('div', {}, el('b', { text: l.name }), el('div.small.muted', { text: `${l.lot_label || ''}${l.lot_label ? ' - ' : ''}${L.zoneLabel(l.zone)}` })), el('div.right', {}, el('div.small.muted', { text: 'Sent' }), el('b', { text: lineQty(l, l.qty_sent) }))),
      el('div.line-grid', {}, rec.el, dmg.el), out, noteWrap);
    const row = { l, rec, dmg, note, card, short: 0, problem: '',
      calc() {
        ui.clear(out); row.problem = ''; row.short = 0;
        const sent = ui.toNum(l.qty_sent); const r = L.dec(rec.get()); const d = dmg.get() === '' ? 0 : L.dec(dmg.get());
        const pr = L.qtyProblem(rec.get(), { whole, allowZero: true }); const pd = dmg.get() === '' ? '' : L.qtyProblem(dmg.get(), { whole, allowZero: true });
        rec.echo(pr, 'bad'); dmg.echo(pd, 'bad');
        if (pr || pd || r === null || d === null) { row.problem = pr || pd || 'x'; noteWrap.hidden = true; return; }
        if (r + d > sent + 1e-9) { row.problem = 'More than was sent'; out.append(el('span.chip-s.bad', {}, icon('alert', 'sm'), `Received + damaged (${ui.fmtNum(r + d, 4)}) is more than the ${ui.fmtNum(sent, 4)} sent`)); noteWrap.hidden = true; return; }
        row.short = Math.round((sent - r - d) * 10000) / 10000;
        if (row.short > 0) out.append(el('span.chip-s.bad', {}, icon('alert', 'sm'), `Short ${ui.fmtNum(row.short, 4)} ${l.unit} - will be written off`));
        else out.append(el('span.chip-s.ok', {}, icon('check-circle', 'sm'), d > 0 ? `${ui.fmtNum(d, 4)} damaged, rest arrived` : 'All arrived'));
        if (d > 0) out.append(el('span.chip-s.warn', { text: 'Damaged goods go to the scrap yard' }));
        noteWrap.hidden = row.short <= 0;
      } };
    [rec.input, dmg.input].forEach((x) => x.addEventListener('input', () => { row.calc(); summary(); }));
    row.calc(); form.append(card); return row;
  });
  const sumBox = el('div'); function summary() { const s = rows.filter((r) => r.short > 0); ui.mount(sumBox, s.length ? L.banner('err', 'alert', el('b', { text: `Short on ${s.length} ${s.length === 1 ? 'line' : 'lines'}. ` }), 'You will have to confirm and explain the shortage.') : L.banner('ok', 'check-circle', 'Everything matches so far.')); }
  summary();
  const draft = L.stateDraft({ page: PAGE, id: `recv-${t.id}`, form, get: () => rows.map((r) => [r.rec.get(), r.dmg.get(), r.note.get()]), set: (s) => { rows.forEach((r, i) => { if (s[i]) { r.rec.set(s[i][0]); r.dmg.set(s[i][1]); r.note.set(s[i][2]); r.calc(); } }); summary(); } });
  draft.restore(); summary();
  const submit = L.btn('Review and receive', 'check', { kind: 'primary', cls: 'xl', type: 'submit', form: 'recv-form' });
  const flow = L.makeFlow();
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    let bad = false;
    rows.forEach((r) => { r.calc(); r.note.setError(''); if (r.problem) { bad = true; } else if (r.short > 0 && r.note.get().trim() === '') { r.note.setError('Say what is missing (for example "2 crates not on the truck")'); bad = true; } });
    if (bad) { form.querySelector('.has-error input, .line-card .chip-s.bad')?.scrollIntoView({ block: 'center', behavior: 'smooth' }); return; }
    const body = { lines: rows.map((r) => ({ line_id: r.l.line_id, qty_received: r.rec.get().trim(), qty_damaged: (r.dmg.get().trim() || '0'), ...(r.note.get().trim() ? { note: r.note.get().trim() } : {}) })) };
    const r = await L.submitDocument({ flow, form, button: submit, path: `/api/godowns/transfers/${t.id}/receive`, body, title: `Receive ${t.tf_no}`, confirmText: rows.some((x) => x.short > 0) ? 'Confirm the shortage and receive' : 'Receive into stock', danger: rows.some((x) => x.short > 0),
      summary: `Goods arrive in ${t.to_name}. Damaged goods go to the scrap yard; anything missing is written off.`, onStale: () => openTransfer(t.id) });
    if (r) { draft.clear(); ui.toast.ok(`${t.tf_no} received`); (r.warnings || []).forEach((w) => ui.toast.warn(w)); openTransfer(t.id, r); }
  });
  ui.mount(view, L.backBar(t.tf_no, () => openTransfer(t.id, t)), el('h2', { text: `Receive ${t.tf_no}` }), el('p.section-sub', { text: `From ${t.from_name}. Count what actually came off the truck and type it for each line.` }), sumBox, form, L.actionBar(submit));
}

/* ---------------------------------------------------------------------------------------------- create */
async function newTransfer() {
  mode = 'create'; L.toTop(); ui.mount(view, ui.skeletonCards(2));
  let vehicles = []; try { vehicles = await L.refVehicles(); } catch { /* optional */ }
  const dests = await L.loadAllLocations();                       // every active godown: where goods may be SENT
  const gl = ERP.godown.list;                                     // godowns this user may send FROM
  const from = ui.field({ label: 'Send from', name: 'from_location_id', type: 'select', required: true, options: gl.map((g) => ({ value: g.id, label: g.name })), value: (ERP.godown.current() && ERP.godown.current() !== 'all') ? ERP.godown.current() : (gl[0] || {}).id });
  const to = ui.field({ label: 'Send to', name: 'to_location_id', type: 'select', required: true, options: [] });
  const veh = ui.field({ label: 'Vehicle', name: 'vehicle_id', type: 'select', optional: true, options: [{ value: '', label: 'Not listed / none' }, ...vehicles.map((v) => ({ value: v.id, label: v.reg_no }))] });
  const vno = ui.field({ label: 'Vehicle number (if not listed)', name: 'vehicle_no', type: 'text', optional: true, maxlength: 30 });
  const driver = ui.field({ label: 'Driver', name: 'driver_name', type: 'text', optional: true, maxlength: 80 });
  const note = ui.field({ label: 'Note', name: 'note', type: 'textarea', optional: true, rows: 2, maxlength: 500 });
  const lines = []; const linesHost = el('div.lines'); const emptyLines = el('p.muted', { text: 'No goods added yet.' });
  const fill = () => { const cur = from.get(); ui.clear(to.input); const opts = dests.filter((g) => g.id !== cur); to.input.append(...(opts.length ? [el('option', { value: '', text: 'Choose destination...' }), ...opts.map((g) => el('option', { value: g.id, text: g.name }))] : [el('option', { value: '', text: 'No other godown exists' })])); if (opts.length === 1) to.input.value = opts[0].id; };
  fill();
  const noDest = dests.length < 2 ? L.banner('warn', 'info', el('b', { text: 'There is no other godown to send goods to. ' }), 'An administrator can add a godown under Administration > Master data.') : null;
  const form = el('form.stack', { id: 'tf-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }), noDest,
    el('div.card.stack', {}, el('div.form-grid', {}, from.el, to.el)),
    el('div.card.stack', {}, el('div.card-head', {}, el('h2', {}, icon('box'), 'Goods to send')), linesHost, emptyLines, L.btn('Add goods', 'plus', { kind: 'secondary', onClick: () => addDialog() })),
    el('div.card.stack', {}, el('div.form-grid', {}, veh.el, vno.el, driver.el), note.el));
  const submit = L.btn('Review and send', 'check', { kind: 'primary', cls: 'xl', type: 'submit', form: 'tf-form' });
  const flow = L.makeFlow();

  function renderLines() {
    ui.clear(linesHost); emptyLines.hidden = lines.length > 0;
    lines.forEach((ln, i) => {
      const g = ln.g; const q = L.unitInput({ name: 'line_qty', label: `Quantity to send (free: ${ui.fmtQty(g.free, g.unit, 4)})`, unit: g.unit, inputmode: g.whole ? 'numeric' : 'decimal', required: true }); q.set(ln.qty);
      const chk = () => { const p = L.qtyProblem(q.get(), { whole: g.whole, max: ui.toNum(g.free) }); q.echo(q.get() === '' ? '' : p || (g.cpp && g.whole ? `= ${ui.fmtCrates(Number(q.get()), g.cpp)}` : ''), p ? 'bad' : ''); ln.qty = q.get(); };
      q.input.addEventListener('input', () => { chk(); form.dispatchEvent(new Event('input', { bubbles: true })); });
      linesHost.append(el('div.line-card', {}, el('div.line-head', {}, el('span', { text: `Line ${i + 1}` }), el('button.btn.ghost.sm.icon', { type: 'button', 'aria-label': `Remove ${g.name}`, on: { click: () => { lines.splice(i, 1); renderLines(); form.dispatchEvent(new Event('input', { bubbles: true })); } } }, icon('trash'))),
        el('div', {}, el('b', { text: g.name }), el('div.small.muted', { text: `${g.lot_label || (g.item_type === 'PRODUCT' ? '' : 'Opening stock')}${g.lot_label ? ' - ' : ''}${L.zoneLabel(g.zone)}${g.expiry_date ? ` - exp ${ui.fmtDate(g.expiry_date)}` : ''}` })), q.el));
      ln.chk = chk; chk();
    });
  }
  function addDialog() {
    if (!from.get()) { from.setError('Choose the sending godown first'); return; }
    const picker = L.stockPicker({ locationId: () => from.get(), zones: ZONES, mode: 'existing', onChange: upd });
    const q = L.unitInput({ name: 'add_qty', label: 'Quantity to send', unit: '', required: true });
    const err = el('div.form-error');
    const body = el('div.stack', {}, err, picker.el, q.el);
    function upd() { const g = picker.get(); const u = q.field.el.querySelector('.unit'); u.textContent = g ? g.unit : ' '; q.echo(g ? `Free to send: ${ui.fmtQty(g.free, g.unit, 4)}${ui.toNum(g.reserved) > 0 ? ` (${ui.fmtNum(g.reserved, 0)} held for loading trips)` : ''}` : ''); }
    q.input.addEventListener('input', () => { const g = picker.get(); if (g && q.get() !== '') { const p = L.qtyProblem(q.get(), { whole: g.whole, max: ui.toNum(g.free) }); q.echo(p || `Sending ${ui.fmtNum(q.get(), 4)} of ${ui.fmtNum(g.free, 4)} free`, p ? 'bad' : ''); } });
    const add = L.btn('Add to transfer', 'plus', { kind: 'primary', onClick: () => {
      ui.clear(err); const g = picker.get(); if (!g) { err.append(L.banner('err', 'alert', 'Choose the zone, item and lot first.')); return; }
      const p = L.qtyProblem(q.get(), { whole: g.whole, max: ui.toNum(g.free) }); if (p) { q.setError(p); return; }
      if (lines.some((x) => x.g.item_id === g.item_id && x.g.zone === g.zone && x.g.lot_id === g.lot_id)) { err.append(L.banner('err', 'alert', 'This item and lot is already on the transfer - change its quantity instead.')); return; }
      lines.push({ g, qty: q.get().trim() }); renderLines(); form.dispatchEvent(new Event('input', { bubbles: true })); dlg.close();
    } });
    const cancel = L.btn('Cancel', null, { kind: 'secondary', onClick: () => dlg.close() });
    const dlg = ui.openDialog({ title: 'Add goods', body, actions: [cancel, add], wide: true });
  }
  from.input.addEventListener('change', async () => {
    if (lines.length && !(await ui.confirmDialog({ title: 'Change the sending godown?', message: 'The goods you added belong to the old godown and will be removed from this transfer.', confirmText: 'Change and clear goods', cancelText: 'Keep' }))) { from.set(from.input.dataset.prev || from.get()); return; }
    lines.splice(0); renderLines(); fill(); from.input.dataset.prev = from.get();
  });
  from.input.dataset.prev = from.get();
  const getState = () => ({ from: from.get(), to: to.get(), veh: veh.get(), vno: vno.get(), driver: driver.get(), note: note.get(), lines: lines.map((l) => ({ g: l.g, qty: l.qty })) });
  const setState = (s) => { if (s.from) from.set(s.from); fill(); to.set(s.to || to.get()); veh.set(s.veh || ''); vno.set(s.vno || ''); driver.set(s.driver || ''); note.set(s.note || ''); lines.splice(0, lines.length, ...(s.lines || [])); renderLines(); };
  const draft = L.stateDraft({ page: PAGE, id: 'new', form, get: getState, set: setState });
  renderLines(); draft.restore();

  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    let bad = false;
    if (!from.get()) { from.setError('Choose the sending godown'); bad = true; } else from.setError('');
    if (!to.get()) { to.setError('Choose where the goods are going'); bad = true; } else to.setError('');
    if (!lines.length) { ui.formError(form, { status: 422, message: 'Add at least one item to send.', fields: {} }); return; }
    lines.forEach((l) => { l.chk && l.chk(); if (L.qtyProblem(l.qty, { whole: l.g.whole, max: ui.toNum(l.g.free) })) bad = true; });
    if (bad) { form.querySelector('.has-error select, .line-card .echo.bad')?.scrollIntoView({ block: 'center', behavior: 'smooth' }); return; }
    const sel = vehicles.find((v) => v.id === veh.get());
    const body = { from_location_id: from.get(), to_location_id: to.get(), lines: lines.map((l) => ({ item_type: l.g.item_type, item_id: l.g.item_id, zone: l.g.zone, ...(l.g.lot_id ? { lot_id: l.g.lot_id } : {}), qty: l.qty.trim() })),
      ...(veh.get() ? { vehicle_id: veh.get(), vehicle_no: sel ? sel.reg_no : undefined } : (vno.get().trim() ? { vehicle_no: vno.get().trim() } : {})), ...(driver.get().trim() ? { driver_name: driver.get().trim() } : {}), ...(note.get().trim() ? { note: note.get().trim() } : {}) };
    const t = await L.submitDocument({ flow, form, button: submit, path: '/api/godowns/transfers', body, title: 'Review transfer', confirmText: 'Send goods', summary: `Goods leave ${gName(from.get())} now and stay "in transit" until ${gName(to.get())} confirms.` });
    if (t) { draft.clear(); ui.toast.ok(`Created ${t.tf_no}`); (t.warnings || []).forEach((w) => ui.toast.warn(w)); showList('IN_TRANSIT'); showNote(t); }
  });
  ui.mount(view, L.backBar('Transfers', () => showList()), el('h2', { text: 'New godown transfer' }), form, L.actionBar(submit));
}

/* ---------------------------------------------------------------------------------------------- start */
const refreshAll = async () => {
  if (mode === 'detail' && detailRefresh) { await detailRefresh(); return; }
  if (mode === 'list') await Promise.all(Object.values(refreshers).map((f) => f()));
};
const qid = new URLSearchParams(location.search).get('id');
if (qid) openTransfer(qid); else showList('IN_TRANSIT');
ERP.live.start('warehouse', { stock: refreshAll, '*': refreshAll });
