// admin-master.js - master-data schemas (suppliers, raw materials, products + BOM editor, customers, routes, vehicles,
// equipment, maintenance schedules) and godown (location) CRUD, all driven by js/crud.js.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { createCrud } from '/js/crud.js';

const { el, icon, toast } = ui;
const A = '/api/admin';
const MATERIAL_CATEGORIES = ['PREFORM', 'CAP', 'LABEL', 'SLEEVE', 'SHRINK_FILM', 'CARTON', 'MINERAL', 'CHEMICAL', 'EMPTY_BOTTLE', 'OTHER'];
const EQUIPMENT_KINDS = ['RO', 'UV', 'OZONATOR', 'BLOW_MOLDER', 'FILTER', 'COMPRESSOR', 'FILLER', 'OTHER'];
const opts = (list, label = (x) => x.replace(/_/g, ' ').toLowerCase().replace(/^\w/, (c) => c.toUpperCase())) => list.map((x) => ({ value: x, label: label(x) }));
const titled = (name, sub, nowrap) => [el('b', { class: nowrap ? 'nowrap' : '', text: name || '-' }), sub ? el('small', { text: sub }) : null];
const active = (url) => `${url}?active=true`;

/** document expiry badge for vehicles: worst of the four dates */
function docBadge(r) {
  const today = ui.todayPlant(); const lead = Number(ui.config.settings.expiry_lead_days ?? 30);
  const docs = [['Insurance', r.insurance_expiry], ['Fitness', r.fitness_expiry], ['PUC', r.puc_expiry], ['Permit', r.permit_expiry]].filter((d) => d[1]);
  if (!docs.length) return ui.badge('INFO', 'No dates', 'neutral');
  const expired = docs.filter((d) => d[1] < today);
  if (expired.length) return ui.badge('EXPIRED', `${expired[0][0]} expired`);
  const soon = docs.filter((d) => d[1] <= ui.addDays(today, lead));
  if (soon.length) return ui.badge('DUE_SOON', `${soon[0][0]} ${ui.fmtDate(soon[0][1])}`);
  return ui.badge('OK', 'Valid');
}
const dueBadge = (r) => {
  const today = ui.todayPlant(); if (!r.next_due_on) return '-';
  const lead = Number(ui.config.settings.expiry_lead_days ?? 30);
  return el('span.row', { style: { gap: '8px' } }, ui.fmtDate(r.next_due_on), r.next_due_on < today ? ui.badge('OVERDUE') : r.next_due_on <= ui.addDays(today, lead) ? ui.badge('DUE_SOON') : null);
};

