// jars-actions.js - plant actions for returnable containers. Every action: form (draft kept) -> dry-run review -> confirm -> idempotent commit.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { ERP } from '/js/shell.js';
import { el, icon, run, dryRunReview } from '/js/pages/loading-common.js';

export const REASONS = {
  WASH_REJECT: 'Failed wash inspection', CRACKED: 'Cracked', LEAKING: 'Leaking', DAMAGED_RETURN: 'Damaged on return', AGE_RETIRED: 'Retired - too old', LOST: 'Lost', OTHER: 'Other',
};
const REASON_OPTS = Object.entries(REASONS).filter(([k]) => k !== 'WASH_REJECT').map(([value, label]) => ({ value, label }));

let custCache = null; let supCache = null;
export const customers = async () => (custCache ||= await api.get('/api/ref/customers'));
export const suppliers = async () => (supCache ||= await api.get('/api/ref/suppliers').catch(() => []));
export const clearRefCache = () => { custCache = null; supCache = null; };

const cOpts = (list, first) => [{ value: '', label: first || 'Choose customer...' }, ...list.map((c) => ({ value: c.id, label: `${c.name}${c.code ? ' (' + c.code + ')' : ''}` }))];

/** Concrete godown for writes ('all' is read-only). */
export function writeLocation(state) { const c = ERP.godown.current(); return c && c !== 'all' ? c : state.writeLoc || (ERP.godown.list[0] || {}).id || null; }

/**
 * actionDialog({id, title, icon, fields:[ui.field|qtyInput results], intro, path, product, loc, build(values)->body|null (null = invalid, errors set),
 *               review:{...}, confirmText, danger, submitLabel, done(res)})
 */
function actionDialog(o) {
  const act = api.action();
  const form = el('form.stack', { id: `ja-${o.id}`, novalidate: true }, el('div.form-error', { role: 'alert' }), o.intro ? el('p.small.muted', { text: o.intro }) : null, ...o.fields.map((f) => f.el || f), o.extra || null);
  const draft = ERP.drafts.bind(form, { page: 'jars', id: `act-${o.id}` });
  const submit = el('button.btn', { type: 'submit', form: `ja-${o.id}`, class: o.danger ? 'danger' : 'primary' }, el('span.btn-icon', {}, icon(o.icon || 'check')), o.submitLabel || 'Review');
  const cancel = el('button.btn.secondary', { type: 'button', on: { click: () => dlg.close() } }, 'Cancel');
  const dlg = ui.openDialog({ title: o.title, body: form, actions: [cancel, submit] });
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    const body = o.build(ui.readForm(form)); if (!body) return;
    if (o.loc) body.location_id = o.loc;
    const r = await run(submit, async () => {
      const rev = await dryRunReview({ path: o.path, body, review: { title: o.reviewTitle || o.title, intro: o.reviewIntro ? o.reviewIntro(body) : undefined, confirmText: o.confirmText || 'Confirm', danger: o.danger, unit: 'containers', confirmTitle: o.confirmTitle } });
      if (!rev.ok) return null;
      return act.postConfirm(o.path, rev.body);
    }, { form });
    if (r) { draft.clear(); dlg.close(true); if (o.done) o.done(r); }
  });
  return dlg;
}
const need = (f, v, msg) => { if (!v) { f.setError(msg); return false; } return true; };
const qty = (name, label, o = {}) => ui.qtyInput({ name, label, decimals: 0, unit: 'containers', ...o });
function qErr(q, msg) { q.el.classList.add('has-error'); q.error.textContent = msg; }
const posInt = (q, label, required = true) => {
  const v = q.value; if (v === null || q.raw.trim() === '') { if (required) { qErr(q, `Enter ${label}`); return false; } return true; }
  if (!q.valid || (required && v < 1)) { qErr(q, required ? 'Enter a whole number of 1 or more' : 'Enter a whole number'); return false; } return true;
};

