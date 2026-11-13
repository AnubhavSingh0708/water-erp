// crud.js - schema-driven CRUD list + create/edit dialog for the Admin console (and reusable by other pages).
//
//   const crud = createCrud(hostEl, {
//     entity: 'suppliers', singular: 'supplier', endpoint: '/api/admin/suppliers',
//     columns: [{ key: 'name', label: 'Name', cls: 'k' }, { key: 'phone', label: 'Phone' }],
//     fields:  [{ name: 'name', label: 'Name', type: 'text', required: true }, ...],
//     rowActions: [{ label: 'BOM', icon: 'list', onClick: (row, crud) => ... }],
//   });
//   crud.refresh();   // call from live-event handlers (keeps search/filter/scroll; never touches an open dialog)
//
// Field types: text | textarea | tel | password(create only) | int | decimal | select | refselect | bool | date | checks
// Field extras: required, nullable, help, placeholder, maxlength, unit, options[{value,label}], optionsFrom{url,value,label,filter},
//   showWhen(values)->bool, createOnly, readOnlyOnEdit, full (span both columns), default, min/max (client hint only), upper
// PUT bodies contain only CHANGED fields (backend PUT is partial and strict: never send id/created_at/*_name back).
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import * as drafts from '/js/drafts.js';

const { el, icon, toast } = ui;

const MEMO = new Map();
async function loadOptions(spec) {
  const hit = MEMO.get(spec.url);
  if (hit && Date.now() - hit.at < 20000) return hit.rows;
  const rows = await api.get(spec.url);
  const list = Array.isArray(rows) ? rows : (rows && rows.items) || [];
  MEMO.set(spec.url, { at: Date.now(), rows: list });
  return list;
}
export const clearOptionsCache = () => MEMO.clear();

const norm = {
  int: (s) => { const t = String(s ?? '').trim(); if (t === '') return null; return /^-?\d+$/.test(t) ? Number(t) : NaN; },
  decimal: (s) => { const t = String(s ?? '').trim().replace(/,/g, ''); if (t === '') return null; return /^-?\d+(\.\d+)?$/.test(t) ? t : NaN; },
};
const sameVal = (a, b) => {
  if (Array.isArray(a) || Array.isArray(b)) return JSON.stringify([...(a || [])].sort()) === JSON.stringify([...(b || [])].sort());
  if (a === null || a === undefined || a === '') return b === null || b === undefined || b === '';
  if (b === null || b === undefined || b === '') return false;
  const na = Number(a), nb = Number(b);
  if (typeof a !== 'boolean' && Number.isFinite(na) && Number.isFinite(nb) && String(a).trim() !== '' && String(b).trim() !== '') return na === nb;
  return String(a) === String(b);
};

