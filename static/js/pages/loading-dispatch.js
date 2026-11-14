// loading-dispatch.js - dispatch notes (walk-in / direct sale of finished goods): list, new note with FEFO preview, printable note, void.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { ERP } from '/js/shell.js';
import { el, icon, cratesNode, crates, kv, run, explain, dryRunReview } from '/js/pages/loading-common.js';
import { docFrame, signatures } from '/js/pages/loading-gate.js';

/* ---------------------------------------------------------------------------------------------- list */
export async function mountDispatchList(host, ctx) {
  let status = 'ISSUED'; let q = ''; let offset = 0; let dead = false;
  const listEl = el('div.stack');
  const seg = ui.segmented({ label: 'Note status', value: status, options: [{ value: 'ISSUED', label: 'Issued' }, { value: 'VOID', label: 'Voided' }, { value: 'ALL', label: 'All' }], onChange: (v) => { status = v; offset = 0; paint(true); } });
  const search = el('input.input', { type: 'search', placeholder: 'Search note no., customer, vehicle', 'aria-label': 'Search dispatch notes', maxlength: 100, on: { input: ui.debounce(() => { q = search.value.trim(); offset = 0; paint(false); }, 350) } });
  const newBtn = el('button.btn.primary', { type: 'button', on: { click: () => ctx.go('dn-new') } }, el('span.btn-icon', {}, icon('plus')), 'New direct sale');
  host.append(el('div.page-head', {}, el('div', {}, el('p.sub', { text: 'Walk-in and counter sales of finished goods. Stock leaves oldest batch first.' })), el('div.actions', {}, newBtn)),
    el('div.toolbar', {}, el('div.searchbar', {}, icon('search', 'sm'), search), seg.el), listEl);
  async function paint(skeleton) {
    if (skeleton) ui.mount(listEl, ui.skeletonCards(3));
    try {
      const r = await api.get('/api/loading/dispatches', { ...ERP.godown.param(), status, q, limit: 30, offset });
      if (dead) return;
      ui.mount(listEl, ui.cardsTable({
        columns: [
          { label: 'Note', cls: 'k', render: (d) => [el('b', { text: d.dn_no }), el('small', { text: d.customer_name || d.party_name || '-' })] },
          { label: 'Status', cls: 's', render: (d) => ui.badge(d.status === 'ISSUED' ? 'DISPATCHED' : d.status, d.status === 'ISSUED' ? 'Issued' : 'Voided') },
          { label: 'Crates', cls: 'num', render: (d) => ui.fmtNum(d.total_crates, 0) },
          { label: 'Vehicle', render: (d) => d.vehicle_no || '-' },
          { label: 'When', render: (d) => ui.fmtDateTime(d.created_at) },
          { label: '', cls: 'a', render: (d) => el('div.btn-row', {}, el('button.btn.secondary.sm', { type: 'button', on: { click: () => ctx.go(`dn=${d.id}`) } }, icon('print', 'sm'), 'Open / print')) },
        ], rows: r.items, onRow: (d) => ctx.go(`dn=${d.id}`),
        emptyState: { icon: 'clipboard', title: q ? 'No notes match your search' : status === 'VOID' ? 'No voided notes' : 'No direct sales yet', text: 'Use this for a customer who collects stock at the dock without a delivery trip.', action: { label: 'New direct sale', icon: 'plus', onClick: () => ctx.go('dn-new') } },
      }), r.total > 30 ? ui.pager({ total: r.total, limit: 30, offset, onPage: (o) => { offset = o; paint(true); } }) : null);
    } catch (e) { if (!e.redirecting && !dead) ui.mount(listEl, ui.errorState(e, () => paint(true))); }
  }
  await paint(false);
  return { refresh: () => paint(false), destroy() { dead = true; } };
}