export const SCHEMAS = {
  suppliers: {
    title: 'Suppliers', singular: 'supplier', endpoint: `${A}/suppliers`, icon: 'truck',
    columns: [{ label: 'Supplier', cls: 'k', render: (r) => titled(r.name, r.contact_person) }, { label: 'Phone', key: 'phone' }, { label: 'Email', key: 'email', hideMobile: true }, { label: 'Lead time', render: (r) => `${r.lead_time_days} days` }],
    fields: [
      { name: 'name', label: 'Name', type: 'text', required: true, maxlength: 120 }, { name: 'contact_person', label: 'Contact person', type: 'text', maxlength: 120 },
      { name: 'phone', label: 'Phone', type: 'tel', maxlength: 40 }, { name: 'email', label: 'Email', type: 'text', inputmode: 'email', maxlength: 120 },
      { name: 'lead_time_days', label: 'Lead time', type: 'int', unit: 'days', min: 0, max: 365, default: 7, help: 'Days between ordering and delivery - used by the reorder engine.' },
      { name: 'address', label: 'Address', type: 'textarea', maxlength: 500 },
    ],
  },
  'raw-materials': {
    title: 'Raw materials', singular: 'raw material', endpoint: `${A}/raw-materials`, icon: 'box',
    columns: [
      { label: 'Material', cls: 'k', render: (r) => titled(r.name, r.code) }, { label: 'Category', render: (r) => r.category.replace(/_/g, ' ').toLowerCase() },
      { label: 'Unit', render: (r) => `${r.stock_unit}${r.purchase_unit !== r.stock_unit || Number(r.stock_per_purchase_unit) !== 1 ? ` (1 ${r.purchase_unit} = ${ui.fmtNum(r.stock_per_purchase_unit, 4)} ${r.stock_unit})` : ''}` },
      { label: 'Min stock', cls: 'num', render: (r) => ui.fmtQty(r.min_stock, r.stock_unit) }, { label: 'Supplier', key: 'default_supplier_name', hideMobile: true },
    ],
    fields: [
      { name: 'code', label: 'Code', type: 'text', required: true, upper: true, maxlength: 40, help: 'Letters, digits, . _ / - (unique).' }, { name: 'name', label: 'Name', type: 'text', required: true, maxlength: 120 },
      { name: 'category', label: 'Category', type: 'select', required: true, options: [{ value: '', label: 'Choose...' }, ...opts(MATERIAL_CATEGORIES)], help: 'A preform is the small test-tube-shaped PET piece that is blown into a bottle.' },
      { name: 'default_supplier_id', label: 'Default supplier', type: 'refselect', nullable: true, optionsFrom: { url: active(`${A}/suppliers`) } },
      { name: 'stock_unit', label: 'Stock unit', type: 'text', required: true, default: 'pcs', maxlength: 20, help: 'How stock is counted (pcs, kg, litre).' },
      { name: 'purchase_unit', label: 'Purchase unit', type: 'text', required: true, default: 'pcs', maxlength: 20, help: 'How it is bought (box, bag, drum).' },
      { name: 'stock_per_purchase_unit', label: 'Stock units per purchase unit', type: 'decimal', required: true, default: '1', min: 0.0001, help: 'Conversion factor, e.g. 1 box = 5000 pcs -> 5000.' },
      { name: 'min_stock', label: 'Minimum stock', type: 'decimal', default: '0', min: 0, help: 'Below this the item shows LOW and a reorder is suggested.' },
      { name: 'unit_weight_g', label: 'Weight per piece', type: 'decimal', nullable: true, unit: 'g', min: 0, showWhen: (v) => v.category === 'PREFORM' || v.category === 'EMPTY_BOTTLE', help: 'Preform weight - used to turn scrap pieces into kg.' },
    ],
  },
  products: {
    title: 'Products', singular: 'product', endpoint: `${A}/products`, icon: 'package', wide: true,
    columns: [
      { label: 'Product', cls: 'k', render: (r) => titled(r.name, r.code) }, { label: 'Kind', render: (r) => (r.kind === 'RETURNABLE' ? 'Returnable jar' : 'Bottled') },
      { label: 'Pack', render: (r) => (r.kind === 'RETURNABLE' ? '1 jar per unit' : `${r.units_per_crate} per crate${r.crates_per_pallet ? ` - ${r.crates_per_pallet} crates/pallet` : ''}`) },
      { label: 'Shelf life', render: (r) => `${r.shelf_life_days} days`, hideMobile: true }, { label: 'Min stock', cls: 'num', render: (r) => `${ui.fmtNum(r.min_stock_crates, 0)} crates` },
    ],
    fields: [
      { name: 'code', label: 'Code', type: 'text', required: true, upper: true, maxlength: 40 }, { name: 'name', label: 'Name', type: 'text', required: true, maxlength: 120 },
      { name: 'kind', label: 'Kind', type: 'select', required: true, default: 'BOTTLED', options: [{ value: 'BOTTLED', label: 'Bottled (PET from preform)' }, { value: 'RETURNABLE', label: 'Returnable jar / container' }], full: true },
      { name: 'size_ml', label: 'Size', type: 'int', unit: 'ml', min: 1, nullable: true, help: 'Required for bottled products. 500, 1000 ... 20000 for a 20 L jar.' },
      { name: 'units_per_crate', label: 'Bottles per crate', type: 'int', min: 1, max: 10000, default: 12, required: true, showWhen: (v) => v.kind === 'BOTTLED' },
      { name: 'crates_per_pallet', label: 'Crates per pallet', type: 'int', min: 1, nullable: true, help: 'Standard pallet pattern. Shows "pallets + loose crates" everywhere.' },
      { name: 'shelf_life_days', label: 'Shelf life', type: 'int', unit: 'days', min: 1, max: 3650, default: 180, required: true },
      { name: 'min_stock_crates', label: 'Minimum finished stock', type: 'decimal', unit: 'crates', default: '0', min: 0 },
      { name: 'preform_material_id', label: 'Preform (blown into the bottle)', type: 'refselect', nullable: true, optionsFrom: { url: active(`${A}/raw-materials`), filter: (r) => r.category === 'PREFORM' }, showWhen: (v) => v.kind === 'BOTTLED' },
      { name: 'bottle_material_id', label: 'Empty bottle (blow-molding output)', type: 'refselect', nullable: true, optionsFrom: { url: active(`${A}/raw-materials`), filter: (r) => r.category === 'EMPTY_BOTTLE' }, showWhen: (v) => v.kind === 'BOTTLED' },
      { name: 'max_cycles', label: 'Retire after', type: 'int', unit: 'fills', min: 1, nullable: true, showWhen: (v) => v.kind === 'RETURNABLE', help: 'Maximum number of fills before a container is retired.' },
    ],
    mapBody(body, row, mode) {
      if (mode === 'create' && body.kind === 'RETURNABLE') delete body.units_per_crate;
      return body;
    },
    rowActions: [{ label: 'Recipe (BOM)', icon: 'clipboard', onClick: (row) => openBom(row) }],
  },
  customers: {
    title: 'Customers', singular: 'customer', endpoint: `${A}/customers`, icon: 'users',
    columns: [{ label: 'Customer', cls: 'k', render: (r) => titled(r.name, r.code) }, { label: 'Phone', key: 'phone' }, { label: 'Route', key: 'route_name' }, { label: 'Address', key: 'address', hideMobile: true }],
    fields: [
      { name: 'code', label: 'Code', type: 'text', required: true, upper: true, maxlength: 40 }, { name: 'name', label: 'Name', type: 'text', required: true, maxlength: 120 },
      { name: 'phone', label: 'Phone', type: 'tel', maxlength: 40 }, { name: 'route_id', label: 'Delivery route', type: 'refselect', nullable: true, optionsFrom: { url: active(`${A}/routes`) } },
      { name: 'address', label: 'Address', type: 'textarea', maxlength: 500 },
    ],
  },
  routes: {
    title: 'Routes', singular: 'route', endpoint: `${A}/routes`, icon: 'route',
    columns: [{ label: 'Route', cls: 'k', key: 'name' }, { label: 'Description', key: 'description' }],
    fields: [{ name: 'name', label: 'Name', type: 'text', required: true, maxlength: 120 }, { name: 'description', label: 'Description', type: 'textarea', maxlength: 500 }],
  },
  vehicles: {
    title: 'Vehicles', singular: 'vehicle', endpoint: `${A}/vehicles`, icon: 'truck', wide: true,
    columns: [
      { label: 'Vehicle', cls: 'k', render: (r) => titled(r.reg_no, r.name, true) }, { label: 'Capacity', cls: 'num', render: (r) => `${ui.fmtNum(r.capacity_crates, 0)} crates` },
      { label: 'Driver', render: (r) => [r.driver_name, r.driver_phone].filter(Boolean).join(' - ') }, { label: 'Documents', cls: 'w', render: docBadge },
    ],
    fields: [
      { name: 'reg_no', label: 'Registration no.', type: 'text', required: true, upper: true, maxlength: 20 }, { name: 'name', label: 'Description', type: 'text', maxlength: 120, placeholder: 'e.g. Tata 407' },
      { name: 'capacity_crates', label: 'Capacity', type: 'int', unit: 'crates', required: true, min: 1, max: 100000 },
      { name: 'driver_name', label: 'Driver', type: 'text', maxlength: 120 }, { name: 'driver_phone', label: 'Driver phone', type: 'tel', maxlength: 40 },
      { name: 'insurance_expiry', label: 'Insurance valid until', type: 'date', nullable: true }, { name: 'fitness_expiry', label: 'Fitness valid until', type: 'date', nullable: true },
      { name: 'puc_expiry', label: 'PUC valid until', type: 'date', nullable: true }, { name: 'permit_expiry', label: 'Permit valid until', type: 'date', nullable: true },
    ],
  },
  equipment: {
    title: 'Equipment', singular: 'equipment', endpoint: `${A}/equipment`, icon: 'wrench',
    columns: [{ label: 'Equipment', cls: 'k', render: (r) => titled(r.name, r.code) }, { label: 'Type', render: (r) => r.kind.replace(/_/g, ' ').toLowerCase() }, { label: 'Location', key: 'location' }],
    fields: [
      { name: 'code', label: 'Code', type: 'text', required: true, upper: true, maxlength: 40 }, { name: 'name', label: 'Name', type: 'text', required: true, maxlength: 120 },
      { name: 'kind', label: 'Type', type: 'select', required: true, options: [{ value: '', label: 'Choose...' }, ...opts(EQUIPMENT_KINDS)] }, { name: 'location', label: 'Location in plant', type: 'text', maxlength: 120 },
    ],
  },
  'maintenance-schedules': {
    title: 'Maintenance schedules', singular: 'schedule', endpoint: `${A}/maintenance-schedules`, icon: 'wrench',
    columns: [
      { label: 'Subject', cls: 'k', render: (r) => titled(r.subject_name, r.subject_type === 'VEHICLE' ? 'Vehicle' : 'Equipment') }, { label: 'Task', key: 'task' },
      { label: 'Every', render: (r) => (r.interval_days ? `${r.interval_days} days` : 'one-off') }, { label: 'Next due', cls: 'w', render: dueBadge },
    ],
    fields: [
      { name: 'subject_type', label: 'Applies to', type: 'select', required: true, default: 'EQUIPMENT', options: [{ value: 'EQUIPMENT', label: 'Plant equipment' }, { value: 'VEHICLE', label: 'Vehicle' }] },
      { name: 'equipment_id', label: 'Equipment', type: 'refselect', nullable: true, required: true, optionsFrom: { url: active(`${A}/equipment`), label: (r) => `${r.name} (${r.code})` }, showWhen: (v) => v.subject_type === 'EQUIPMENT' },
      { name: 'vehicle_id', label: 'Vehicle', type: 'refselect', nullable: true, required: true, optionsFrom: { url: active(`${A}/vehicles`), label: (r) => r.reg_no }, showWhen: (v) => v.subject_type === 'VEHICLE' },
      { name: 'task', label: 'Task', type: 'text', required: true, maxlength: 200, full: true, placeholder: 'e.g. Replace RO membrane' },
      { name: 'interval_days', label: 'Repeat every', type: 'int', unit: 'days', min: 1, max: 3650, nullable: true, help: 'Leave empty for a one-off task.' },
      { name: 'next_due_on', label: 'Next due', type: 'date', required: true }, { name: 'last_done_on', label: 'Last done', type: 'date', nullable: true },
    ],
  },
};
export const MASTER_ORDER = ['suppliers', 'raw-materials', 'products', 'customers', 'routes', 'vehicles', 'equipment', 'maintenance-schedules'];

