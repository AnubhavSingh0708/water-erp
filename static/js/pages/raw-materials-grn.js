// Receive goods (GRN) tab and Supplier returns tab.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { ERP } from '/js/shell.js';
import * as L from '/js/pages/warehouse-lib.js';

const { el, icon } = ui;
const PAGE = 'raw-materials';

export function mountGrn(panel, ctx, which) { return which === 'returns' ? mountReturns(panel, ctx) : mountReceive(panel, ctx); }

/* ================================================================================================ GRN */
function grnDoc(g) {
  const body = el('div.stack.tight', {},
    L.kv([['GRN', g.grn_no], ['Supplier', g.supplier_name], ['Purchase order', g.po_no], ['Challan no', g.challan_no], ['Vehicle', g.vehicle_no], ['Godown', g.location_name],
      ['Received on', ui.fmtDate(g.received_on)], ['Received by', g.received_by_name], g.status === 'VOID' ? ['Status', `VOID - ${g.void_reason || ''}`] : null, ['Notes', g.notes]]),
    L.docTable([
      { h: 'Material', v: (l) => l.name }, { h: 'Qty received', num: true, v: (l) => `${ui.fmtNum(l.qty_purchase, 4)} ${l.purchase_unit}` },
      { h: 'In stock units', num: true, v: (l) => `${ui.fmtNum(l.qty_stock, 4)} ${l.stock_unit}` }, { h: 'Lot no', v: (l) => l.lot_no }, { h: 'Expiry', v: (l) => (l.expiry_date ? ui.fmtDate(l.expiry_date) : '') },
    ], g.lines),
    L.signatureBlock(['Store keeper', 'Driver / supplier', 'Checked by']));
  return body;
}
export function showGrn(g, { onVoided } = {}) {
  const acts = [];
  if (g.status === 'ACTIVE') {
    acts.push(L.btn('Void GRN', 'ban', { kind: 'danger outline', onClick: (ev) => voidGrn(g, ev.currentTarget, dlg, onVoided) }));
  }
  const dlg = L.showDocument({ title: 'Goods receipt note', docNo: g.grn_no, subtitle: g.status === 'VOID' ? 'VOID' : g.supplier_name, build: () => grnDoc(g), actions: acts, dlgTitle: `${g.grn_no}${g.status === 'VOID' ? ' (void)' : ''}` });
  return dlg;
}
async function voidGrn(g, button, dlg, onVoided) {
  const reason = await ui.promptDialog({ title: `Void ${g.grn_no}?`, label: 'Why is this GRN wrong?', help: 'Use this when the receipt was entered by mistake. The stock it added is removed again - only possible while that stock is still unused in the raw store.', min: 5, max: 300, confirmText: 'Review void', danger: true });
  if (!reason) return;
  const path = `/api/raw-materials/grns/${g.id}/void`;
  const flow = L.makeFlow();
  try {
    const d = await api.busy(button, () => api.dryRun(path, { reason }));
    const ok = await ui.reviewSheet({ title: `Void ${g.grn_no}`, summary: 'This takes the received stock out of the raw store again.', effects: L.prepEffects(d.effects, await L.unitMap().catch(() => null)), warnings: d.warnings, confirmText: 'Void this GRN', danger: true });
    if (!ok) return;
    await api.busy(button, () => flow.act.postConfirm(path, { reason, confirm: true }));
    ui.toast.ok(`${g.grn_no} voided`); dlg.close(); if (onVoided) onVoided();
  } catch (e) { L.errorDialog(e, 'GRN not voided'); }
}

