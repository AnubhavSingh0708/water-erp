// admin-settings.js - Settings tab: grouped, typed inputs; saves only changed keys, all-or-nothing on the server.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';

const { el, icon, toast } = ui;

// label/help/unit/range per key (server validates the same ranges). Unknown keys fall back to a generic row.
const META = {
  'plant.name': { g: 'Plant', label: 'Plant name', t: 'text', max: 120 },
  'plant.code': { g: 'Plant', label: 'Plant code', t: 'text', max: 12, help: 'Short code used in batch numbers (letters, digits, - _).' },
  'plant.qr_base_url': { g: 'Plant', label: 'Public address for QR links', t: 'text', max: 200, help: 'Leave empty to use the address the browser opened. Example: https://erp.example.com' },
  'plant.utc_offset_minutes': { g: 'Plant', label: 'Plant time zone', t: 'int', unit: 'minutes from UTC', min: -720, max: 840, tz: true, help: 'India = 330 (UTC+05:30). All business dates (batch day, expiry, reports) use plant time.' },
  'features.qr_labels': { g: 'Features', label: 'QR crate labels', t: 'bool', help: 'Off by default: loading works on stock by product and batch. Switch on to print QR stickers and scan sets.' },
  'loading.strict_qc_lock': { g: 'Loading & dispatch', label: 'Strict QC lock', t: 'bool', help: 'On: only APPROVED batches can be loaded. Off: quarantined batches can be loaded with a visible warning.' },
  'loading.default_crate_set_size': { g: 'Loading & dispatch', label: 'Default QR set size', t: 'int', unit: 'crates', min: 1, max: 1000, help: 'Used when a product has no pallet size and QR labels are on.' },
  'loading.void_window_hours': { g: 'Loading & dispatch', label: 'Void window', t: 'int', unit: 'hours', min: 1, max: 720, help: 'How long after dispatch a non-admin may void a gate pass / dispatch note.' },
  'settle.damage_warn_pct': { g: 'Loading & dispatch', label: 'Damage warning at settlement', t: 'int', unit: '% of load', min: 0, max: 100, help: 'Settling a trip with more damaged crates than this asks for confirmation.' },
  'auth.session_ttl_hours': { g: 'Security', label: 'Session length', t: 'int', unit: 'hours of inactivity', min: 1, max: 2160 },
  'auth.session_inactivity_purge_days': { g: 'Security', label: 'Delete idle sessions after', t: 'int', unit: 'days', min: 1, max: 365 },
  'auth.max_failed_logins': { g: 'Security', label: 'Failed sign-ins before lock-out', t: 'int', min: 1, max: 100 },
  'auth.lockout_minutes': { g: 'Security', label: 'Lock-out time', t: 'int', unit: 'minutes', min: 1, max: 1440 },
  'inventory.burn_rate_lookback_days': { g: 'Inventory & reorder', label: 'Usage look-back', t: 'int', unit: 'days', min: 1, max: 365, help: 'Average daily usage is computed over this many days.' },
  'inventory.safety_stock_days': { g: 'Inventory & reorder', label: 'Safety stock', t: 'int', unit: 'days', min: 0, max: 90 },
  'inventory.worker_interval_minutes': { g: 'Inventory & reorder', label: 'Reorder check interval', t: 'int', unit: 'minutes', min: 1, max: 1440 },
  'entry.large_adjust_pct': { g: 'Entry checks', label: 'Large adjustment threshold', t: 'int', unit: '% of balance', min: 1, max: 1000, help: 'Bigger stock adjustments/transfers must be confirmed explicitly.' },
  'production.scrap_warn_pct': { g: 'Production & lab', label: 'Blow-molding scrap warning', t: 'int', unit: '%', min: 0, max: 100, help: 'A blow run above this needs a note.' },
  'lab.ph_min': { g: 'Production & lab', label: 'pH minimum', t: 'dec', min: 0, max: 14 }, 'lab.ph_max': { g: 'Production & lab', label: 'pH maximum', t: 'dec', min: 0, max: 14 },
  'lab.tds_min': { g: 'Production & lab', label: 'TDS minimum', t: 'dec', unit: 'ppm', min: 0 }, 'lab.tds_max': { g: 'Production & lab', label: 'TDS maximum', t: 'dec', unit: 'ppm', min: 0 },
  'jars.overdue_days': { g: 'Returnable jars', label: 'Jar overdue after', t: 'int', unit: 'days', min: 1, max: 365 },
  'jars.destroy_warn_pct': { g: 'Returnable jars', label: 'Destroy-rate warning', t: 'int', unit: '% of fills (30 days)', min: 0, max: 100 },
  'alerts.expiry_lead_days': { g: 'Alerts', label: 'Expiry / due-date lead time', t: 'int', unit: 'days', min: 0, max: 365, help: 'How early stock expiry, vehicle documents and maintenance are flagged.' },
};
const GROUP_ORDER = ['Plant', 'Features', 'Loading & dispatch', 'Inventory & reorder', 'Production & lab', 'Returnable jars', 'Alerts', 'Entry checks', 'Security', 'Other'];

