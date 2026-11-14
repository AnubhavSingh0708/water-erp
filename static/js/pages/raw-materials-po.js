// Purchase orders tab: list, create (lines in purchase units with live conversion echo), detail, cancel.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import * as L from '/js/pages/warehouse-lib.js';

const { el, icon } = ui;

const STATUS_FILTERS = [{ value: 'ACTIVE', label: 'Open' }, { value: 'RECEIVED', label: 'Received' }, { value: 'CANCELLED', label: 'Cancelled' }, { value: 'ALL', label: 'All' }];

export function mountPo(panel, ctx) {
  const host = el('div.stack');
  panel.append(host);
  let status = 'ACTIVE';
  let inForm = false;

  function showList() {
    inForm = false;
    const seg = ui.segmented({ options: STATUS_FILTERS, value: status, label: 'Purchase order status', onChange: (v) => { status = v; load(); } });
    const listHost = el('div');
    ui.mount(host, el('div.page-head', {}, el('div.seg-row', {}, seg.el), el('div.actions', {}, L.btn('New purchase order', 'plus', { onClick: () => showForm() }))), listHost);
    host._list = listHost; delete listHost.dataset.loaded;
    load();
  }
  async function load() {
    const lh = host._list; if (!lh || inForm) return;
    await L.refreshInto(lh, async () => {
      const rows = await api.get('/api/raw-materials/purchase-orders', { status: ['OPEN', 'PARTIAL', 'RECEIVED', 'CANCELLED'].includes(status) ? status : '' });
      const shown = status === 'ACTIVE' ? rows.filter((r) => r.status === 'OPEN' || r.status === 'PARTIAL') : rows;
      return ui.cardsTable({
        columns: [
          { label: 'Order', cls: 'k', render: (p) => [el('b.nowrap', { text: p.po_no }), el('small', { text: p.supplier_name })] },
          { label: 'Status', cls: 's', render: (p) => ui.badge(p.status) },
          { label: 'Ordered', render: (p) => ui.fmtDate(p.ordered_on) },
          { label: 'Expected', render: (p) => (p.expected_on ? ui.fmtDate(p.expected_on) : '-') },
          { label: 'Items', cls: 'num', render: (p) => `${p.lines_count} ${Number(p.lines_count) === 1 ? 'material' : 'materials'}` },
          { label: '', cls: 'a', render: (p) => el('div.btn-row', {}, L.btn('Open', 'list', { kind: 'secondary', sm: true, onClick: () => openPo(p.id) }),
            p.status === 'OPEN' || p.status === 'PARTIAL' ? L.btn('Receive goods', 'download', { kind: 'secondary', sm: true, onClick: () => ctx.goto('grn', { poId: p.id, supplierId: p.supplier_id }) }) : null) },
        ],
        rows: shown, onRow: (p) => openPo(p.id),
        emptyState: { icon: 'clipboard', title: status === 'ACTIVE' ? 'No open purchase orders' : 'No purchase orders here', text: 'Create one when you order materials, then receive the delivery against it.', action: { label: 'New purchase order', icon: 'plus', onClick: () => showForm() } },
      });
    });
  }

  /* ----------------------------------------------------------------------------------------- detail */
  async function openPo(id) {
    const body = el('div.stack', {}, ui.skeletonCards(2));
    const dlg = ui.openDialog({ title: 'Purchase order', body, wide: true });
    try {
      const p = await api.get(`/api/raw-materials/purchase-orders/${id}`);
      const canCancel = p.status === 'OPEN' && p.lines.every((l) => ui.toNum(l.qty_received) === 0);
      const open = p.status === 'OPEN' || p.status === 'PARTIAL';
      const acts = [];
      if (canCancel) acts.push(L.btn('Cancel order', 'ban', { kind: 'danger outline', onClick: async (ev) => {
        if (!(await ui.confirmDialog({ title: `Cancel ${p.po_no}?`, message: 'Nothing has been received yet. A cancelled order cannot be reopened.', confirmText: 'Cancel order', cancelText: 'Keep it', danger: true }))) return;
        try { await api.busy(ev.currentTarget, () => api.action().post(`/api/raw-materials/purchase-orders/${id}/cancel`, {})); ui.toast.ok(`${p.po_no} cancelled`); dlg.close(); load(); } catch (e) { L.errorDialog(e, 'Could not cancel'); if (e.code === 'INVALID_STATE') { dlg.close(); openPo(id); load(); } }
      } }));
      if (open) acts.push(L.btn('Receive goods', 'download', { kind: 'primary', onClick: () => { dlg.close(); ctx.goto('grn', { poId: p.id, supplierId: p.supplier_id }); } }));
      ui.mount(body, el('div.stack', {},
        L.kv([['Order', p.po_no], ['Supplier', p.supplier_name], ['Status', ui.badge(p.status)], ['Ordered on', ui.fmtDate(p.ordered_on)], ['Expected', p.expected_on ? ui.fmtDate(p.expected_on) : '-'], ['Notes', p.notes]]),
        ui.cardsTable({
          columns: [
            { label: 'Material', cls: 'k', render: (l) => [el('b', { text: l.name }), el('small', { text: l.code })] },
            { label: 'Ordered', cls: 'num', render: (l) => `${ui.fmtNum(l.qty_ordered, 4)} ${l.purchase_unit}` },
            { label: 'Received', cls: 'num', render: (l) => `${ui.fmtNum(l.qty_received, 4)} ${l.purchase_unit}` },
            { label: 'Still to come', cls: 'num', render: (l) => { const left = ui.toNum(l.qty_ordered) - ui.toNum(l.qty_received); return left > 0 ? `${ui.fmtNum(left, 4)} ${l.purchase_unit}` : ui.badge('RECEIVED', 'Complete'); } },
          ], rows: p.lines })));
      if (acts.length) { dlg.foot ? ui.mount(dlg.foot, acts) : dlg.el.append(el('div.dlg-foot', {}, acts)); }
    } catch (e) { ui.mount(body, ui.errorState(e, () => { dlg.close(); openPo(id); })); }
  }

  /* ----------------------------------------------------------------------------------------- form */
  async function showForm(prefill = {}) {
    inForm = true; L.toTop();
    ui.mount(host, ui.skeletonCards(2));
    let materials, suppliers;
    try { [materials, suppliers] = await Promise.all([L.refMaterials(), L.refSuppliers()]); } catch (e) { ui.mount(host, ui.errorState(e, () => showForm(prefill))); return; }
    const matById = new Map(materials.map((m) => [m.id, m]));
    const supF = ui.field({ label: 'Supplier', name: 'supplier_id', type: 'select', required: true, options: [{ value: '', label: 'Choose supplier...' }, ...suppliers.map((s) => ({ value: s.id, label: s.name }))] });
    const expF = ui.field({ label: 'Expected delivery', name: 'expected_on', type: 'date', optional: true, help: 'Optional - when the supplier promised to deliver.' });
    expF.input.min = ui.todayPlant();
    const notesF = ui.field({ label: 'Notes', name: 'notes', type: 'textarea', optional: true, rows: 2, maxlength: 500 });
    const linesHost = el('div.lines');
    const rows = [];
    const form = el('form.stack', { id: 'po-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }),
      el('div.card.stack', {}, el('div.form-grid', {}, supF.el, expF.el)),
      el('div.card.stack', {}, el('div.card-head', {}, el('h2', {}, icon('box'), 'What are you ordering?')),
        el('p.section-sub', { text: 'Enter quantities in the unit you buy in (bags, kg, litres...). We show the stock units next to it so you can check the number.' }),
        linesHost, L.btn('Add another material', 'plus', { kind: 'secondary', onClick: () => addLine() })),
      el('div.card', {}, notesF.el));
    const submit = L.btn('Create purchase order', 'check', { kind: 'primary', type: 'submit', form: 'po-form' });
    const cancel = L.btn('Cancel', null, { kind: 'ghost', onClick: () => leave() });
    const bar = L.actionBar(submit);

    function addLine(v = {}) {
      const mSel = ui.field({ label: 'Material', name: 'raw_material_id', type: 'select', options: [{ value: '', label: 'Choose material...' }, ...materials.map((m) => ({ value: m.id, label: L.materialLabel(m) }))], value: v.raw_material_id || '' });
      const q = L.unitInput({ name: 'qty_ordered', label: 'Quantity', unit: '', required: true });
      const card = el('div.line-card', {}, el('div.line-head', {}, el('span', { class: 'line-no', text: 'Material' }),
        el('button.btn.ghost.sm.icon', { type: 'button', 'aria-label': 'Remove this material', on: { click: () => { const i = rows.indexOf(row); if (rows.length > 1) { rows.splice(i, 1); card.remove(); renumber(); form.dispatchEvent(new Event('input', { bubbles: true })); } else { mSel.set(''); q.set(''); upd(); } } } }, icon('trash'))), mSel.el, q.el);
      const unitEl = q.field.el.querySelector('.unit');
      const upd = () => {
        const m = matById.get(mSel.get()); unitEl.textContent = m ? m.purchase_unit : '';
        const n = L.dec(q.get());
        if (!m) { q.echo(''); return; }
        if (q.get() === '') { q.echo(`1 ${m.purchase_unit} = ${ui.fmtNum(m.stock_per_purchase_unit, 4)} ${m.stock_unit}`, ''); return; }
        const prob = L.qtyProblem(q.get());
        if (prob) { q.echo(prob, 'bad'); return; }
        q.echo(`= ${ui.fmtNum(n * ui.toNum(m.stock_per_purchase_unit), 4)} ${m.stock_unit}`, 'ok');
      };
      mSel.input.addEventListener('change', () => { const m = matById.get(mSel.get()); if (m && !supF.get() && m.default_supplier_id) supF.set(m.default_supplier_id); upd(); });
      q.input.addEventListener('input', upd);
      const row = { card, mSel, q, upd, get: () => ({ raw_material_id: mSel.get(), qty: q.get() }), set: (s) => { mSel.set(s.raw_material_id || ''); q.set(s.qty || ''); upd(); } };
      rows.push(row); linesHost.append(card);
      if (v.qty !== undefined) q.set(String(ui.fmtNum(v.qty, 4)).replace(/,/g, ''));
      upd(); renumber();
      return row;
    }
    function renumber() { rows.forEach((r, i) => { r.card.querySelector('.line-no').textContent = `Material ${i + 1}`; }); }

    const getState = () => ({ supplier_id: supF.get(), expected_on: expF.get(), notes: notesF.get(), lines: rows.map((r) => r.get()) });
    const setState = (s) => {
      supF.set(s.supplier_id || ''); expF.set(s.expected_on || ''); notesF.set(s.notes || '');
      rows.splice(0).forEach((r) => r.card.remove());
      (s.lines && s.lines.length ? s.lines : [{}]).forEach((l) => addLine({ raw_material_id: l.raw_material_id }).set(l));
    };
    addLine();
    const hasPrefill = prefill && (prefill.materialId || prefill.supplierId);
    const draft = L.stateDraft({ page: 'raw-materials', id: 'po', form, get: getState, set: setState });
    if (hasPrefill) {
      if (prefill.supplierId) supF.set(prefill.supplierId);
      const r = rows[0]; r.set({ raw_material_id: prefill.materialId || '', qty: prefill.qty !== undefined ? String(ui.fmtNum(prefill.qty, 4)).replace(/,/g, '') : '' });
      if (prefill.materialId && !supF.get()) { const m = matById.get(prefill.materialId); if (m && m.default_supplier_id) supF.set(m.default_supplier_id); }
    } else draft.restore();

    function leave() { inForm = false; showList(); }
    form.addEventListener('submit', async (ev) => {
      ev.preventDefault(); ui.clearFormError(form);
      let bad = false; const lines = [];
      if (!supF.get()) { supF.setError('Choose the supplier'); bad = true; }
      if (expF.get() && expF.get() < ui.todayPlant()) { expF.setError('The expected date cannot be in the past'); bad = true; }
      const seen = new Set();
      rows.forEach((r) => {
        r.card.classList.remove('has-problem'); r.mSel.setError(''); r.q.setError('');
        const s = r.get(); let p = false;
        if (!s.raw_material_id) { r.mSel.setError('Choose a material'); p = true; }
        else if (seen.has(s.raw_material_id)) { r.mSel.setError('This material is already on the order - change its quantity instead'); p = true; }
        else seen.add(s.raw_material_id);
        const prob = L.qtyProblem(s.qty); if (prob) { r.q.setError(prob); p = true; }
        if (p) { r.card.classList.add('has-problem'); bad = true; } else lines.push({ raw_material_id: s.raw_material_id, qty_ordered: s.qty.trim() });
      });
      if (bad) { const first = form.querySelector('.has-error input, .has-error select'); if (first) { first.focus(); first.scrollIntoView({ block: 'center', behavior: 'smooth' }); } return; }
      const body = { supplier_id: supF.get(), lines, ...(expF.get() ? { expected_on: expF.get() } : {}), ...(notesF.get().trim() ? { notes: notesF.get().trim() } : {}) };
      const po = await L.submitPlain({ flow, form, button: submit, path: '/api/raw-materials/purchase-orders', body });
      if (po) { draft.clear(); ui.toast.ok(`Created ${po.po_no}`); leave(); openPo(po.id); }
    });
    const flow = L.makeFlow();
    ui.mount(host, L.backBar('Purchase orders', () => leave()), el('h2', { text: 'New purchase order' }), form, bar);
    bar.prepend(cancel);
  }

  ctx.refreshers.po = load;
  ctx.refreshers['po:open'] = (arg) => { if (arg.open) { showList(); openPo(arg.open); } else showForm(arg); };
  showList();
}