/** Build the form controls for `fields`. Returns {form, read(), setOptions, refreshVisibility}. */
export async function buildFields(fields, row, { mode }) {
  const wrap = el('div.form-grid');
  const ctl = {};      // name -> {field, wrapper, getter}
  for (const f of fields) {
    if (mode === 'edit' && f.createOnly && !f.showOnEdit) continue;
    if (mode === 'create' && f.editOnly) continue;
    const cur = row ? row[f.name] : (typeof f.default === 'function' ? f.default() : f.default);
    const base = { name: f.name, label: f.label, help: f.help, required: f.required, optional: !f.required && f.showOptional !== false && f.type !== 'bool' && f.type !== 'checks' };
    let c;
    if (f.type === 'bool') {
      const input = el('input', { type: 'checkbox', name: f.name, checked: !!cur, id: `f-${f.name}` });
      const root = el('div.field', { dataset: { name: f.name } }, el('label.switch', { for: `f-${f.name}` }, input, el('span.track'), el('span', { text: f.label })), f.help ? el('p.help', { text: f.help }) : null, el('p.error'));
      c = { root, get: () => input.checked, input };
    } else if (f.type === 'checks') {
      const opts = f.options || (f.optionsFrom ? (await loadOptions(f.optionsFrom).catch(() => [])).filter(f.optionsFrom.filter || (() => true)).map((r) => ({ value: r[f.optionsFrom.value || 'id'], label: typeof f.optionsFrom.label === 'function' ? f.optionsFrom.label(r) : r[f.optionsFrom.label || 'name'], sub: f.optionsFrom.sub ? f.optionsFrom.sub(r) : '' })) : []);
      const have = new Set((cur || []).map(String));
      const boxes = opts.map((o) => el('input', { type: 'checkbox', name: f.name, value: o.value, 'data-group': f.name, checked: have.has(String(o.value)) }));
      const root = el('fieldset.field', { dataset: { name: f.name } }, el('legend.label', { text: f.label }),
        f.help ? el('p.help', { text: f.help }) : null,
        opts.length ? el('div.checks.cols', {}, opts.map((o, i) => el('label.check', {}, boxes[i], el('span', {}, o.label, o.sub ? el('span.sub', { text: o.sub }) : null)))) : el('p.muted.small', { text: f.emptyText || 'Nothing to choose from yet.' }),
        el('p.error'));
      c = { root, get: () => boxes.filter((b) => b.checked).map((b) => b.value), input: boxes[0] };
    } else {
      let type = f.type; let options = f.options;
      if (type === 'refselect') {
        const spec = f.optionsFrom; let rows = [];
        try { rows = (await loadOptions(spec)).filter(spec.filter || (() => true)); } catch { rows = []; }
        const keep = row && row[f.name] ? row[f.name] : null;
        options = [{ value: '', label: f.blankLabel || (f.required ? 'Choose...' : 'None') }, ...rows.map((r) => ({ value: r[spec.value || 'id'], label: typeof spec.label === 'function' ? spec.label(r) : r[spec.label || 'name'] }))];
        if (keep && !options.some((o) => o.value === keep)) options.push({ value: keep, label: `${(row[f.name.replace(/_id$/, '_name')] || 'Current (inactive)')}` });
        type = 'select';
      }
      const isNum = type === 'int' || type === 'decimal';
      const fld = ui.field({
        ...base, type: type === 'password' ? 'password' : isNum ? type : (type === 'select' ? 'select' : type === 'textarea' ? 'textarea' : type === 'date' ? 'date' : type === 'tel' ? 'tel' : 'text'),
        options, value: cur ?? (type === 'select' ? '' : ''), placeholder: f.placeholder, maxlength: f.maxlength, unit: f.unit, disabled: (mode === 'edit' && f.readOnlyOnEdit) || undefined,
        autocomplete: type === 'password' ? 'new-password' : 'off', inputmode: f.inputmode, rows: f.rows,
      });
      if (f.type === 'password') fld.input.dataset.nodraft = '1';
      if (f.upper) fld.input.addEventListener('input', () => { const p = fld.input.selectionStart; fld.input.value = fld.input.value.toUpperCase(); try { fld.input.setSelectionRange(p, p); } catch { /* ignore */ } });
      c = { root: fld.el, get: () => fld.get(), input: fld.input };
    }
    if (f.full || f.type === 'textarea' || f.type === 'checks') c.root.classList.add('full');
    ctl[f.name] = { f, ...c };
    wrap.append(c.root);
  }
  const values = () => { const v = {}; for (const [n, c] of Object.entries(ctl)) v[n] = c.get(); return v; };
  const refreshVisibility = () => {
    const v = values();
    for (const c of Object.values(ctl)) { const show = c.f.showWhen ? !!c.f.showWhen(v) : true; c.root.hidden = !show; c.hidden = !show; }
  };
  wrap.addEventListener('input', refreshVisibility); wrap.addEventListener('change', refreshVisibility);
  refreshVisibility();
  return { el: wrap, ctl, values, refreshVisibility };
}

/** Convert form values to a request body. create: omit empties; edit: diff vs the row. Returns {body, errors}. */
export function toBody(fields, ctl, row, mode) {
  const body = {}; const errors = {};
  for (const f of fields) {
    const c = ctl[f.name]; if (!c) continue;
    if (mode === 'edit' && (f.createOnly || f.readOnlyOnEdit)) continue;
    let v = c.get();
    const hidden = !!c.hidden;
    if (hidden) {
      // a hidden field must not keep a stale value on the record
      if (mode === 'edit' && f.nullable && row && row[f.name] !== null && row[f.name] !== undefined && f.type !== 'bool') body[f.name] = null;
      continue;
    }
    if (f.type === 'int' || f.type === 'decimal') {
      v = norm[f.type](v);
      if (Number.isNaN(v)) { errors[f.name] = f.type === 'int' ? 'Enter a whole number' : 'Enter a number (digits and one decimal point)'; continue; }
    } else if (f.type === 'select' || f.type === 'refselect' || f.type === 'date') { v = v === '' ? null : v; }
    else if (typeof v === 'string') { v = v.trim(); if (f.upper) v = v.toUpperCase(); }
    if (f.required && (v === null || v === '' || (Array.isArray(v) && f.requireOne && !v.length))) { errors[f.name] = 'This field is required'; continue; }
    if (f.type === 'decimal' && v !== null && f.min !== undefined && Number(v) < f.min) { errors[f.name] = `Must be at least ${f.min}`; continue; }
    if (f.type === 'int' && v !== null && ((f.min !== undefined && v < f.min) || (f.max !== undefined && v > f.max))) { errors[f.name] = `Enter a number between ${f.min ?? '-'} and ${f.max ?? '-'}`; continue; }
    if (mode === 'create') {
      if (v === null || v === '') continue;
      if (f.type === 'decimal' && f.sendAs === 'number') v = Number(v);
      body[f.name] = v;
    } else {
      const orig = row ? row[f.name] : undefined;
      if (sameVal(v, orig)) continue;
      if (v === null || v === '') body[f.name] = f.nullable ? null : (f.type === 'text' || f.type === 'textarea' || f.type === 'tel' ? '' : null);
      else body[f.name] = v;
    }
  }
  return { body, errors };
}