export function mountSettings(host, { onSaved } = {}) {
  let list = []; let base = {}; let loaded = false; let stale = false;
  const body = el('div.stack');
  const save = el('button.btn.primary', { type: 'submit', form: 'settings-form', disabled: true }, el('span.btn-icon', {}, icon('save')), 'Save changes');
  const discard = el('button.btn.ghost', { type: 'button', disabled: true }, 'Discard');
  const bar = el('div.action-bar', {}, el('span.note', { id: 'dirtynote', text: 'No changes' }), discard, save);
  const form = el('form.stack', { id: 'settings-form', novalidate: true }, el('div.form-error', { role: 'alert' }), body);
  host.append(form, bar);
  const inputs = {};
  const act = api.action();

  function typeOf(k, v) { const m = META[k]; if (m) return m.t; return typeof v === 'boolean' ? 'bool' : typeof v === 'number' ? (Number.isInteger(v) ? 'int' : 'dec') : 'text'; }
  const cur = (k) => { const c = inputs[k]; const t = c.t; return t === 'bool' ? c.el.checked : t === 'text' ? c.el.value : c.el.value.trim(); };
  const parsed = (k) => {
    const c = inputs[k]; const v = cur(k);
    if (c.t === 'bool') return v; if (c.t === 'text') return v;
    if (v === '') return NaN; const n = Number(v.replace(',', '.')); return c.t === 'int' && !Number.isInteger(n) ? NaN : n;
  };
  const dirtyKeys = () => Object.keys(inputs).filter((k) => { const p = parsed(k); return !(p === base[k] || (typeof p === 'number' && Number(base[k]) === p)); });
  function updateDirty() {
    const n = dirtyKeys().length; save.disabled = n === 0; discard.disabled = n === 0;
    document.getElementById('dirtynote').textContent = n ? `${n} unsaved change${n === 1 ? '' : 's'}` : 'No changes';
    if (inputs['plant.utc_offset_minutes']) tzPreview();
  }
  let tzEl = null;
  function tzPreview() {
    const m = Number(inputs['plant.utc_offset_minutes'].el.value);
    if (!tzEl || !Number.isFinite(m)) return;
    const d = new Date(Date.now() + m * 60000); const p = (x) => String(x).padStart(2, '0');
    tzEl.textContent = `${ui.fmtOffset(m)} - plant time now ${p(d.getUTCHours())}:${p(d.getUTCMinutes())}`;
  }

  function render() {
    ui.clear(body);
    const groups = new Map(); GROUP_ORDER.forEach((g) => groups.set(g, []));
    const order = Object.keys(META);
    [...list].sort((a, b) => { const ia = order.indexOf(a.key), ib = order.indexOf(b.key); return (ia < 0 ? 999 : ia) - (ib < 0 ? 999 : ib) || a.key.localeCompare(b.key); }).forEach((s) => { const g = (META[s.key] && META[s.key].g) || 'Other'; groups.get(g).push(s); });
    for (const [g, items] of groups) {
      if (!items.length) continue;
      const card = el('section.card', {}, el('div.card-head', {}, el('h3', { text: g })), el('div.form-grid'));
      const grid = card.querySelector('.form-grid');
      items.forEach((s) => {
        const m = META[s.key] || {}; const t = typeOf(s.key, s.value); const id = 'set-' + s.key.replace(/\W/g, '-');
        let control; let input;
        if (t === 'bool') {
          input = el('input', { type: 'checkbox', id, name: s.key, checked: !!s.value });
          control = el('div.field.full', { dataset: { name: s.key } }, el('label.switch', { for: id }, input, el('span.track'), el('span', { text: m.label || s.key })), el('p.help', { text: m.help || s.description || '' }), el('p.error'));
        } else {
          input = el('input.input', { id, name: s.key, type: 'text', inputmode: t === 'int' ? 'numeric' : t === 'dec' ? 'decimal' : 'text', maxlength: m.max && t === 'text' ? m.max : undefined, autocomplete: 'off', spellcheck: 'false' });
          input.value = s.value ?? '';
          const grp = m.unit ? el('div.input-group.has-unit', {}, input, el('span.unit', { text: m.unit })) : input;
          control = el('div.field', { dataset: { name: s.key } }, el('label', { for: id, text: m.label || s.key }), grp, el('p.help', { text: m.help || s.description || '' }), el('p.error'));
          if (m.tz) { tzEl = el('p.help.strong', { 'aria-live': 'polite' }); control.insertBefore(tzEl, control.querySelector('.error')); }
        }
        inputs[s.key] = { el: input, t };
        input.addEventListener('input', updateDirty); input.addEventListener('change', updateDirty);
        grid.append(control);
      });
      body.append(card);
    }
    updateDirty();
  }
  discard.addEventListener('click', () => { ui.clearFormError(form); render(); });

  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    const keys = dirtyKeys(); if (!keys.length) return;
    const errors = {}; const payload = {};
    keys.forEach((k) => {
      const v = parsed(k); const m = META[k] || {};
      if (typeof v === 'number' && Number.isNaN(v)) { errors[k] = inputs[k].t === 'int' ? 'Enter a whole number' : 'Enter a number'; return; }
      if (typeof v === 'number' && ((m.min !== undefined && v < m.min) || (m.max !== undefined && v > m.max))) { errors[k] = `Must be between ${m.min ?? '-'} and ${m.max ?? '-'}`; return; }
      payload[k] = v;
    });
    if (Object.keys(errors).length) { ui.formError(form, { status: 422, fields: errors, message: '' }); return; }
    await api.busy(save, async () => {
      try {
        const res = await act.put('/api/admin/settings', payload);
        list = res; base = Object.fromEntries(res.map((s) => [s.key, s.value])); stale = false;
        toast.ok('Settings saved'); render(); if (onSaved) onSaved(res);
      } catch (e) { if (e.network) save.lastChild.textContent = 'Not sent - Retry'; ui.formError(form, e); }
    });
  });

  async function refresh() {
    if (loaded && dirtyKeys().length) {   // never overwrite what the admin is typing
      if (!stale) { stale = true; form.prepend(el('div.banner.warn.inline', { id: 'stale-set' }, icon('info'), el('div.grow', { text: 'These settings were changed elsewhere. Save or discard your edits to see the latest values.' }), el('button.btn.secondary.sm', { type: 'button', on: { click: () => { stale = false; load(true); } } }, 'Load latest'))); }
      return;
    }
    await load();
  }
  async function load(force) {
    if (!loaded) ui.mount(body, ui.skeletonCards(3));
    try {
      list = await api.get('/api/admin/settings'); base = Object.fromEntries(list.map((s) => [s.key, s.value])); loaded = true;
      const s = document.getElementById('stale-set'); if (s) s.remove();
      render();
    } catch (e) { if (!e.redirecting) ui.mount(body, ui.errorState(e, () => load())); }
  }
  load();
  return { refresh };
}
