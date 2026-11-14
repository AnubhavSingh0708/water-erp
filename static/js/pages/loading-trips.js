// loading-trips.js - one trip: header + capacity, items on the trip, FEFO availability with a "load" sheet,
// gate pass (dry-run review -> confirm), cancel, void, on-the-road / settled / cancelled summaries.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { el, icon, cratesNode, crates, capacityBar, run, explain, dryRunReview, getVehicles, daysBetween } from '/js/pages/loading-common.js';

const DOCS = [['insurance_expiry', 'Insurance'], ['fitness_expiry', 'Fitness'], ['puc_expiry', 'PUC'], ['permit_expiry', 'Permit']];

export async function mountTrip(host, { id, ctx }) {
  let trip = null; let avail = []; let vehicle = null; let prev = new Map(); const changed = new Map();
  let dead = false; let sheet = null; let product = 'ALL'; let lastStatus = null;
  const strict = () => ui.config.settings.strict_qc_lock !== false;
  const lead = Number(ui.config.settings.expiry_lead_days) || 30;

  const back = el('button.btn.ghost.sm.back', { type: 'button', on: { click: () => ctx.go('trips') } }, icon('chevron-left', 'sm'), 'All trips');
  const noteBox = el('div.stack.tight.ld-notes', { 'aria-live': 'polite' });
  const headBox = el('div'); const itemsBox = el('div'); const availBox = el('div'); const barBox = el('div');
  host.append(back, noteBox, headBox, itemsBox, availBox, barBox);

  /* ------------------------------------------------------------ data */
  async function loadTrip() {
    const [t, vs] = await Promise.all([api.get(`/api/loading/trips/${encodeURIComponent(id)}`), getVehicles().catch(() => [])]);
    trip = t; vehicle = vs.find((v) => v.id === t.vehicle_id) || null;
  }
  async function loadAvail(mark) {
    if (!trip || trip.status !== 'LOADING') { avail = []; return; }
    const fresh = await api.get('/api/loading/availability', { trip_id: id, location_id: trip.location_id });
    if (mark) fresh.forEach((r) => { const p = prev.get(r.batch_id); if (p !== undefined && p !== r.available) { changed.set(r.batch_id, { from: p, to: r.available }); setTimeout(() => { changed.delete(r.batch_id); if (!dead) renderAvail(); }, 20000); } });
    prev = new Map(fresh.map((r) => [r.batch_id, r.available])); avail = fresh;
  }
  async function refresh(quiet) {
    try {
      const before = trip && trip.status;
      await loadTrip(); await loadAvail(!quiet);
      if (dead) return;
      if (before && before !== trip.status) { if (sheet) { sheet.close(); sheet = null; } banner(`This trip was changed by someone else - it is now ${trip.status}.`, 'warn'); }
      renderAll();
      if (sheet && sheet.update) sheet.update();
    } catch (e) { if (e.redirecting || dead) return; if (e.status === 404 || e.status === 403) { ui.mount(host, ui.errorState(e, () => ctx.go('trips'))); return; } if (e.network && trip) { banner('Server unreachable - showing the last data.', 'warn'); return; } throw e; }
  }
  function banner(text, tone = 'warn') {
    ui.clear(noteBox); noteBox.append(el('div.banner.inline', { class: tone }, icon(tone === 'ok' ? 'check-circle' : 'alert'), el('div.grow', { text }), el('button.btn.ghost.sm', { type: 'button', on: { click: () => ui.clear(noteBox) } }, 'OK')));
  }

  /* ------------------------------------------------------------ render */
  function renderAll() {
    renderHead(); renderItems(); renderAvail(); renderBar();
    if (trip.status !== lastStatus) { lastStatus = trip.status; if (trip.status === 'LOADING' && ctx.pendingQr && ctx.qrOn() && !sheet) { const c = ctx.pendingQr; ctx.pendingQr = null; openScan(c); } }
  }
  function docStrip() {
    if (!vehicle) return null;
    const today = ui.todayPlant();
    const list = DOCS.filter(([k]) => vehicle[k]).map(([k, l]) => ({ l, left: daysBetween(today, vehicle[k]), date: vehicle[k] }));
    const bad = list.filter((x) => x.left < 0), soon = list.filter((x) => x.left >= 0 && x.left <= lead);
    if (!bad.length && !soon.length) return null;
    return el('div.banner.inline', { class: bad.length ? 'err' : 'warn' }, icon(bad.length ? 'ban' : 'clock'), el('div.grow', {},
      el('b', { text: bad.length ? `${bad.map((x) => x.l).join(', ')} expired - the gate pass will be blocked` : 'Vehicle papers expire soon' }),
      el('div.small', { style: { fontWeight: 500 }, text: [...bad.map((x) => `${x.l} expired ${ui.fmtDate(x.date)}`), ...soon.map((x) => `${x.l} expires in ${x.left} day${x.left === 1 ? '' : 's'}`)].join(' - ') })));
  }
  function renderHead() {
    const t = trip;
    const cancelBtn = t.status === 'LOADING' ? el('button.btn.ghost.sm.danger-text', { type: 'button', on: { click: cancelTrip } }, icon('x-circle', 'sm'), 'Cancel trip') : null;
    ui.mount(headBox, el('section.card.trip-head', {},
      el('div.tc-top', {}, el('div.grow', {}, el('b.tc-veh', { text: t.vehicle_reg }), el('div.small.muted', { text: [t.trip_no, t.route_name, t.driver_name].filter(Boolean).join(' - ') }),
        el('div.small.muted', {}, icon('pin', 'sm'), ' ', t.location_name || '')), ui.badge(t.status, t.status === 'DISPATCHED' ? 'On the road' : undefined)),
      t.status === 'CANCELLED' ? el('div.banner.err.inline', {}, icon('ban'), el('div.grow', {}, el('b', { text: t.voided_at ? 'Gate pass voided' : 'Trip cancelled' }), t.void_reason ? el('div.small', { style: { fontWeight: 500 }, text: t.void_reason }) : null)) : capacityBar(t.loaded_crates, t.capacity_crates),
      t.status === 'LOADING' ? docStrip() : null,
      t.status === 'DISPATCHED' ? el('div.banner.info.inline', {}, icon('truck'), el('div.grow', {}, el('b', { text: `On the road since ${ui.fmtTime(t.gate_pass_issued_at)}` }), el('div.small', { style: { fontWeight: 500 }, text: `Gate pass ${t.gate_pass_no || ''} - ${ui.fmtRelative(t.gate_pass_issued_at)}` }))) : null,
      t.status === 'SETTLED' ? el('div.banner.ok.inline', {}, icon('check-circle'), el('div.grow', {}, el('b', { text: `Settled ${ui.fmtDateTime(t.settled_at)}` }), el('div.small', { style: { fontWeight: 500 }, text: `Gate pass ${t.gate_pass_no || ''}` }))) : null,
      cancelBtn ? el('div.row.end', {}, cancelBtn) : null));
  }

  function itemCard(it) {
    const settled = trip.status === 'SETTLED';
    return el('li.item-row', {},
      el('div.grow', {}, el('b', { text: it.product_name }),
        el('div.small.muted', { text: `${it.batch_no} - exp ${ui.fmtDate(it.expiry_date)}${it.set_label ? ` - label ${it.set_label}` : ''}` }),
        settled ? el('div.small', { text: `Delivered ${ui.fmtNum(it.crates_delivered, 0)} - returned ${ui.fmtNum(it.crates_returned, 0)} - damaged ${ui.fmtNum(it.crates_damaged, 0)}` }) : null,
        it.qc_status !== 'APPROVED' ? el('div', {}, ui.badge(it.qc_status, `${it.qc_status} - not QC approved`)) : null),
      el('div.item-qty', {}, el('b', {}, cratesNode(it.crates, it.crates_per_pallet)), trip.status === 'LOADING' ? el('button.btn.ghost.sm.icon', { type: 'button', 'aria-label': `Remove ${it.product_name} ${it.batch_no} from trip`, on: { click: () => removeItem(it) } }, icon('trash', 'sm')) : null));
  }
  function renderItems() {
    const items = trip.items || [];
    const total = items.reduce((s, i) => s + i.crates, 0);
    ui.mount(itemsBox, el('section.card', {}, el('div.card-head', {}, el('h2', {}, icon('package'), trip.status === 'LOADING' ? 'On this trip' : 'Load'), items.length ? el('span.count-pill', { text: String(items.length) }) : null),
      items.length ? el('ul.list.items', {}, items.map(itemCard)) : ui.empty({ icon: 'package', title: 'Nothing loaded yet', text: trip.status === 'LOADING' ? 'Pick stock below and tap Load.' : '' }),
      items.length ? el('div.item-total', {}, el('span', { text: 'Total' }), el('b', { text: `${ui.fmtNum(total, 0)} crates` })) : null,
      trip.status === 'DISPATCHED' || trip.status === 'SETTLED' ? el('div.btn-row.ld-doc-actions', {},
        el('button.btn.secondary', { type: 'button', on: { click: () => ctx.go(`gp=${trip.id}`) } }, icon('print'), 'Gate pass / print')) : null));
  }

  /* availability + load */
  function renderAvail() {
    if (trip.status !== 'LOADING') { ui.clear(availBox); return; }
    const products = [...new Map(avail.map((r) => [r.product_id, r.product_name])).entries()];
    if (product !== 'ALL' && !products.some(([pid]) => pid === product)) product = 'ALL';
    const rows = avail.filter((r) => product === 'ALL' || r.product_id === product);
    const firstOf = new Set(); const seen = new Set();
    avail.forEach((r) => { if (!seen.has(r.product_id) && r.available > 0) { seen.add(r.product_id); firstOf.add(r.batch_id); } });
    const scanBtn = ctx.qrOn() ? el('button.btn.secondary.sm', { type: 'button', on: { click: () => openScan() } }, icon('qr', 'sm'), 'Scan label') : null;
    ui.mount(availBox, el('section.card.pad0', {},
      el('div.card-head', {}, el('h2', {}, icon('warehouse'), 'Add stock'), scanBtn),
      el('div.av-wrap', {},
        !strict() ? el('div.banner.warn.inline', {}, icon('alert'), el('div.grow', { text: 'Strict QC lock is off: batches that are not yet approved are listed with a warning.' })) : null,
        products.length > 1 ? el('div.chips.av-filter', { role: 'group', 'aria-label': 'Filter by product' }, [['ALL', 'All products'], ...products].map(([pid, name]) => el('button.chip-btn', { type: 'button', 'aria-pressed': String(product === pid), on: { click: () => { product = pid; renderAvail(); } } }, name))) : null,
        rows.length ? el('div.av-list', {}, rows.map((r) => availRow(r, firstOf.has(r.batch_id)))) : ui.empty({ icon: 'warehouse', title: 'No ready stock to load', text: 'Nothing approved is free in this godown right now. Stock appears here as soon as QC approves a batch.' }))));
  }
  function availRow(r, oldest) {
    const ch = changed.get(r.batch_id); const none = r.available <= 0;
    return el('article.av-row', { class: [none ? 'none' : '', r.qc_status !== 'APPROVED' ? 'unapproved' : ''].join(' ').trim(), dataset: { batch: r.batch_id } },
      el('div.av-top', {}, el('div.grow', {}, el('b', { text: r.product_name }), el('div.small.muted', { text: `${r.batch_no} - exp ${ui.fmtDate(r.expiry_date)}` })),
        el('div.av-badges', {}, oldest ? ui.badge('INFO', 'Oldest first', 'info') : null, r.qc_status !== 'APPROVED' ? ui.badge(r.qc_status, `${r.qc_status} - not QC approved`) : null)),
      el('div.av-free', {}, el('div', {}, el('span.muted.small', { text: 'Free to load ' }), el('b.big', {}, cratesNode(r.available, r.crates_per_pallet))),
        el('div.small.muted', { text: [`${ui.fmtNum(r.fg_ready, 0)} on hand`, r.reserved ? `${ui.fmtNum(r.reserved, 0)} reserved by open trips` : '', r.in_trip ? `${ui.fmtNum(r.in_trip, 0)} on this trip` : ''].filter(Boolean).join(' - ') })),
      ch ? el('div.stock-changed', { role: 'status' }, icon('refresh', 'sm'), ` Stock just changed: ${ui.fmtNum(ch.from, 0)} -> ${ui.fmtNum(ch.to, 0)} free`) : null,
      el('button.btn.primary.block', { type: 'button', disabled: none, on: { click: () => openLoad(r.batch_id) } }, icon('plus', 'sm'), none ? 'Nothing free' : 'Load this batch'));
  }

  function openLoad(batchId) {
    const act = api.action();
    const row = () => avail.find((a) => a.batch_id === batchId);
    const r0 = row(); if (!r0) { ui.toast.warn('That batch is no longer available'); return; }
    const cpp = r0.crates_per_pallet || 0;
    const room = () => Math.max(0, trip.capacity_crates - trip.loaded_crates);
    const info = el('div.load-info', {});
    const q = ui.qtyInput({ name: 'crates', label: 'How many crates?', cratesPerPallet: cpp, required: true, onInput: () => check() });
    const quick = el('div.chips.quick', {});
    const msg = el('div.ld-check', { role: 'status', 'aria-live': 'polite' });
    const stale = el('div.stock-changed', { hidden: true, role: 'status' });
    const body = el('div.stack', {}, info, stale, q.el, quick, msg);
    const addBtn = el('button.btn.primary', { type: 'button', disabled: true, on: { click: submit } }, el('span.btn-icon', {}, icon('plus')), 'Add to trip');
    const cancel = el('button.btn.secondary', { type: 'button', on: { click: () => dlg.close() } }, 'Close');
    const dlg = ui.openDialog({ title: 'Load stock', body, actions: [cancel, addBtn] });
    sheet = { close: () => dlg.close(), update };
    dlg.closed.then(() => { if (sheet && sheet.close && sheet.dlg === dlg) sheet = null; });
    sheet.dlg = dlg;
    function quickBtn(label, n) { return el('button.chip-btn', { type: 'button', on: { click: () => { q.set(String(n)); q.input.dispatchEvent(new Event('input', { bubbles: true })); } } }, label); }
    function update() {
      const r = row();
      if (!r) { ui.mount(info, el('div.banner.err.inline', {}, icon('ban'), el('div.grow', { text: 'This batch is no longer available (sold, rejected or fully reserved).' }))); addBtn.disabled = true; return; }
      const ch = changed.get(batchId);
      stale.hidden = !ch; if (ch) ui.mount(stale, icon('refresh', 'sm'), ` Stock just changed: ${ui.fmtNum(ch.from, 0)} -> ${ui.fmtNum(ch.to, 0)} free - check your number`);
      ui.mount(info, el('div.stack.tight', {}, el('b.big', { text: r.product_name }), el('div.small.muted', { text: `${r.batch_no} - exp ${ui.fmtDate(r.expiry_date)}` }),
        el('div.row', {}, el('span', {}, 'Free now: ', el('b', {}, cratesNode(r.available, cpp))), el('span', {}, 'Vehicle room: ', el('b', { text: `${ui.fmtNum(room(), 0)} crates` }))),
        r.qc_status !== 'APPROVED' ? el('div.banner.warn.inline', {}, icon('alert'), el('div.grow', { text: 'Not approved by Lab QC yet. These crates would leave the plant before approval.' })) : null));
      ui.mount(quick, [cpp ? quickBtn('+1 pallet', (q.value || 0) + cpp) : null, quickBtn('+10', (q.value || 0) + 10), quickBtn(`Fill vehicle (${Math.min(r.available, room())})`, Math.min(r.available, room())), r.available > room() ? null : quickBtn('All free', r.available)]);
      check();
    }
    function check() {
      const r = row(); const n = q.value;
      ui.clear(msg); addBtn.disabled = true;
      if (!r) return;
      const lbl = addBtn.lastChild;
      if (!n || !q.valid) { lbl.textContent = 'Add to trip'; return; }
      lbl.textContent = `Add ${ui.fmtNum(n, 0)} crates`;
      const issues = []; const infos = [];
      if (n > r.available) issues.push(`Only ${ui.fmtNum(r.available, 0)} crates are free - enter at most ${ui.fmtNum(r.available, 0)}.`);
      if (n > room()) issues.push(`Over the vehicle capacity by ${ui.fmtNum(n - room(), 0)} crates - you can add at most ${ui.fmtNum(room(), 0)} more.`);
      if (!issues.length) {
        const after = trip.loaded_crates + n; const pct = Math.round((after * 100) / trip.capacity_crates);
        infos.push(`After adding: ${ui.fmtNum(after, 0)} of ${ui.fmtNum(trip.capacity_crates, 0)} crates (${pct}%).`);
        if (pct >= 95) infos.push('The vehicle will be almost full.');
      }
      ui.mount(msg, issues.map((t) => el('div.banner.err.inline', {}, icon('ban'), el('div.grow', { text: t }))), infos.map((t, i) => el('div.small', { class: i ? 'warn-text' : 'muted', text: t })));
      addBtn.disabled = issues.length > 0;
    }
    async function submit() {
      const r = row(); const n = q.value; if (!r || !n) return;
      if (r.qc_status !== 'APPROVED') {
        const ok = await ui.confirmDialog({ title: 'Load unapproved stock?', message: `${r.batch_no} is still ${r.qc_status} (Lab QC has not approved it).`, warnings: ['These crates would leave the plant before QC approval.'], confirmText: 'Yes, load it', cancelText: 'No, go back' });
        if (!ok) return;
      }
      const res = await run(addBtn, () => act.post(`/api/loading/trips/${encodeURIComponent(id)}/items`, { batch_id: batchId, crates: n }), { refresh: () => refresh(false) });
      if (res) {
        trip = res.trip; await loadAvail(false).catch(() => {}); renderAll();
        (res.warnings || []).forEach((w) => ui.toast.warn(w));
        ui.toast.ok(`Added ${crates(n, cpp)} - ${r.product_name}`, { sub: `${r.batch_no}. Vehicle ${ui.fmtNum(trip.loaded_crates, 0)} of ${ui.fmtNum(trip.capacity_crates, 0)} crates.` });
        dlg.close(true);
      }
    }
    update();
    q.input.focus();
  }

  async function removeItem(it) {
    const ok = await ui.confirmDialog({ title: 'Remove from trip?', message: `${crates(it.crates, it.crates_per_pallet)} of ${it.product_name} (${it.batch_no}) go back to free stock.`, confirmText: 'Yes, remove', cancelText: 'Keep it' });
    if (!ok) return;
    const act = api.action();
    const t = await run(null, () => act.del(`/api/loading/trips/${encodeURIComponent(id)}/items/${encodeURIComponent(it.id)}`), { refresh: () => refresh(false) });
    if (t) { trip = t; await loadAvail(false).catch(() => {}); renderAll(); ui.toast.ok('Removed from the trip'); }
  }

  /* ------------------------------------------------------------ bottom bar + actions */
  function renderBar() {
    ui.clear(barBox);
    if (trip.status === 'LOADING') {
      const n = trip.loaded_crates; const btn = el('button.btn.primary', { type: 'button', disabled: !n, on: { click: () => issue(btn) } }, el('span.btn-icon', {}, icon('truck')), n ? `Issue gate pass (${ui.fmtNum(n, 0)} crates)` : 'Issue gate pass');
      barBox.append(el('div.action-bar', {}, !n ? el('span.note', { text: 'Add at least one item to issue the gate pass.' }) : null, btn));
    } else if (trip.status === 'DISPATCHED') {
      const settle = el('button.btn.primary', { type: 'button', on: { click: () => ctx.go(`settle=${trip.id}`) } }, el('span.btn-icon', {}, icon('check-circle')), 'Settle trip');
      const voidBtn = el('button.btn.danger.outline', { type: 'button', on: { click: voidPass } }, icon('ban'), 'Void gate pass');
      barBox.append(el('div.action-bar', {}, voidBtn, settle));
    }
  }

  const gpAct = api.action();
  async function issue(btn) {
    if (!trip.items || !trip.items.length) return;
    const before = el('div.stack.tight', {},
      el('b', { text: `${trip.vehicle_reg} - ${trip.driver_name || 'no driver named'}` }),
      el('ul.list.compact', {}, trip.items.map((it) => el('li', {}, el('div.grow', {}, el('b', { text: it.product_name }), el('div.small.muted', { text: `${it.batch_no} - exp ${ui.fmtDate(it.expiry_date)}` })), el('b', {}, cratesNode(it.crates, it.crates_per_pallet))))),
      el('div.row.between', {}, el('span', { text: 'Total leaving' }), el('b', { text: `${ui.fmtNum(trip.loaded_crates, 0)} crates` })),
      vehicle ? vehicleDocs() : null);
    const r = await run(btn, async () => {
      const rev = await dryRunReview({ path: `/api/loading/trips/${encodeURIComponent(id)}/gate-pass`, body: {}, review: { title: 'Issue gate pass?', intro: 'This is exactly what will leave ready stock. It cannot be edited afterwards (only voided).', before, confirmText: 'Issue gate pass', unit: 'crates' } });
      if (!rev.ok) return null;
      return gpAct.postConfirm(`/api/loading/trips/${encodeURIComponent(id)}/gate-pass`, rev.body);
    }, { refresh: () => refresh(false) });
    if (r) { ui.toast.ok(`Gate pass ${r.gate_pass_no} issued`, { sub: 'Print it and hand it to the driver.' }); (r.warnings || []).forEach((w) => ui.toast.warn(w)); ctx.justIssued = id; ctx.go(`gp=${id}`); }
  }
  function vehicleDocs() {
    const today = ui.todayPlant();
    return el('div.chips', {}, DOCS.filter(([k]) => vehicle[k]).map(([k, l]) => { const left = daysBetween(today, vehicle[k]); return ui.badge(left < 0 ? 'OVERDUE' : left <= lead ? 'DUE_SOON' : 'OK', `${l}: ${left < 0 ? 'expired' : left <= lead ? `${left} d left` : 'valid'}`); }));
  }
  async function cancelTrip() {
    const reason = await ui.promptDialog({ title: `Cancel trip ${trip.trip_no}?`, label: 'Reason (optional)', help: 'All crates on this trip go back to free stock.', confirmText: 'Cancel trip', danger: true, min: 0 });
    if (reason === null) return;
    const act = api.action();
    const t = await run(null, () => act.post(`/api/loading/trips/${encodeURIComponent(id)}/cancel`, { reason }), { refresh: () => refresh(false) });
    if (t) { ui.toast.ok(`Trip ${t.trip_no} cancelled`); ctx.go('trips'); }
  }
  async function voidPass() {
    const reason = await ui.promptDialog({ title: `Void gate pass ${trip.gate_pass_no || ''}?`, label: 'Why is it being voided?', help: 'Wrong load, or the vehicle never left. At least 5 characters.', min: 5, danger: true, confirmText: 'Continue' });
    if (!reason) return;
    const act = api.action();
    const t = await run(null, () => act.postConfirm(`/api/loading/trips/${encodeURIComponent(id)}/void`, { reason }, { danger: true, confirmTitle: 'Void this gate pass?', confirmText: 'Yes, void it' }), { refresh: () => refresh(false) });
    if (t) { ui.toast.ok('Gate pass voided', { sub: 'The crates are back in stock.' }); trip = t; await refresh(true); }
  }
  async function openScan(code) { const m = await import('/js/pages/loading-scan.js'); sheet = m.openScanner({ trip, ctx, code, onAdded: async (res) => { if (res && res.trip) trip = res.trip; await loadAvail(false).catch(() => {}); renderAll(); }, refresh: () => refresh(false), onClose: () => { sheet = null; } }); }

  await loadTrip(); await loadAvail(false);
  prev = new Map(avail.map((r) => [r.batch_id, r.available]));
  renderAll();
  return { refresh: () => refresh(false), destroy() { dead = true; if (sheet && sheet.close) { try { sheet.close(); } catch { /* ignore */ } } } };
}