export const LOCATIONS = {
  entity: 'locations', title: 'Godowns', singular: 'godown', endpoint: `${A}/locations`, icon: 'warehouse', deactivateLabel: 'Deactivate',
  deactivateMessage: () => 'A godown that still holds stock or has transfers in transit cannot be deactivated. History stays.',
  columns: [
    { label: 'Godown', cls: 'k', render: (r) => titled(r.name, r.code) }, { label: 'Type', render: (r) => (r.kind === 'PLANT' ? 'Plant' : 'Godown') },
    { label: 'City', key: 'city' }, { label: 'Contact', render: (r) => [r.contact_person, r.phone].filter(Boolean).join(' - ') },
    { label: 'Production', render: (r) => (r.is_production_site ? ui.badge('OK', 'Production site') : '-'), hideMobile: true },
  ],
  fields: [
    { name: 'code', label: 'Code', type: 'text', required: true, upper: true, maxlength: 40 }, { name: 'name', label: 'Name', type: 'text', required: true, maxlength: 120 },
    { name: 'kind', label: 'Type', type: 'select', required: true, default: 'GODOWN', options: [{ value: 'GODOWN', label: 'Godown (storage only)' }, { value: 'PLANT', label: 'Plant' }] },
    { name: 'is_production_site', label: 'Production site (batches and QC happen here)', type: 'bool', help: 'Raw materials are consumed from this site and finished goods land in its QC quarantine.' },
    { name: 'city', label: 'City', type: 'text', maxlength: 120 }, { name: 'phone', label: 'Phone', type: 'tel', maxlength: 40 },
    { name: 'contact_person', label: 'Contact person', type: 'text', maxlength: 120 }, { name: 'address', label: 'Address', type: 'textarea', maxlength: 500 },
  ],
};