/* ---------------------------------------------------------------------------------------------- new note */
export async function mountDispatchNew(host, { ctx }) {
  const g = ERP.godown;
  const [availAll, customers] = await Promise.all([api.get('/api/loading/availability', { ...g.param() }), api.get('/api/ref/customers').catch(() => [])]);
  let avail = availAll.filter((a) => a.kind !== 'RETURNABLE');
  const lines = [];   // {product_id, product_name, batch_id|null, batch_no, crates, cpp}
  const back = el('button.btn.ghost.sm.back', { type: 'button', on: { click: () => ctx.go('dispatch') } }, icon('chevron-left', 'sm'), 'All direct sales');

  const cust = ui.field({ label: 'Customer', name: 'customer_id', type: 'select', value: '', options: [{ value: '', label: 'Walk-in / other party (type a name)' }, ...customers.map((c) => ({ value: c.id, label: `${c.name}${c.code ? ' (' + c.code + ')' : ''}` }))] });
  const party = ui.field({ label: 'Party name', name: 'party_name', type: 'text', maxlength: 120, required: true, placeholder: 'Who is collecting the goods?' });
  const veh = ui.field({ label: 'Vehicle number', name: 'vehicle_no', type: 'text', optional: true, maxlength: 40, autocapitalize: 'characters', placeholder: 'e.g. MH12 AB 1234' });
  const note = ui.field({ label: 'Note', name: 'note', type: 'textarea', optional: true, maxlength: 500, rows: 2 });
  const jarsData = el('textarea', { name: 'dnlines', hidden: true });
  const syncParty = () => { party.el.hidden = !!cust.get(); };
  cust.input.addEventListener('change', syncParty);

  const form = el('form.stack.loose', { id: 'dn-form', novalidate: true }, el('div.form-error', { role: 'alert' }), jarsData,
    el('section.card.stack', {}, el('div.card-head', {}, el('h2', {}, icon('users'), 'Who is it for?')), cust.el, party.el, el('div.form-grid', {}, veh.el, note.el)));
  const linesBox = el('div.stack.tight'); const addBox = el('div');
  form.append(el('section.card', {}, el('div.card-head', {}, el('h2', {}, icon('package'), 'Goods')), linesBox, addBox));
  const total = () => lines.reduce((s, l) => s + l.crates, 0);

  const freeOf = (pid, bid) => avail.filter((a) => a.product_id === pid && (!bid || a.batch_id === bid) && a.qc_status === 'APPROVED').reduce((s, a) => s + a.available, 0);
  function usedOf(pid, bid) { return lines.filter((l) => l.product_id === pid && (!bid || l.batch_id === bid || !l.batch_id)).reduce((s, l) => s + l.crates, 0); }

  /* line editor */
  const products = [...new Map(avail.filter((a) => a.qc_status === 'APPROVED').map((a) => [a.product_id, a])).values()];
  function buildAdder() {
    if (!products.length) { ui.mount(addBox, ui.empty({ icon: 'warehouse', title: 'No approved stock to sell', text: 'Nothing approved by QC is free in this godown right now.' })); return; }
    const pf = ui.field({ label: 'Product', name: 'ad_product', type: 'select', options: products.map((p) => ({ value: p.product_id, label: `${p.product_name} - ${crates(freeOf(p.product_id), p.crates_per_pallet)} free` })) });
    const bf = ui.field({ label: 'Batch', name: 'ad_batch', type: 'select', options: [], help: 'Oldest first is the safe default (FEFO).' });
    let q = null; const qbox = el('div'); const msg = el('div.ld-check', { role: 'status' });
    const addBtn = el('button.btn.secondary.block', { type: 'button', on: { click: add } }, icon('plus', 'sm'), 'Add to note');
    const cur = () => avail.find((a) => a.product_id === pf.get());
    function paintBatch() {
      const pid = pf.get(); const rows = avail.filter((a) => a.product_id === pid && a.qc_status === 'APPROVED' && a.available > 0);
      ui.mount(bf.input, [el('option', { value: '' }, 'Oldest first (automatic)'), ...rows.map((r) => el('option', { value: r.batch_id }, `${r.batch_no} - exp ${ui.fmtDate(r.expiry_date)} - ${crates(r.available, r.crates_per_pallet)} free`))]);
      q = ui.qtyInput({ name: 'ad_crates', label: 'How many crates?', cratesPerPallet: (cur() || {}).crates_per_pallet || 0, onInput: check });
      ui.mount(qbox, q.el); check();
    }
    function check() {
      ui.clear(msg); const n = q && q.value; if (!n || !q.valid) return;
      const free = freeOf(pf.get(), bf.get() || null) - usedOf(pf.get(), bf.get() || null);
      if (n > free) ui.mount(msg, el('div.banner.err.inline', {}, icon('ban'), el('div.grow', { text: `Only ${ui.fmtNum(Math.max(0, free), 0)} crates are free${bf.get() ? ' in that batch' : ''}${usedOf(pf.get(), bf.get() || null) ? ' (after the lines already on this note)' : ''}.` })));
    }
    function add() {
      const n = q.value; const p = cur(); if (!p) return;
      if (!n || !q.valid || n < 1) { q.error.textContent = 'Enter how many crates'; q.el.classList.add('has-error'); q.input.focus(); return; }
      const bid = bf.get() || null; const free = freeOf(p.product_id, bid) - usedOf(p.product_id, bid);
      if (n > free) { check(); return; }
      const b = bid ? avail.find((a) => a.batch_id === bid) : null;
      lines.push({ product_id: p.product_id, product_name: p.product_name, batch_id: bid, batch_no: b ? b.batch_no : '', crates: n, cpp: p.crates_per_pallet });
      paintLines(); paintBatch(); persist(); q.input.value = '';
    }
    pf.input.addEventListener('change', paintBatch); bf.input.addEventListener('change', check);
    ui.mount(addBox, el('div.add-line.stack.tight', {}, el('div.section-title', { text: 'Add goods' }), pf.el, bf.el, qbox, msg, addBtn));
    paintBatch();
  }
  function paintLines() {
    ui.mount(linesBox, lines.length ? el('ul.list.items', {}, lines.map((l, i) => el('li.item-row', {}, el('div.grow', {}, el('b', { text: l.product_name }), el('div.small.muted', { text: l.batch_id ? `Batch ${l.batch_no}` : 'Oldest batch first (automatic)' })),
      el('div.item-qty', {}, el('b', {}, cratesNode(l.crates, l.cpp)), el('button.btn.ghost.sm.icon', { type: 'button', 'aria-label': `Remove ${l.product_name} line`, on: { click: () => { lines.splice(i, 1); paintLines(); persist(); buildAdder(); } } }, icon('trash', 'sm'))))))
      : el('p.muted.small', { text: 'No goods added yet.' }));
    const n = total(); if (submit) { submit.disabled = !n; ui.mount(noteEl, n ? `${ui.fmtNum(n, 0)} crates on this note` : 'Add at least one line.'); }
  }
  function persist() { jarsData.value = JSON.stringify(lines); jarsData.dispatchEvent(new Event('input', { bubbles: true })); }

  const noteEl = el('span.note'); let submit = null;
  host.append(back, el('div.page-head', {}, el('div', {}, el('p.sub', { text: g.currentName() ? `Selling from ${g.currentName()}` : 'New direct sale' }))), form);
  const draft = ERP.drafts.bind(form, { page: 'loading', id: 'dn-new' });
  try { (JSON.parse(jarsData.value || '[]') || []).forEach((l) => { if (avail.some((a) => a.product_id === l.product_id)) lines.push(l); }); } catch { /* ignore */ }
  syncParty(); buildAdder();
  submit = el('button.btn.primary', { type: 'submit', form: 'dn-form' }, el('span.btn-icon', {}, icon('clipboard')), 'Review and issue');
  host.append(el('div.action-bar', {}, noteEl, submit)); paintLines();

  const act = api.action();
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    if (!cust.get() && !party.get().trim()) { party.setError('Type who is collecting the goods, or choose a customer'); party.input.focus(); return; }
    if (!lines.length) return;
    const body = { ...(g.param().location_id && g.param().location_id !== 'all' ? g.param() : {}), lines: lines.map((l) => (l.batch_id ? { batch_id: l.batch_id, crates: l.crates } : { product_id: l.product_id, crates: l.crates })) };
    if (cust.get()) body.customer_id = cust.get(); else body.party_name = party.get().trim();
    if (veh.get().trim()) body.vehicle_no = veh.get().trim(); if (note.get().trim()) body.note = note.get().trim();
    const who = cust.get() ? (customers.find((c) => c.id === cust.get()) || {}).name : party.get().trim();
    const r = await run(submit, async () => {
      const rev = await dryRunReview({ path: '/api/loading/dispatches', body, review: { title: 'Issue dispatch note?', intro: `Direct sale to ${who}: ${ui.fmtNum(total(), 0)} crates leave ready stock, oldest batch first:`, confirmText: 'Issue note' } });
      if (!rev.ok) return null;
      return act.postConfirm('/api/loading/dispatches', rev.body);
    }, { form, refresh: () => ctx.go('dn-new') });
    if (r) { draft.clear(); ui.toast.ok(`Dispatch note ${r.dn_no} issued`, { sub: 'Print it and give it to the customer.' }); ctx.justIssued = r.id; ctx.go(`dn=${r.id}`); }
  });
  void explain;
  return {};
}