/* ---------------------------------------------------------------------------------------------- the actions */
export async function openReceiveNew({ product, state, done }) {
  const sup = await suppliers();
  const q = qty('qty', 'How many new containers?', { required: true });
  const s = ui.field({ label: 'Supplier', name: 'supplier_id', type: 'select', optional: true, options: [{ value: '', label: 'Not recorded' }, ...sup.map((x) => ({ value: x.id, label: x.name }))] });
  const n = ui.field({ label: 'Note', name: 'note', type: 'text', optional: true, maxlength: 500, placeholder: 'Delivery challan, remarks...' });
  actionDialog({ id: 'receive-new', title: 'Receive new containers', icon: 'download', fields: [q, s, n], path: '/api/jars/receive-new', loc: writeLocation(state), confirmText: 'Receive containers',
    intro: `New ${product.name} containers join the fleet and go to the empty store (they still need washing).`,
    build: (v) => { if (!posInt(q, 'the quantity')) return null; const b = { product_id: product.product_id, qty: q.value }; if (v.supplier_id) b.supplier_id = v.supplier_id; if (v.note) b.note = v.note; return b; },
    done: (r) => { ui.toast.ok(`${r.qty} new containers received`); done(); } });
}
export function openWash({ product, state, done }) {
  const q = qty('qty', 'Washed - came out clean', { required: true, help: 'These move from empty to clean containers.' });
  const rj = qty('rejected', 'Rejected - failed inspection', { help: 'Optional. They are scrapped and counted as destroyed.' });
  rj.input.value = '0';
  const n = ui.field({ label: 'Note', name: 'note', type: 'text', optional: true, maxlength: 500 });
  const info = el('div.small.muted', { 'aria-live': 'polite' });
  const upd = () => { const t = (q.value || 0) + (rj.value || 0); ui.mount(info, `Taken from empty containers: ${ui.fmtNum(t, 0)} (${product.plant_empty} in stock).`); if (t > product.plant_empty) info.classList.add('warn-text'); else info.classList.remove('warn-text'); };
  q.input.addEventListener('input', upd); rj.input.addEventListener('input', upd);
  actionDialog({ id: 'wash', title: 'Record washing', icon: 'droplet', fields: [q, rj, info, n], path: '/api/jars/wash', loc: writeLocation(state), confirmText: 'Record wash',
    build: (v) => { if (!posInt(q, 'how many came out clean', false) || !posInt(rj, 'the rejected count', false)) return null; if (!(q.value || 0) && !(rj.value || 0)) { qErr(q, 'Enter how many were washed'); return null; }
      const b = { product_id: product.product_id, qty: q.value || 0 }; if (rj.value) b.rejected = rj.value; if (v.note) b.note = v.note; return b; },
    done: (r) => { ui.toast.ok(`${r.washed} washed${r.rejected ? `, ${r.rejected} rejected` : ''}`); done(); } });
  upd();
}
export async function openReceiveEmpties({ product, state, customer, holdings, done }) {
  const cl = await customers();
  const c = ui.field({ label: 'Customer', name: 'customer_id', type: 'select', required: true, value: customer || '', options: cOpts(cl) });
  const g = qty('qty_good', 'Good - back in the empty store', { required: true }); const d = qty('qty_damaged', 'Damaged - scrapped'); d.input.value = '0';
  const n = ui.field({ label: 'Note', name: 'note', type: 'text', optional: true, maxlength: 500 });
  const info = el('div.small.muted', { 'aria-live': 'polite' });
  const upd = () => { const h = holdings.get(c.get()); const t = (g.value || 0) + (d.value || 0);
    ui.mount(info, c.get() ? (h === undefined ? 'This customer holds none of these containers on record.' : `Holds ${h} now. After this: ${Math.max(0, h - t)}.`) : '');
    info.classList.toggle('warn-text', c.get() && (h === undefined ? t > 0 : t > h)); };
  [c.input, g.input, d.input].forEach((i) => i.addEventListener('input', upd)); c.input.addEventListener('change', upd);
  actionDialog({ id: 'receive-empties', title: 'Receive empties from a customer', icon: 'upload', fields: [c, g, d, info, n], path: '/api/jars/receive-empties', loc: writeLocation(state), confirmText: 'Receive empties',
    build: (v) => { if (!need(c, v.customer_id, 'Choose the customer')) return null; if (!posInt(g, 'good containers', false) || !posInt(d, 'damaged containers', false)) return null;
      if (!(g.value || 0) && !(d.value || 0)) { qErr(g, 'Enter how many came back'); return null; }
      const h = holdings.get(v.customer_id) || 0; if ((g.value || 0) + (d.value || 0) > h) { qErr(g, `This customer only holds ${h}. Record an opening balance first if they hold more.`); return null; }
      const b = { customer_id: v.customer_id, product_id: product.product_id, qty_good: g.value || 0, qty_damaged: d.value || 0 }; if (v.note) b.note = v.note; return b; },
    done: (r) => { ui.toast.ok('Empties received', { sub: `Customer now holds ${r.holding}` }); done(); } });
  upd();
}
export async function openIssue({ product, state, customer, done }) {
  const cl = await customers();
  const c = ui.field({ label: 'Customer', name: 'customer_id', type: 'select', required: true, value: customer || '', options: cOpts(cl) });
  const q = qty('qty', 'How many filled containers?', { required: true, help: `Ready filled stock: ${product.filled_ready}. Crates reserved by open loading trips cannot be sold here.` });
  const n = ui.field({ label: 'Note', name: 'note', type: 'text', optional: true, maxlength: 500 });
  actionDialog({ id: 'issue', title: 'Counter issue', icon: 'jar', fields: [c, q, n], path: '/api/jars/issue', loc: writeLocation(state), confirmText: 'Issue containers',
    intro: 'Filled containers handed over at the counter. They are taken from the oldest approved batch and added to what the customer holds.',
    build: (v) => { if (!need(c, v.customer_id, 'Choose the customer') || !posInt(q, 'the quantity')) return null; if (q.value > product.filled_ready) { qErr(q, `Only ${product.filled_ready} filled containers are ready.`); return null; }
      const b = { customer_id: v.customer_id, product_id: product.product_id, qty: q.value }; if (v.note) b.note = v.note; return b; },
    done: (r) => { ui.toast.ok(`${r.qty} containers issued`, { sub: `Customer now holds ${r.holding}` }); done(); } });
}
export async function openDestroy({ product, state, done }) {
  const cl = await customers();
  const loc = ui.field({ label: 'Where are they?', name: 'location', type: 'select', required: true, options: [{ value: 'PLANT_EMPTY', label: `At the plant - empty (${product.plant_empty})` }, { value: 'PLANT_CLEAN', label: `At the plant - clean (${product.plant_clean})` }, { value: 'CUSTOMER', label: 'At a customer (write-off)' }] });
  const c = ui.field({ label: 'Customer', name: 'customer_id', type: 'select', value: '', options: cOpts(cl) });
  const q = qty('qty', 'How many to destroy?', { required: true });
  const r = ui.field({ label: 'Reason', name: 'reason_code', type: 'select', required: true, options: [{ value: '', label: 'Choose a reason...' }, ...REASON_OPTS] });
  const n = ui.field({ label: 'Details', name: 'note', type: 'textarea', required: true, maxlength: 500, rows: 2, help: 'At least 5 characters - what happened? This goes in the audit log.' });
  const sync = () => { c.el.hidden = loc.get() !== 'CUSTOMER'; }; loc.input.addEventListener('change', sync); sync();
  actionDialog({ id: 'destroy', title: 'Destroy containers', icon: 'trash', danger: true, fields: [loc, c, q, r, n], path: '/api/jars/destroy', loc: writeLocation(state), confirmText: 'Destroy for good', confirmTitle: 'This cannot be undone',
    intro: 'Permanently removes containers from the fleet (broken, leaking, lost or too old).',
    build: (v) => { if (!need(r, v.reason_code, 'Choose the reason')) return null; if (v.location === 'CUSTOMER' && !need(c, v.customer_id, 'Choose the customer')) return null; if (!posInt(q, 'the quantity')) return null; if (v.note.trim().length < 5) { n.setError('Write at least 5 characters'); return null; }
      const b = { product_id: product.product_id, qty: q.value, location: v.location, reason_code: v.reason_code, note: v.note.trim() }; if (v.location === 'CUSTOMER') b.customer_id = v.customer_id; return b; },
    done: (x) => { ui.toast.ok(`${x.qty} containers destroyed`); done(); } });
}
export async function openOpening({ product, done }) {
  const cl = await customers();
  const c = ui.field({ label: 'Customer', name: 'customer_id', type: 'select', required: true, options: cOpts(cl) });
  const q = qty('qty', 'Containers the customer already holds', { required: true });
  const today = ui.todayPlant();
  const d = ui.field({ label: 'Handed out on', name: 'as_of', type: 'date', value: today, help: 'The real date they received them - the overdue clock starts here.' }); d.input.max = today;
  const n = ui.field({ label: 'Note', name: 'note', type: 'text', optional: true, maxlength: 500 });
  actionDialog({ id: 'opening', title: 'Opening balance', icon: 'clipboard', fields: [c, q, d, n], path: '/api/jars/opening', confirmText: 'Record balance',
    intro: 'Use this once per customer for containers that were already out before the system started.',
    build: (v) => { if (!need(c, v.customer_id, 'Choose the customer') || !posInt(q, 'the quantity')) return null; if (v.as_of > today) { d.setError('Cannot be in the future'); return null; }
      const b = { customer_id: v.customer_id, product_id: product.product_id, qty: q.value, as_of: v.as_of || today }; if (v.note) b.note = v.note; return b; },
    done: (r) => { ui.toast.ok('Opening balance recorded', { sub: `Customer holds ${r.holding}` }); done(); } });
}