/** Open the create/edit dialog. cfg: {title, fields, endpoint, row?, onSaved(row), mapBody?(body, row), idPath?} */
export async function openRecordDialog({ cfg, row, mode, onSaved }) {
  const singular = cfg.singular || 'record';
  const form = el('form.stack', { novalidate: true, autocomplete: 'off', id: `crudform-${cfg.entity}` }, el('div.form-error', { role: 'alert' }));
  const built = await buildFields(cfg.fields, row, { mode });
  form.append(built.el);
  const save = el('button.btn.primary', { type: 'submit', form: form.id }, el('span.btn-icon', {}, icon('save')), mode === 'create' ? `Create ${singular}` : 'Save changes');
  const cancel = el('button.btn.secondary', { type: 'button' }, 'Cancel');
  const title = mode === 'create' ? `New ${singular}` : `Edit ${singular}${row && (row.name || row.username || row.reg_no || row.code) ? ': ' + (row.name || row.username || row.reg_no || row.code) : ''}`;
  const dlg = ui.openDialog({ title, body: form, actions: [cancel, save], wide: cfg.wide });
  cancel.addEventListener('click', () => dlg.close());
  const draft = drafts.bind(form, { page: 'crud', id: `${cfg.entity}.${mode === 'edit' && row ? row.id : 'new'}` });
  built.refreshVisibility();
  const act = api.action();
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    const { body, errors } = toBody(cfg.fields, built.ctl, row, mode);
    if (Object.keys(errors).length) { ui.formError(form, { status: 422, fields: errors, message: '' }); return; }
    if (mode === 'edit' && !Object.keys(body).length) { toast('No changes to save'); dlg.close(); return; }
    const payload = cfg.mapBody ? cfg.mapBody(body, row, mode) : body;
    await api.busy(save, async () => {
      try {
        const saved = mode === 'create' ? await act.post(cfg.endpoint, payload) : await act.put(`${cfg.endpoint}/${row.id}`, payload);
        draft.clear(); toast.ok(mode === 'create' ? `${cap(singular)} created` : 'Changes saved');
        dlg.close(saved); if (onSaved) onSaved(saved);
      } catch (e) {
        if (e.cancelled) return;
        if (e.network) save.lastChild.textContent = 'Not sent - Retry';
        ui.formError(form, e);
      }
    });
  });
  return dlg.closed;
}
const cap = (s) => s.charAt(0).toUpperCase() + s.slice(1);