/* ---------------------------------------------------------------------------------------------- printable note + void */
export async function mountDispatchDoc(host, { id, ctx }) {
  const back = el('button.btn.ghost.sm.back.no-print', { type: 'button', on: { click: () => ctx.go('dispatch') } }, icon('chevron-left', 'sm'), 'All direct sales');
  const render = async () => {
    const d = await api.get(`/api/loading/dispatches/${encodeURIComponent(id)}`);
    const t = d.totals || {};
    const header = ui.printHeader({ title: 'DISPATCH NOTE', docNo: d.dn_no, subtitle: d.location_name, date: ui.fmtDateTime(d.created_at) });
    const palletTotal = (d.lines || []).reduce((s, l) => s + (l.pallets || 0), 0), looseTotal = (d.lines || []).reduce((s, l) => s + (l.loose_crates ?? l.crates), 0);
    const body = el('div.stack.gp-body', {},
      el('div.gp-title', {}, el('h2', { text: 'Dispatch note' }), el('div.mono.gp-no', { text: d.dn_no }), el('div.small.muted', { text: `${d.plant_name || ''}${d.location_name ? ' - ' + d.location_name : ''}` })),
      d.status === 'VOID' ? el('div.banner.err.inline', {}, icon('ban'), el('div.grow', {}, el('b', { text: 'VOID' }), el('div.small', { style: { fontWeight: 500 }, text: d.void_reason || '' }))) : null,
      kv([['Customer / party', d.customer_name || d.party_name], ['Date', ui.fmtDateTime(d.created_at)], ['Issued by', d.created_by_name], ['Vehicle', d.vehicle_no], ['Note', d.note]]),
      ui.cardsTable({ columns: [
        { label: 'Product', cls: 'k', render: (l) => el('b', { text: l.product_name }) }, { label: 'Batch', render: (l) => l.batch_no }, { label: 'Expiry', render: (l) => ui.fmtDate(l.expiry_date) },
        { label: 'Quantity', cls: 'num w', render: (l) => cratesNode(l.crates, l.crates_per_pallet) }, { label: 'Bottles', cls: 'num', render: (l) => ui.fmtNum(l.crates * l.units_per_crate, 0) }], rows: d.lines || [] }),
      el('div.gp-totals', {}, el('div', {}, el('span.muted', { text: 'Total crates' }), el('b.big', { text: ui.fmtNum(t.crates, 0) })), el('div', {}, el('span.muted', { text: 'Pallets + loose' }), el('b.big', { text: `${ui.fmtNum(palletTotal, 0)} + ${ui.fmtNum(looseTotal, 0)}` })), el('div', {}, el('span.muted', { text: 'Bottles' }), el('b.big', { text: ui.fmtNum(t.bottles, 0) }))),
      signatures(['Issued by (store)', 'Received by (customer / driver)']));
    const voidBtn = d.status === 'ISSUED' ? el('button.btn.danger.outline', { type: 'button', on: { click: () => voidNote(d) } }, icon('ban'), 'Void note') : null;
    const banner = ctx.justIssued === id ? el('div.banner.ok.inline.no-print', {}, icon('check-circle'), el('div.grow', {}, el('b', { text: `Dispatch note ${d.dn_no} issued` }), el('div.small', { style: { fontWeight: 500 }, text: 'Print it now.' }))) : null; ctx.justIssued = null;
    const { root } = docFrame({ header, body, actions: voidBtn, banner, printTitle: d.dn_no });
    ui.mount(host, back, root);
  };
  async function voidNote(d) {
    const reason = await ui.promptDialog({ title: `Void ${d.dn_no}?`, label: 'Why is it being voided?', help: `${ui.fmtNum(d.total_crates, 0)} crates go back into stock (same batches). At least 5 characters.`, min: 5, danger: true, confirmText: 'Void note' });
    if (!reason) return;
    const act = api.action();
    const r = await run(null, () => act.post(`/api/loading/dispatches/${encodeURIComponent(id)}/void`, { reason }), { refresh: render });
    if (r) { ui.toast.ok(`${d.dn_no} voided`, { sub: 'The crates are back in stock.' }); await render(); }
  }
  await render();
  return { refresh: render };
}