function mountReceive(panel, ctx) {
  const host = el('div.stack');
  panel.append(host);
  const state = { status: 'ACTIVE', q: '', offset: 0 };
  let inForm = false; let listHost = null;

  function showList() {
    inForm = false; L.clearSub();
    const search = el('input.input', { type: 'search', placeholder: 'Search GRN, challan or supplier', 'aria-label': 'Search GRNs', maxlength: 100, value: state.q });
    const seg = ui.segmented({ options: [{ value: 'ACTIVE', label: 'Received' }, { value: 'VOID', label: 'Voided' }, { value: '', label: 'All' }], value: state.status, label: 'GRN status', onChange: (v) => { state.status = v; state.offset = 0; load(); } });
    search.addEventListener('input', ui.debounce(() => { state.q = search.value.trim(); state.offset = 0; load(); }, 350));
    listHost = el('div');
    ui.mount(host, el('div.page-head', {}, el('div', {}, el('p.sub', { text: 'Record every delivery when it arrives. Stock is added only when you confirm.' })), el('div.actions', {}, L.btn('Receive goods', 'plus', { onClick: () => showForm() }))),
      el('div.toolbar', {}, el('div.searchbar', {}, icon('search'), search), el('div.toolbar-end', {}, seg.el)), listHost);
    load();
  }
  async function load() {
    if (inForm || !listHost) return;
    await L.refreshInto(listHost, async () => {
      const d = await api.get('/api/raw-materials/grns', { limit: 15, offset: state.offset, status: state.status, q: state.q });
      return el('div.stack.tight', {},
        ui.cardsTable({
          columns: [
            { label: 'GRN', cls: 'k', render: (g) => [el('b.nowrap', { text: g.grn_no }), el('small', { text: g.supplier_name })] },
            { label: 'Status', cls: 's', render: (g) => ui.badge(g.status === 'ACTIVE' ? 'RECEIVED' : 'VOID', g.status === 'ACTIVE' ? 'Received' : 'Voided') },
            { label: 'Date', render: (g) => ui.fmtDate(g.received_on) }, { label: 'Challan', render: (g) => g.challan_no || '-' },
            { label: 'Order', hideMobile: true, render: (g) => g.po_no || '-' }, { label: 'Godown', hideMobile: !ERP.godown.multiple, render: (g) => g.location_name },
            { label: 'Items', cls: 'num', render: (g) => `${g.lines_count}` },
          ], rows: d.items, onRow: (g) => openGrn(g.id),
          emptyState: { icon: 'download', title: state.q || state.status ? 'No GRNs match' : 'No deliveries recorded yet', text: 'When a delivery arrives, press "Receive goods" and enter what the challan says.', action: { label: 'Receive goods', icon: 'plus', onClick: () => showForm() } },
        }),
        d.total > 15 ? ui.pager({ total: d.total, limit: 15, offset: state.offset, onPage: (o) => { state.offset = o; load(); } }) : null);
    });
  }
  async function openGrn(id) {
    try { const g = await api.get(`/api/raw-materials/grns/${id}`); showGrn(g, { onVoided: () => { load(); ctx.refreshers.stock && ctx.refreshers.stock(); } }); } catch (e) { ui.toastError(e); }
  }

  /* -------------------------------------------------------------------------------------------- form */
  async function showForm(prefill = {}) {
    inForm = true; L.enterSub(() => { inForm = false; showList(); }); L.toTop();
    ui.mount(host, ui.skeletonCards(2));
    let materials, suppliers;
    try { [materials, suppliers] = await Promise.all([L.refMaterials(), L.refSuppliers()]); } catch (e) { ui.mount(host, ui.errorState(e, () => showForm(prefill))); return; }
    const matById = new Map(materials.map((m) => [m.id, m]));
    const gd = L.godownField({ label: 'Receiving godown', name: 'location_id' });
    const supF = ui.field({ label: 'Supplier', name: 'supplier_id', type: 'select', required: true, options: [{ value: '', label: 'Choose supplier...' }, ...suppliers.map((s) => ({ value: s.id, label: s.name }))] });
    const poF = ui.field({ label: 'Purchase order', name: 'po_id', type: 'select', optional: true, options: [{ value: '', label: 'No purchase order (direct purchase)' }], help: 'Pick the order this delivery belongs to - the lines are filled in for you.' });
    const challanF = ui.field({ label: 'Challan / invoice no', name: 'challan_no', type: 'text', optional: true, maxlength: 60, help: 'Copy it exactly from the supplier challan. A challan can only be received once per supplier.' });
    const vehF = ui.field({ label: 'Vehicle no', name: 'vehicle_no', type: 'text', optional: true, maxlength: 30, placeholder: 'e.g. UP14 AB 1234' });
    const notesF = ui.field({ label: 'Notes', name: 'notes', type: 'textarea', optional: true, rows: 2, maxlength: 500 });
    const linesHost = el('div.lines'); const rows = []; let po = null; let poOptions = [];
    const poNote = el('div');

    const form = el('form.stack', { id: 'grn-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }),
      el('div.card.stack', {}, el('div.form-grid', {}, supF.el, poF.el, challanF.el, vehF.el, gd.el), poNote),
      el('div.card.stack', {}, el('div.card-head', {}, el('h2', {}, icon('box'), 'What arrived?')),
        el('p.section-sub', { text: 'Quantities are in the unit on the challan (bags, kg, litres...). We convert to stock units - check the "=" line against the bag count.' }),
        linesHost, L.btn('Add another material', 'plus', { kind: 'secondary', onClick: () => addLine() })),
      el('div.card', {}, notesF.el));
    const submit = L.btn('Review and receive', 'check', { kind: 'primary', type: 'submit', form: 'grn-form' });
    const flow = L.makeFlow();

    async function loadPos(keepPo) {
      const sup = supF.get(); poOptions = [];
      if (!sup) { fillPoSelect(keepPo); return; }
      try {
        const [a, b] = await Promise.all([api.get('/api/raw-materials/purchase-orders', { status: 'OPEN' }), api.get('/api/raw-materials/purchase-orders', { status: 'PARTIAL' })]);
        poOptions = [...a, ...b].filter((p) => p.supplier_id === sup);
      } catch { poOptions = []; }
      fillPoSelect(keepPo);
    }
    function fillPoSelect(keep) {
      ui.clear(poF.input);
      [{ value: '', label: poOptions.length ? 'No purchase order (direct purchase)' : (supF.get() ? 'No open orders for this supplier' : 'Choose a supplier first') }, ...poOptions.map((p) => ({ value: p.id, label: `${p.po_no} - ${p.status === 'PARTIAL' ? 'partly received' : 'open'}` }))]
        .forEach((o) => poF.input.append(el('option', { value: o.value, text: o.label })));
      poF.input.value = keep && poOptions.some((p) => p.id === keep) ? keep : '';
    }
    async function applyPo(id, { fill = true } = {}) {
      po = null; ui.clear(poNote);
      if (!id) { rows.forEach((r) => r.refreshMaterials()); return; }
      try { po = await api.get(`/api/raw-materials/purchase-orders/${id}`); } catch (e) { ui.toastError(e); return; }
      ui.mount(poNote, L.banner('info', 'clipboard', el('b', { text: `${po.po_no}: ` }), po.lines.map((l) => `${l.name} ${ui.fmtNum(ui.toNum(l.qty_ordered) - ui.toNum(l.qty_received), 4)} ${l.purchase_unit} still to come`).join('; ')));
      if (!fill) { rows.forEach((r) => r.refreshMaterials()); return; }
      const anyData = rows.some((r) => r.q.get() !== '' || r.lot.get() !== '');
      if (anyData && !(await ui.confirmDialog({ title: 'Replace the lines?', message: `Fill the lines from ${po.po_no} (what is still to come)? Your current lines will be replaced.`, confirmText: 'Replace lines' }))) return;
      rows.splice(0).forEach((r) => r.card.remove());
      const todo = po.lines.filter((l) => ui.toNum(l.qty_ordered) - ui.toNum(l.qty_received) > 0);
      (todo.length ? todo : po.lines).forEach((l) => addLine({ raw_material_id: l.raw_material_id, po_line_id: l.id, qty: Math.max(0, ui.toNum(l.qty_ordered) - ui.toNum(l.qty_received)) }));
    }

    function addLine(v = {}) {
      const mSel = ui.field({ label: 'Material', name: 'raw_material_id', type: 'select', options: [] });
      const q = L.unitInput({ name: 'qty_purchase', label: 'Quantity received', unit: '', required: true });
      const lot = ui.field({ label: 'Supplier lot / batch no', name: 'lot_no', type: 'text', optional: true, maxlength: 60 });
      const exp = ui.field({ label: 'Expiry date', name: 'expiry_date', type: 'date', optional: true }); exp.input.min = ui.todayPlant();
      const poInfo = el('div.chip-row');
      const card = el('div.line-card', {}, el('div.line-head', {}, el('span.line-no', { text: 'Material' }),
        el('button.btn.ghost.sm.icon', { type: 'button', 'aria-label': 'Remove this line', on: { click: () => removeRow() } }, icon('trash'))), mSel.el, q.el, poInfo, el('div.line-grid', {}, lot.el, exp.el));
      const unitEl = q.field.el.querySelector('.unit');
      const row = { card, mSel, q, lot, exp, po_line_id: v.po_line_id || null, refreshMaterials, upd, get: () => ({ raw_material_id: mSel.get(), qty: q.get(), lot_no: lot.get(), expiry_date: exp.get(), po_line_id: row.po_line_id }), set: (s) => { refreshMaterials(); mSel.set(s.raw_material_id || ''); q.set(s.qty || ''); lot.set(s.lot_no || ''); exp.set(s.expiry_date || ''); row.po_line_id = s.po_line_id || null; upd(); } };
      function removeRow() { if (rows.length > 1) { rows.splice(rows.indexOf(row), 1); card.remove(); renumber(); form.dispatchEvent(new Event('input', { bubbles: true })); } else { row.set({}); } }
      function refreshMaterials() {
        const cur = mSel.get();
        const list = po ? po.lines.map((l) => matById.get(l.raw_material_id)).filter(Boolean) : materials;
        const uniq = [...new Map(list.map((m) => [m.id, m])).values()];
        ui.clear(mSel.input);
        mSel.input.append(el('option', { value: '', text: 'Choose material...' }), ...uniq.map((m) => el('option', { value: m.id, text: L.materialLabel(m) })));
        mSel.input.value = uniq.some((m) => m.id === cur) ? cur : '';
      }
      function upd() {
        const m = matById.get(mSel.get()); unitEl.textContent = m ? m.purchase_unit : '';
        ui.clear(poInfo);
        const n = L.dec(q.get());
        if (!m) { q.echo(''); return; }
        if (po && !row.po_line_id) { const pl = po.lines.find((l) => l.raw_material_id === m.id); if (pl) row.po_line_id = pl.id; }
        if (q.get() === '') q.echo(`1 ${m.purchase_unit} = ${ui.fmtNum(m.stock_per_purchase_unit, 4)} ${m.stock_unit}`);
        else { const prob = L.qtyProblem(q.get()); if (prob) q.echo(prob, 'bad'); else q.echo(`= ${ui.fmtNum(n * ui.toNum(m.stock_per_purchase_unit), 4)} ${m.stock_unit} will be added to stock`, 'ok'); }
        const pl = po && po.lines.find((l) => l.id === row.po_line_id);
        if (pl) {
          const ordered = ui.toNum(pl.qty_ordered); const got = ui.toNum(pl.qty_received); const left = ordered - got;
          poInfo.append(el('span.chip-s', { text: `Ordered ${ui.fmtNum(ordered, 4)} - received ${ui.fmtNum(got, 4)} - left ${ui.fmtNum(Math.max(0, left), 4)} ${pl.purchase_unit}` }));
          if (n !== null) {
            if (got + n > ordered * 1.1) poInfo.append(el('span.chip-s.bad', {}, icon('alert', 'sm'), 'More than 110% of the order - the server will refuse this'));
            else if (n > left) poInfo.append(el('span.chip-s.warn', {}, icon('alert', 'sm'), 'More than the order - check the count'));
          }
        }
      }
      mSel.input.addEventListener('change', () => { row.po_line_id = null; upd(); });
      q.input.addEventListener('input', upd);
      refreshMaterials();
      if (v.raw_material_id) mSel.set(v.raw_material_id);
      if (v.qty !== undefined && v.qty !== '') q.set(String(ui.fmtNum(v.qty, 4)).replace(/,/g, ''));
      rows.push(row); linesHost.append(card); upd(); renumber();
      return row;
    }
    function renumber() { rows.forEach((r, i) => { r.card.querySelector('.line-no').textContent = `Material ${i + 1}`; }); }

    const getState = () => ({ location_id: gd.get(), supplier_id: supF.get(), po_id: poF.get(), challan_no: challanF.get(), vehicle_no: vehF.get(), notes: notesF.get(), lines: rows.map((r) => r.get()) });
    const setState = async (s) => {
      if (s.location_id) gd.field.set(s.location_id);
      supF.set(s.supplier_id || ''); challanF.set(s.challan_no || ''); vehF.set(s.vehicle_no || ''); notesF.set(s.notes || '');
      rows.splice(0).forEach((r) => r.card.remove());
      (s.lines && s.lines.length ? s.lines : [{}]).forEach((l) => addLine().set(l));
      await loadPos(s.po_id); poF.set(poF.input.value); if (poF.get()) await applyPo(poF.get(), { fill: false });
      rows.forEach((r, i) => { if (s.lines && s.lines[i]) r.set(s.lines[i]); });
    };
    const draft = L.stateDraft({ page: PAGE, id: 'grn', form, get: getState, set: (s) => { setState(s); } });

    supF.input.addEventListener('change', async () => { await loadPos(''); await applyPo('', {}); });
    poF.input.addEventListener('change', () => applyPo(poF.get()));
    addLine(); fillPoSelect('');
    if (prefill.poId) { supF.set(prefill.supplierId || ''); await loadPos(prefill.poId); poF.input.value = prefill.poId; await applyPo(prefill.poId); }
    else draft.restore();

    form.addEventListener('submit', L.once(async (ev) => {
      ev.preventDefault(); ui.clearFormError(form);
      let bad = false; const lines = [];
      if (!supF.get()) { supF.setError('Choose the supplier this delivery came from'); bad = true; }
      rows.forEach((r) => {
        r.card.classList.remove('has-problem'); r.mSel.setError(''); r.q.setError(''); r.exp.setError('');
        const s = r.get(); let p = false;
        if (!s.raw_material_id) { r.mSel.setError('Choose a material'); p = true; }
        const prob = L.qtyProblem(s.qty); if (prob) { r.q.setError(prob); p = true; }
        if (s.expiry_date && s.expiry_date < ui.todayPlant()) { r.exp.setError('This expiry date is already in the past - check the label'); p = true; }
        if (p) { r.card.classList.add('has-problem'); bad = true; }
        else lines.push({ raw_material_id: s.raw_material_id, qty_purchase: s.qty.trim(), ...(po && s.po_line_id ? { po_line_id: s.po_line_id } : {}), ...(s.lot_no.trim() ? { lot_no: s.lot_no.trim() } : {}), ...(s.expiry_date ? { expiry_date: s.expiry_date } : {}) });
      });
      if (bad) { const first = form.querySelector('.has-error input, .has-error select'); if (first) { first.focus(); first.scrollIntoView({ block: 'center', behavior: 'smooth' }); } return; }
      const loc = gd.get();
      const body = { supplier_id: supF.get(), lines, ...(loc ? { location_id: loc } : {}), ...(po ? { po_id: po.id } : {}), ...(challanF.get().trim() ? { challan_no: challanF.get().trim() } : {}),
        ...(vehF.get().trim() ? { vehicle_no: vehF.get().trim() } : {}), ...(notesF.get().trim() ? { notes: notesF.get().trim() } : {}) };
      const g = await L.submitDocument({ flow, form, button: submit, path: '/api/raw-materials/grns', body, title: 'Review this receipt', confirmText: 'Receive into stock',
        summary: 'Check the quantities below. They are added to the raw store as soon as you confirm.',
        onError: (e) => {
          if (e.code === 'DUPLICATE') { challanF.setError('This challan was already received from this supplier - look in the list before entering it again.'); challanF.input.focus(); }
          if (e.code === 'BAD_REQUEST' && /over-receipt/i.test(e.message || '')) form.querySelector('.line-card')?.classList.add('has-problem');
        } });
      if (g) {
        draft.clear(); ui.toast.ok(`Received ${g.grn_no}`); (g.warnings || []).forEach((w) => ui.toast.warn(w));
        inForm = false; showList(); showGrn(g, { onVoided: load }); ctx.refreshers.stock && ctx.refreshers.stock();
      }
    }));
    ui.mount(host, L.backBar('Receipts', () => { showList(); }), el('h2', { text: 'Receive goods' }), form, L.actionBar(submit));
  }

  ctx.refreshers.grn = load;
  ctx.refreshers['grn:open'] = (arg) => showForm(arg);
  showList();
}

/* ================================================================================================ supplier returns */
function rtvDoc(r) {
  return el('div.stack.tight', {},
    L.kv([['Return', r.rtv_no], ['Supplier', r.supplier_name], ['Against GRN', r.grn_no], ['Godown', r.location_name], ['Date', ui.fmtDateTime(r.created_at)], ['Prepared by', r.created_by_name], ['Note', r.note]]),
    L.docTable([{ h: 'Material', v: (l) => l.name }, { h: 'Quantity returned', num: true, v: (l) => `${ui.fmtNum(l.qty_stock, 4)} ${l.stock_unit}` }, { h: 'Lot', v: (l) => l.lot_no || '' }, { h: 'Reason', v: (l) => l.reason }], r.lines),
    L.signatureBlock(['Store keeper', 'Driver', 'Supplier signature']));
}
function showRtv(r) { return L.showDocument({ title: 'Return to supplier', docNo: r.rtv_no, subtitle: r.supplier_name, build: () => rtvDoc(r) }); }

function mountReturns(panel, ctx) {
  const host = el('div.stack'); panel.append(host);
  let inForm = false; let listHost = null; let offset = 0;
  function showList() {
    inForm = false; L.clearSub(); listHost = el('div');
    ui.mount(host, el('div.page-head', {}, el('div', {}, el('p.sub', { text: 'Send damaged or wrong goods back to the supplier. Stock is removed from the raw store.' })), el('div.actions', {}, L.btn('New return', 'plus', { onClick: () => showForm() }))), listHost);
    load();
  }
  async function load() {
    if (inForm || !listHost) return;
    await L.refreshInto(listHost, async () => {
      const d = await api.get('/api/raw-materials/supplier-returns', { limit: 15, offset });
      return el('div.stack.tight', {}, ui.cardsTable({
        columns: [
          { label: 'Return', cls: 'k', render: (r) => [el('b.nowrap', { text: r.rtv_no }), el('small', { text: r.supplier_name })] },
          { label: 'Date', render: (r) => ui.fmtDate(r.created_at) }, { label: 'Items', cls: 'num', render: (r) => `${r.lines_count}` },
          { label: '', cls: 'a', render: (r) => L.btn('Open / print', 'print', { kind: 'secondary', sm: true, onClick: () => openRtv(r.id) }) },
        ], rows: d.items, onRow: (r) => openRtv(r.id),
        emptyState: { icon: 'arrow-right', title: 'No returns yet', text: 'When goods must go back to a supplier, record it here so stock and the order stay right.', action: { label: 'New return', icon: 'plus', onClick: () => showForm() } },
      }), d.total > 15 ? ui.pager({ total: d.total, limit: 15, offset, onPage: (o) => { offset = o; load(); } }) : null);
    });
  }
  async function openRtv(id) { try { showRtv(await api.get(`/api/raw-materials/supplier-returns/${id}`)); } catch (e) { ui.toastError(e); } }

  async function showForm() {
    inForm = true; L.enterSub(() => showList()); L.toTop(); ui.mount(host, ui.skeletonCards(2));
    let suppliers; try { suppliers = await L.refSuppliers(); } catch (e) { ui.mount(host, ui.errorState(e, () => showForm())); return; }
    const gd = L.godownField({ label: 'Godown', name: 'location_id' });
    const supF = ui.field({ label: 'Supplier', name: 'supplier_id', type: 'select', required: true, options: [{ value: '', label: 'Choose supplier...' }, ...suppliers.map((s) => ({ value: s.id, label: s.name }))] });
    const grnF = ui.field({ label: 'Return goods from which receipt (GRN)?', name: 'grn_id', type: 'select', required: true, options: [{ value: '', label: 'Choose a supplier first' }], help: 'Pick the delivery the goods came with - its lots are listed below.' });
    const noteF = ui.field({ label: 'Note', name: 'note', type: 'textarea', optional: true, rows: 2, maxlength: 500 });
    const linesHost = el('div.lines'); let grn = null; const rows = [];
    const form = el('form.stack', { id: 'rtv-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }),
      el('div.card.stack', {}, el('div.form-grid', {}, supF.el, grnF.el, gd.el)),
      el('div.card.stack', {}, el('div.card-head', {}, el('h2', {}, icon('box'), 'What goes back?')), linesHost), el('div.card', {}, noteF.el));
    const submit = L.btn('Review and return', 'check', { kind: 'primary', type: 'submit', form: 'rtv-form' });
    const flow = L.makeFlow();
    async function loadGrns() {
      ui.clear(grnF.input); grn = null; ui.mount(linesHost, el('p.muted.small', { text: 'Choose the receipt to see its lots.' })); rows.length = 0;
      if (!supF.get()) { grnF.input.append(el('option', { value: '', text: 'Choose a supplier first' })); return; }
      try {
        const d = await api.get('/api/raw-materials/grns', { supplier_id: supF.get(), status: 'ACTIVE', limit: 100 });
        grnF.input.append(el('option', { value: '', text: d.items.length ? 'Choose the receipt...' : 'No receipts from this supplier' }), ...d.items.map((g) => el('option', { value: g.id, text: `${g.grn_no} - ${ui.fmtDate(g.received_on)}${g.challan_no ? ` - ${g.challan_no}` : ''}` })));
      } catch (e) { ui.toastError(e); }
    }
    async function loadGrn() {
      rows.length = 0; if (!grnF.get()) { ui.mount(linesHost, el('p.muted.small', { text: 'Choose the receipt to see its lots.' })); return; }
      try { grn = await api.get(`/api/raw-materials/grns/${grnF.get()}`); } catch (e) { ui.toastError(e); return; }
      if (grn.location_id && gd.field) gd.field.set(grn.location_id);
      ui.mount(linesHost, grn.lines.map((l) => {
        const use = el('input', { type: 'checkbox', name: 'use', 'aria-label': `Return ${l.name}` });
        const q = L.unitInput({ name: 'qty_stock', label: `Quantity to return (received ${ui.fmtNum(l.qty_stock, 4)} ${l.stock_unit})`, unit: l.stock_unit });
        const reason = ui.field({ label: 'Reason', name: 'reason', type: 'text', maxlength: 200, placeholder: 'e.g. damaged cartons' });
        const body = el('div.stack.tight', { hidden: true }, q.el, reason.el);
        use.addEventListener('change', () => { body.hidden = !use.checked; if (use.checked) q.input.focus(); });
        q.input.addEventListener('input', () => { const p = L.qtyProblem(q.get(), { max: ui.toNum(l.qty_stock) }); q.echo(q.get() === '' ? '' : p || `Returning ${ui.fmtNum(q.get(), 4)} of ${ui.fmtNum(l.qty_stock, 4)} ${l.stock_unit}`, p ? 'bad' : 'ok'); });
        const card = el('div.line-card', {}, el('label.check', {}, use, el('span', {}, el('b', { text: l.name }), el('span.sub', { text: `${l.lot_no ? `Lot ${l.lot_no} - ` : ''}received ${ui.fmtNum(l.qty_purchase, 4)} ${l.purchase_unit} (${ui.fmtNum(l.qty_stock, 4)} ${l.stock_unit})` }))), body);
        rows.push({ l, use, q, reason, card });
        return card;
      }));
    }
    const getState = () => ({ supplier_id: supF.get(), grn_id: grnF.get(), note: noteF.get(), lines: rows.map((r) => ({ use: r.use.checked, qty: r.q.get(), reason: r.reason.get() })) });
    const setState = async (s) => { supF.set(s.supplier_id || ''); await loadGrns(); grnF.set(s.grn_id || ''); await loadGrn(); noteF.set(s.note || ''); rows.forEach((r, i) => { const x = (s.lines || [])[i]; if (x) { r.use.checked = !!x.use; r.q.set(x.qty || ''); r.reason.set(x.reason || ''); r.card.querySelector('.stack.tight').hidden = !x.use; } }); };
    const draft = L.stateDraft({ page: PAGE, id: 'rtv', form, get: getState, set: (s) => { setState(s); } });
    supF.input.addEventListener('change', async () => { await loadGrns(); });
    grnF.input.addEventListener('change', loadGrn);
    form.addEventListener('submit', L.once(async (ev) => {
      ev.preventDefault(); ui.clearFormError(form);
      let bad = false; const lines = [];
      if (!supF.get()) { supF.setError('Choose the supplier'); bad = true; }
      if (!grnF.get()) { grnF.setError('Choose the receipt the goods came with'); bad = true; }
      rows.forEach((r) => {
        r.q.setError(''); r.reason.setError('');
        if (!r.use.checked) return;
        const prob = L.qtyProblem(r.q.get(), { max: ui.toNum(r.l.qty_stock) }); if (prob) { r.q.setError(prob); bad = true; }
        if (r.reason.get().trim().length < 3) { r.reason.setError('Give a short reason (at least 3 letters)'); bad = true; }
        if (!prob && r.reason.get().trim().length >= 3) lines.push({ raw_material_id: r.l.raw_material_id, lot_id: r.l.lot_id, qty_stock: r.q.get().trim(), reason: r.reason.get().trim() });
      });
      if (!bad && !lines.length) { ui.formError(form, { status: 422, message: 'Tick at least one lot to return.', fields: {} }); bad = true; }
      if (bad) return;
      const loc = gd.get();
      const body = { supplier_id: supF.get(), grn_id: grnF.get(), lines, ...(loc ? { location_id: loc } : {}), ...(noteF.get().trim() ? { note: noteF.get().trim() } : {}) };
      const r = await L.submitDocument({ flow, form, button: submit, path: '/api/raw-materials/supplier-returns', body, title: 'Review this return', confirmText: 'Return to supplier', summary: 'This removes the goods from the raw store.' });
      if (r) { draft.clear(); ui.toast.ok(`Created ${r.rtv_no}`); showList(); showRtv(r); ctx.refreshers.stock && ctx.refreshers.stock(); }
    }));
    ui.mount(host, L.backBar('Returns', () => showList()), el('h2', { text: 'Return goods to a supplier' }), form, L.actionBar(submit));
    loadGrns(); draft.restore();
  }
  ctx.refreshers.returns = load;
  showList();
}