/** Full list + toolbar. */
export function createCrud(host, cfg) {
  let rows = []; let q = ''; let active = cfg.defaultFilter || 'true'; let loadErr = null; let loaded = false;
  const searchIn = el('input.input', { type: 'search', placeholder: `Search ${cfg.title.toLowerCase()}...`, 'aria-label': `Search ${cfg.title}`, autocomplete: 'off', inputmode: 'search', maxlength: 100 });
  const filter = ui.segmented({ options: [{ value: 'true', label: 'Active' }, { value: 'false', label: 'Inactive' }, { value: 'all', label: 'All' }], value: active, label: 'Show', onChange: (v) => { active = v; refresh(); } });
  const addBtn = cfg.noCreate ? null : el('button.btn.primary', { type: 'button', on: { click: () => create() } }, icon('plus'), `New ${cfg.singular}`);
  const listEl = el('div', { 'aria-live': 'polite' });
  const countEl = el('span.muted.small');
  host.append(
    el('div.row.between', { style: { marginBottom: '12px' } },
      el('div.row.grow', {}, el('div.searchbar', {}, icon('search', 'sm'), searchIn), cfg.noFilter ? null : filter.el),
      el('div.row', {}, countEl, ...(cfg.headerActions || []).map((a) => el('button.btn.secondary', { type: 'button', on: { click: () => a.onClick(api_) } }, icon(a.icon || 'plus'), a.label)), addBtn)),
    listEl);
  searchIn.addEventListener('input', ui.debounce(() => { q = searchIn.value.trim(); refresh(); }, 300));

  const columns = () => {
    const cols = cfg.columns.map((c) => ({ ...c }));
    if (!cfg.noStatus) cols.push({ label: 'Status', cls: 's', render: (r) => (r.is_active === false ? ui.badge('INACTIVE') : ui.badge('ACTIVE')) });
    cols.push({
      label: '', cls: 'a', render: (r) => el('div.btn-row', {},
        el('button.btn.secondary.sm', { type: 'button', on: { click: () => edit(r) } }, icon('edit', 'sm'), 'Edit'),
        ...(cfg.rowActions || []).filter((a) => !a.show || a.show(r)).map((a) => el('button.btn.sm', { type: 'button', class: a.danger ? 'danger outline' : 'secondary', on: { click: (ev) => a.onClick(r, api_, ev.currentTarget) } }, icon(a.icon || 'list', 'sm'), typeof a.label === 'function' ? a.label(r) : a.label)),
        cfg.noDelete ? null : (r.is_active === false
          ? el('button.btn.secondary.sm', { type: 'button', on: { click: (ev) => reactivate(r, ev.currentTarget) } }, icon('refresh', 'sm'), 'Reactivate')
          : el('button.btn.danger.outline.sm', { type: 'button', on: { click: () => deactivate(r) } }, icon('ban', 'sm'), cfg.deactivateLabel || 'Deactivate'))),
    });
    return cols;
  };

  function paint() {
    ui.mount(listEl, loadErr ? ui.errorState(loadErr, () => refresh(true)) : ui.cardsTable({
      columns: columns(), rows, rowClass: (r) => (r.is_active === false ? 'inactive' : ''),
      emptyState: { icon: cfg.icon || 'package', title: q ? 'No matches' : `No ${cfg.title.toLowerCase()} yet`, text: q ? 'Try a different search.' : (cfg.emptyText || `Add your first ${cfg.singular} to get started.`), action: !q && addBtn ? { label: `New ${cfg.singular}`, icon: 'plus', onClick: () => create() } : null },
    }));
    countEl.textContent = loaded && !loadErr ? `${rows.length} ${rows.length === 1 ? cfg.singular : cfg.title.toLowerCase()}` : '';
  }
  async function refresh(showSkeleton) {
    if (!loaded || showSkeleton) ui.mount(listEl, ui.skeletonCards(3));
    try {
      const res = await api.get(cfg.endpoint, { q: q || undefined, active: active === 'all' ? undefined : active, limit: 500, ...(cfg.query || {}) });
      rows = Array.isArray(res) ? res : (res && res.items) || [];
      if (cfg.sort) rows.sort(cfg.sort);
      loadErr = null;
    } catch (e) { if (e.redirecting) return; loadErr = e; }
    loaded = true; paint();
  }
  async function create() { await openRecordDialog({ cfg, mode: 'create', onSaved: () => { MEMO.clear(); refresh(); if (cfg.onChange) cfg.onChange(); } }); }
  async function edit(row) { await openRecordDialog({ cfg, row, mode: 'edit', onSaved: () => { MEMO.clear(); refresh(); if (cfg.onChange) cfg.onChange(); } }); }
  async function deactivate(row) {
    const name = row.name || row.username || row.reg_no || row.code || 'this record';
    const ok = await ui.confirmDialog({
      title: `${cfg.deactivateLabel || 'Deactivate'} "${name}"?`, danger: true, confirmText: cfg.deactivateLabel || 'Deactivate',
      message: cfg.deactivateMessage ? cfg.deactivateMessage(row) : 'It disappears from selection lists. Existing history and documents stay unchanged and you can reactivate it later.',
    });
    if (!ok) return;
    try { await api.del(`${cfg.endpoint}/${row.id}`); toast.ok(`${cap(cfg.singular)} deactivated`); MEMO.clear(); refresh(); if (cfg.onChange) cfg.onChange(); }
    catch (e) { ui.toastError(e); }
  }
  async function reactivate(row, btn) {
    await api.busy(btn, async () => { try { await api.put(`${cfg.endpoint}/${row.id}`, { is_active: true }); toast.ok(`${cap(cfg.singular)} reactivated`); MEMO.clear(); refresh(); } catch (e) { ui.toastError(e); } });
  }
  const api_ = { refresh, rows: () => rows, edit, create, cfg };
  paint();
  return api_;
}