/* --------------------------------------------------------------------------------------------- panes */
export function mountMaster(host) {
  const instances = {}; let cur = MASTER_ORDER[0];
  const picker = ui.segmented({ options: MASTER_ORDER.map((k) => ({ value: k, label: SCHEMAS[k].title })), value: cur, label: 'Master data type', onChange: show });
  const body = el('div.stack');
  host.append(el('div', { style: { overflowX: 'auto', marginBottom: '12px' } }, picker.el), body);
  function show(key) {
    cur = key; ui.clear(body);
    const sub = el('div'); body.append(sub);
    instances[key] = createCrud(sub, { ...SCHEMAS[key], entity: key });
    instances[key].refresh();
    try { sessionStorage.setItem('erp.admin.master', key); } catch { /* ignore */ }
  }
  let start = MASTER_ORDER[0]; try { const s = sessionStorage.getItem('erp.admin.master'); if (s && SCHEMAS[s]) start = s; } catch { /* ignore */ }
  picker.set(start); show(start);
  return { refresh: () => instances[cur] && instances[cur].refresh() };
}
export function mountGodowns(host) {
  const crud = createCrud(host, LOCATIONS); crud.refresh();
  return { refresh: () => crud.refresh() };
}

/* --------------------------------------------------------------------------------------------- BOM editor */
const ZONE_FOR = (cat) => (cat === 'EMPTY_BOTTLE' ? 'BLOW_FLOOR' : 'RM_STORE');
async function openBom(product) {
  let materials = []; let lines = [];
  try {
    const [m, b] = await Promise.all([api.get(`${A}/raw-materials`, { active: true, limit: 500 }), api.get(`${A}/products/${product.id}/bom`)]);
    materials = m; lines = b.map((l) => ({ raw_material_id: l.raw_material_id, qty: String(parseFloat(l.qty_per_crate)), basis: l.basis || 'PER_CRATE' }));
  } catch (e) { ui.toastError(e); return; }
  const byId = new Map(materials.map((m) => [m.id, m]));
  const returnable = product.kind === 'RETURNABLE';
  const choices = materials.filter((m) => !(returnable && (m.category === 'PREFORM' || m.category === 'EMPTY_BOTTLE')));
  const form = el('form.stack', { novalidate: true, id: 'bomform' }, el('div.form-error', { role: 'alert' }));
  const list = el('div.stack.tight');
  const intro = el('div.banner.info.inline', {}, icon('info'), el('div.grow', {},
    returnable ? 'Returnable containers never use preforms or empty bottles - only caps/seals, labels and minerals. The clean container itself is taken from the clean-container stock when a batch is made.'
      : 'List everything used per crate. The empty bottle is taken from the blow floor; everything else from the raw-material store. Minerals can be dosed per 1000 litres of water.'));
  const render = () => {
    ui.clear(list);
    if (!lines.length) list.append(ui.empty({ icon: 'clipboard', title: 'No ingredients yet', text: 'Add cap, label, shrink film, carton, minerals...' }));
    lines.forEach((l, i) => {
      const mat = byId.get(l.raw_material_id);
      const sel = el('select.select', { name: `lines[${i}].raw_material_id`, 'aria-label': 'Material' }, el('option', { value: '', text: 'Choose material...' }),
        choices.map((m) => el('option', { value: m.id, selected: m.id === l.raw_material_id, text: `${m.name} (${m.category.replace(/_/g, ' ').toLowerCase()})` })));
      sel.addEventListener('change', () => { l.raw_material_id = sel.value; render(); });
      const qty = el('input.input', { name: `lines[${i}].qty_per_crate`, type: 'text', inputmode: 'decimal', value: l.qty, 'aria-label': 'Quantity', placeholder: '0', autocomplete: 'off' });
      qty.addEventListener('input', () => { l.qty = qty.value; });
      const basis = el('select.select', { name: `lines[${i}].basis`, 'aria-label': 'Basis' },
        el('option', { value: 'PER_CRATE', selected: l.basis === 'PER_CRATE', text: 'per crate' }),
        el('option', { value: 'PER_1000L', selected: l.basis === 'PER_1000L', disabled: !product.size_ml, text: product.size_ml ? 'per 1000 L of water' : 'per 1000 L (set the size first)' }));
      basis.addEventListener('change', () => { l.basis = basis.value; render(); });
      const unit = mat ? `${mat.stock_unit} ${l.basis === 'PER_1000L' ? 'per 1000 L' : 'per crate'}` : '';
      list.append(el('div.card.tight.flat', {}, el('div.stack.tight', {},
        el('div.field', { dataset: { name: `lines[${i}].raw_material_id` } }, sel, el('p.error')),
        el('div.grid.cols-2', {},
          el('div.field', { dataset: { name: `lines[${i}].qty_per_crate` } }, qty, el('p.help', { text: unit || 'Quantity' }), el('p.error')),
          el('div.field', { dataset: { name: `lines[${i}].basis` } }, basis, mat ? el('p.help', { text: `Taken from the ${ZONE_FOR(mat.category) === 'BLOW_FLOOR' ? 'blow floor' : 'raw-material store'}` }) : null, el('p.error'))),
        el('div.row.end', {}, el('button.btn.danger.outline.sm', { type: 'button', on: { click: () => { lines.splice(i, 1); render(); } } }, icon('trash', 'sm'), 'Remove')))));
    });
  };
  const add = el('button.btn.secondary', { type: 'button', on: { click: () => { lines.push({ raw_material_id: '', qty: '', basis: 'PER_CRATE' }); render(); const last = list.lastElementChild; last && last.querySelector('select')?.focus(); } } }, icon('plus'), 'Add ingredient');
  form.append(intro, list, add);
  render();
  const save = el('button.btn.primary', { type: 'submit', form: 'bomform' }, el('span.btn-icon', {}, icon('save')), 'Save recipe');
  const cancel = el('button.btn.secondary', { type: 'button' }, 'Cancel');
  const dlg = ui.openDialog({ title: `Recipe: ${product.name}`, body: form, actions: [cancel, save], wide: true });
  cancel.addEventListener('click', () => dlg.close());
  const act = api.action();
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    const errs = {}; const body = [];
    lines.forEach((l, i) => {
      if (!l.raw_material_id) errs[`lines[${i}].raw_material_id`] = 'Choose a material';
      const q = String(l.qty).trim().replace(',', '.');
      if (!/^\d+(\.\d+)?$/.test(q) || Number(q) <= 0) errs[`lines[${i}].qty_per_crate`] = 'Enter a quantity above 0';
      const m = byId.get(l.raw_material_id);
      body.push({ raw_material_id: l.raw_material_id, qty_per_crate: q, basis: l.basis, source_zone: m ? ZONE_FOR(m.category) : 'RM_STORE' });
    });
    if (new Set(lines.map((l) => l.raw_material_id).filter(Boolean)).size !== lines.filter((l) => l.raw_material_id).length) errs[`lines[${lines.findIndex((l, i) => lines.findIndex((x) => x.raw_material_id === l.raw_material_id) !== i)}].raw_material_id`] = 'Each material can appear only once';
    if (Object.keys(errs).length) { ui.formError(form, { status: 422, fields: errs, message: '' }); return; }
    await api.busy(save, async () => {
      try { await act.put(`${A}/products/${product.id}/bom`, body); toast.ok('Recipe saved'); dlg.close(true); }
      catch (e) { if (e.network) save.lastChild.textContent = 'Not sent - Retry'; ui.formError(form, e); }
    });
  });
}
