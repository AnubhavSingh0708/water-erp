// ui.js - DOM builder, escaping, toasts, dialogs, form errors, badges, formatters, tables, quantity inputs.
// RULE: dynamic strings only go through el()/textContent/esc(). Never innerHTML with data.

/* ---------------------------------------------------------------- escaping + DOM builder */
const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
export const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ESC[c]);

const SVG_NS = 'http://www.w3.org/2000/svg';
/** <svg class="i"><use href="/img/icons.svg#name"/></svg> */
export function icon(name, cls = '') {
  const s = document.createElementNS(SVG_NS, 'svg');
  s.setAttribute('class', ('i ' + cls).trim()); s.setAttribute('aria-hidden', 'true'); s.setAttribute('focusable', 'false');
  const u = document.createElementNS(SVG_NS, 'use'); u.setAttribute('href', `/img/icons.svg#${name}`);
  s.appendChild(u); return s;
}

/**
 * el('div.card#id', {class:'x', text:'..', dataset:{a:1}, on:{click:fn}, aria-label:'..', onclick:fn}, ...children)
 * children: Node | string | number | array | null/false (skipped). Strings become text nodes (never HTML).
 */
export function el(tag, props, ...children) {
  let t = tag, id = '', cls = [];
  const m = /^([a-z0-9]+)?((?:[.#][\w-]+)*)$/i.exec(tag);
  if (m) { t = m[1] || 'div'; (m[2].match(/[.#][\w-]+/g) || []).forEach((x) => (x[0] === '.' ? cls.push(x.slice(1)) : (id = x.slice(1)))); }
  const node = document.createElement(t);
  if (id) node.id = id;
  if (cls.length) node.className = cls.join(' ');
  if (props && (props instanceof Node || typeof props === 'string' || typeof props === 'number' || Array.isArray(props))) { children.unshift(props); props = null; }
  for (const [k, v] of Object.entries(props || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k === 'class' || k === 'className') node.className = (node.className ? node.className + ' ' : '') + v;
    else if (k === 'text') node.textContent = v;
    else if (k === 'dataset') Object.assign(node.dataset, v);
    else if (k === 'style' && typeof v === 'object') Object.assign(node.style, v);
    else if (k === 'on') for (const [ev, fn] of Object.entries(v)) node.addEventListener(ev, fn);
    else if (/^on[A-Za-z]+$/.test(k) && typeof v === 'function') node.addEventListener(k.slice(2).toLowerCase(), v);
    else if (v === true) node.setAttribute(k, '');
    else if (k === 'value' || k === 'checked' || k === 'selected' || k === 'disabled' || k === 'indeterminate') node[k] = v;
    else node.setAttribute(k, String(v));
  }
  append(node, children);
  return node;
}
export function append(node, children) {
  for (const c of children.flat(Infinity)) {
    if (c === null || c === undefined || c === false) continue;
    node.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return node;
}
export function clear(node) { while (node.firstChild) node.removeChild(node.firstChild); return node; }
export function mount(node, ...children) { clear(node); return append(node, children); }
export const $ = (sel, root = document) => root.querySelector(sel);
export const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
export const debounce = (fn, ms = 300) => { let t; const d = (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; d.cancel = () => clearTimeout(t); return d; };
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/* ---------------------------------------------------------------- config + user (set by shell.js) */
export const config = { utcOffsetMinutes: null, plantName: 'Neersagar', user: null, qrLabels: false, settings: {} };
export function setConfig(c) { Object.assign(config, c); }

/* ---------------------------------------------------------------- formatters */
const nf = new Map();
function numFmt(dp) {
  if (!nf.has(dp)) nf.set(dp, new Intl.NumberFormat(undefined, { minimumFractionDigits: 0, maximumFractionDigits: dp }));
  return nf.get(dp);
}
export function toNum(v) { if (v === null || v === undefined || v === '') return NaN; return typeof v === 'number' ? v : Number(String(v).trim().replace(/,/g, '')); }
/** Decimal string/number -> "1,234.5" (trailing zeros trimmed, max dp). */
export function fmtNum(v, dp = 2) { const n = toNum(v); return Number.isFinite(n) ? numFmt(dp).format(n) : '-'; }
export function fmtQty(v, unit = '', dp = 2) { const s = fmtNum(v, dp); return s === '-' ? s : unit ? `${s} ${unit}` : s; }
/** Pallets + loose crates: fmtCrates(130, 60) -> "2 pallets + 10 crates" ; no cpp -> "130 crates". */
export function fmtCrates(n, cratesPerPallet) {
  const c = Math.round(toNum(n));
  if (!Number.isFinite(c)) return '-';
  const crates = (x) => `${fmtNum(x, 0)} ${x === 1 ? 'crate' : 'crates'}`;
  const cpp = toNum(cratesPerPallet);
  if (!cpp || cpp < 1 || c < cpp) return crates(c);
  const p = Math.floor(c / cpp), l = c % cpp;
  return `${fmtNum(p, 0)} ${p === 1 ? 'pallet' : 'pallets'}${l ? ` + ${crates(l)}` : ''}`;
}
/** Plant-local calendar parts for an instant (offset from /api/ref/settings; falls back to the browser zone). */
function parts(d) {
  if (config.utcOffsetMinutes === null || config.utcOffsetMinutes === undefined) {
    return { y: d.getFullYear(), mo: d.getMonth() + 1, d: d.getDate(), h: d.getHours(), mi: d.getMinutes(), wd: d.getDay() };
  }
  const t = new Date(d.getTime() + config.utcOffsetMinutes * 60000);
  return { y: t.getUTCFullYear(), mo: t.getUTCMonth() + 1, d: t.getUTCDate(), h: t.getUTCHours(), mi: t.getUTCMinutes(), wd: t.getUTCDay() };
}
const MON = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const p2 = (n) => String(n).padStart(2, '0');
/** 'YYYY-MM-DD' (plain date) or ISO timestamp -> "13 Nov 2026". */
export function fmtDate(v) {
  if (!v) return '-';
  if (/^\d{4}-\d{2}-\d{2}$/.test(v)) { const [y, m, d] = v.split('-').map(Number); return `${d} ${MON[m - 1]} ${y}`; }
  const d = new Date(v); if (isNaN(d)) return '-';
  const p = parts(d); return `${p.d} ${MON[p.mo - 1]} ${p.y}`;
}
export function fmtTime(v) { const d = new Date(v); if (isNaN(d)) return '-'; const p = parts(d); return `${p2(p.h)}:${p2(p.mi)}`; }
export function fmtDateTime(v) { if (!v) return '-'; const d = new Date(v); if (isNaN(d)) return '-'; return `${fmtDate(v)}, ${fmtTime(v)}`; }
/** Today as YYYY-MM-DD in plant time. */
export function todayPlant() { const p = parts(new Date()); return `${p.y}-${p2(p.mo)}-${p2(p.d)}`; }
export function addDays(ymd, n) { const [y, m, d] = ymd.split('-').map(Number); const t = new Date(Date.UTC(y, m - 1, d + n)); return `${t.getUTCFullYear()}-${p2(t.getUTCMonth() + 1)}-${p2(t.getUTCDate())}`; }
export function fmtRelative(v) {
  const t = new Date(v).getTime(); if (isNaN(t)) return '-';
  const s = Math.round((Date.now() - t) / 1000);
  if (s < 45) return 'just now';
  const m = Math.round(s / 60); if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60); if (h < 24) return `${h} h ago`;
  const d = Math.round(h / 24); if (d < 8) return `${d} d ago`;
  return fmtDate(v);
}
export function fmtMinutes(m) { m = Math.round(toNum(m)); if (!Number.isFinite(m)) return '-'; if (m < 60) return `${m} min`; const h = Math.floor(m / 60); return h < 24 ? `${h} h ${m % 60} min` : `${Math.floor(h / 24)} d ${h % 24} h`; }
export function fmtOffset(mins) { const n = toNum(mins); if (!Number.isFinite(n)) return '-'; const s = n < 0 ? '-' : '+'; const a = Math.abs(n); return `UTC${s}${p2(Math.floor(a / 60))}:${p2(a % 60)}`; }
export const initials = (name) => String(name || '?').split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0].toUpperCase()).join('') || '?';

/* ---------------------------------------------------------------- badges */
const BADGES = {
  // green
  APPROVED: ['ok', 'check-circle'], FG_READY: ['ok', 'check-circle'], OK: ['ok', 'check-circle'], RECEIVED: ['ok', 'check-circle'], SETTLED: ['ok', 'check-circle'], AVAILABLE: ['ok', 'check-circle'],
  PASS: ['ok', 'check-circle'], ACTIVE: ['ok', 'check-circle'], DONE: ['ok', 'check-circle'], DELIVERED: ['ok', 'check-circle'], LIVE: ['ok', 'check-circle'], ON: ['ok', 'check-circle'],
  // amber
  QUARANTINE: ['warn', 'clock'], LOW: ['warn', 'alert'], DUE_SOON: ['warn', 'clock'], PARTIAL: ['warn', 'clock'], IN_TRANSIT: ['warn', 'transfer'], LOADING: ['warn', 'truck'], OPEN: ['warn', 'clock'],
  DRAFT: ['warn', 'edit'], RESERVED: ['warn', 'lock'], DISPATCHED: ['info', 'truck'], WARNING: ['warn', 'alert'], PENDING: ['warn', 'clock'], HIGH_DESTROY_RATE: ['warn', 'alert'], NEAR_RETIREMENT: ['warn', 'alert'],
  // red
  REJECTED: ['bad', 'x-circle'], CRITICAL: ['bad', 'alert'], OVERDUE: ['bad', 'alert'], OUT: ['bad', 'ban'], FAIL: ['bad', 'x-circle'], VOID: ['bad', 'ban'], CANCELLED: ['bad', 'ban'],
  EXPIRED: ['bad', 'ban'], LOCKED: ['bad', 'lock'], ERROR: ['bad', 'alert'], INACTIVE: ['neutral', 'ban'], OFF: ['neutral', 'ban'],
  INFO: ['info', 'info'],
};
/** badge('QUARANTINE') -> <span class="badge warn"><icon> QUARANTINE</span> (text + icon + colour, never colour alone). */
export function badge(status, label, tone) {
  const key = String(status ?? '').toUpperCase();
  const [t, ic] = BADGES[key] || ['neutral', 'info'];
  const text = label ?? (key ? key.replace(/_/g, ' ') : '-');
  return el('span', { class: `badge ${tone || t}` }, icon(ic), text);
}
export const SEVERITY_TONE = { CRITICAL: 'bad', HIGH: 'bad', WARNING: 'warn', MEDIUM: 'warn', INFO: 'info', LOW: 'info' };

/* ---------------------------------------------------------------- toasts */
function toastHost() {
  let h = document.getElementById('toasts');
  if (!h) { h = el('div#toasts.toasts', { 'aria-live': 'polite', 'aria-atomic': 'false' }); document.body.append(h); }
  return h;
}
/** toast('Saved', {type:'ok'|'err'|'warn'|'info', ms, sub, action:{label, onClick}}) */
export function toast(msg, o = {}) {
  const type = o.type || 'info';
  const ic = { ok: 'check-circle', err: 'alert', warn: 'alert', info: 'info' }[type] || 'info';
  const node = el('div.toast', { class: type, role: type === 'err' ? 'alert' : 'status' },
    icon(ic), el('div.t-body', {}, msg, o.sub ? el('small', { text: o.sub }) : null),
    o.action ? el('button', { type: 'button', on: { click: () => { o.action.onClick(); gone(); } } }, o.action.label) : null,
    el('button', { type: 'button', 'aria-label': 'Dismiss', on: { click: () => gone() } }, icon('x', 'sm')));
  const gone = () => { node.classList.add('leaving'); setTimeout(() => node.remove(), 200); };
  toastHost().append(node);
  const ms = o.ms ?? (type === 'err' ? 8000 : 3800);
  if (ms) setTimeout(gone, ms);
  return { close: gone };
}
toast.ok = (m, o) => toast(m, { ...o, type: 'ok' });
toast.err = (m, o) => toast(m, { ...o, type: 'err' });
toast.warn = (m, o) => toast(m, { ...o, type: 'warn' });
/** Toast an API error object (message + hint + Ref on 5xx). Ignores redirects/cancellations. */
export function toastError(err, fallback = 'Something went wrong') {
  if (!err || err.redirecting || err.cancelled || err.aborted) return;
  const sub = [err.hint, errRef(err)].filter(Boolean).join('  ');
  toast.err(err.message || fallback, { sub });
}
export const errRef = (err) => (err && err.requestId && (err.status >= 500 || err.network === false && !err.status) ? `Ref: ${err.requestId}` : '');

/* ---------------------------------------------------------------- dialogs */
let openCount = 0;
/**
 * openDialog({title, body, actions:[Node], sheet:true, wide:false, onClose})
 * Returns {el, body, foot, close(result), closed:Promise<result>}. Native <dialog>: focus trap + Esc included.
 */
export function openDialog({ title, body, actions, sheet = true, wide = false, dismissible = true, onClose, labelledby } = {}) {
  const dlg = el('dialog.dlg', { class: `${sheet ? 'sheet' : ''} ${wide ? 'wide' : ''}`.trim(), 'aria-labelledby': labelledby || 'dlg-t-' + (++openCount) });
  const titleEl = el('h2', { id: dlg.getAttribute('aria-labelledby') }, title || '');
  const head = el('div.dlg-head', {}, titleEl, dismissible ? el('button.btn.ghost.icon.sm', { type: 'button', 'aria-label': 'Close', on: { click: () => close(undefined) } }, icon('x')) : null);
  const bodyEl = el('div.dlg-body', {}, body);
  const foot = actions && actions.length ? el('div.dlg-foot', {}, actions) : null;
  dlg.append(head, bodyEl); if (foot) dlg.append(foot);
  document.body.append(dlg);
  let resolve; const closed = new Promise((r) => (resolve = r));
  let result;
  const close = (r) => { result = r; if (dlg.open) dlg.close(); else finish(); };
  const finish = () => { dlg.remove(); if (onClose) onClose(result); resolve(result); };
  dlg.addEventListener('close', finish, { once: true });
  dlg.addEventListener('cancel', (e) => { if (!dismissible) e.preventDefault(); });
  dlg.addEventListener('click', (e) => { if (dismissible && e.target === dlg) close(undefined); });   // backdrop click
  dlg.showModal();
  // focus the first meaningful control, not the close button
  const first = dlg.querySelector('[autofocus], .dlg-body input:not([type=hidden]), .dlg-body select, .dlg-body textarea');
  if (first) first.focus({ preventScroll: true });
  return { el: dlg, body: bodyEl, foot, titleEl, close, closed };
}

/** confirmDialog({title, message, warnings:[..], confirmText, cancelText, danger}) -> Promise<boolean>. */
export function confirmDialog({ title = 'Are you sure?', message = '', warnings = [], confirmText = 'Confirm', cancelText = 'Cancel', danger = false, detail } = {}) {
  const body = el('div.stack.tight', {},
    message ? el('p', { text: message }) : null,
    warnings && warnings.length ? el('div.banner.warn.inline', {}, icon('alert'), el('ul.warn-list', {}, warnings.map((w) => el('li', { text: String(w) })))) : null,
    detail || null);
  const yes = el('button.btn', { type: 'button', class: danger ? 'danger' : 'primary', on: { click: () => dlg.close(true) } }, confirmText);
  const no = el('button.btn.secondary', { type: 'button', on: { click: () => dlg.close(false) } }, cancelText);
  const dlg = openDialog({ title, body, actions: [no, yes] });
  no.focus();
  return dlg.closed.then((r) => r === true);
}

/** Dialog with a text input to type a reason (min length). Resolves string or null. */
export function promptDialog({ title, label, help, confirmText = 'Save', min = 0, max = 500, multiline = true, danger = false, value = '' } = {}) {
  const input = multiline ? el('textarea.textarea', { name: 'reason', maxlength: max, rows: 3, value }) : el('input.input', { name: 'reason', maxlength: max, value });
  const err = el('p.error', { role: 'alert' });
  const f = el('div.field', {}, el('label', { text: label || 'Reason' }), input, help ? el('p.help', { text: help }) : null, err);
  const yes = el('button.btn', { type: 'button', class: danger ? 'danger' : 'primary', on: { click: () => {
    const v = input.value.trim();
    if (v.length < min) { f.classList.add('has-error'); err.textContent = `Please enter at least ${min} characters.`; input.focus(); return; }
    dlg.close(v);
  } } }, confirmText);
  const no = el('button.btn.secondary', { type: 'button', on: { click: () => dlg.close(null) } }, 'Cancel');
  const dlg = openDialog({ title, body: f, actions: [no, yes] });
  return dlg.closed.then((r) => (typeof r === 'string' ? r : null));
}

/** "Product · BATCH-NO": keep the batch number in one piece (never broken at its hyphens). */
function itemTitle(text) {
  const [name, ...rest] = String(text).split(' \u00b7 ');
  return el('b', {}, name, rest.length ? [' \u00b7 ', el('span.nowrap', { text: rest.join(' \u00b7 ') })] : null);
}
/** unitLabel('containers', 1) -> 'container'; unitLabel({one:'box', other:'boxes'}, 2) -> 'boxes'. No unit -> ''. */
export function unitLabel(unit, n) {
  if (!unit) return '';
  const one = Math.abs(toNum(n)) === 1;
  if (typeof unit === 'object') return (one ? unit.one : (unit.other || unit.many)) || unit.one || '';
  return one ? (SINGULAR[String(unit).toLowerCase()] || unit) : unit;   // only well-known plurals are singularised ('nos', 'pcs', 'kg' stay as given)
}
const SINGULAR = { crates: 'crate', containers: 'container', pallets: 'pallet', jars: 'jar', bottles: 'bottle', bags: 'bag', rolls: 'roll', sets: 'set', labels: 'label', sheets: 'sheet', boxes: 'box', cartons: 'carton', drums: 'drum', cans: 'can', cases: 'case', sacks: 'sack', litres: 'litre', liters: 'liter' };

/**
 * reviewSheet({title, effects:[{item,zone,delta,after,unit,note,right}], warnings:[], confirmText, cancelText, summary, intro,
 *              before:Node, after:Node, unit:'crates'|{one,other}, decimals, emptyText, danger}) -> Promise<boolean>
 * Shows what the dry run says will happen BEFORE the real submit. `before` / `after` are extra nodes above / below the
 * stock effects (item lists, vehicle papers, notes); `unit` is the default unit (singular when |delta| = 1); `decimals` default 4.
 */
export function reviewSheet({ title = 'Review before saving', effects = [], warnings = [], confirmText = 'Confirm and save', cancelText = 'Go back and edit', summary, intro, before, after, unit, decimals = 4, emptyText = 'No stock changes.', danger = false } = {}) {
  const list = effects.length ? el('div.effects', {}, effects.map((e) => {
    const d = toNum(e.delta);
    const u = unitLabel(e.unit || unit, d);
    const moves = Number.isFinite(d) && d !== 0;
    const sub = [e.zone, moves && e.after !== undefined && e.after !== null ? `will be ${fmtNum(e.after, decimals)}` : '', e.note].filter(Boolean).join(' - ');
    return el('div.effect', {}, el('div.grow', {}, itemTitle(e.item_name || e.item || ''), sub ? el('small', { text: sub }) : null),
      moves ? el('span.delta', { class: d < 0 ? 'neg' : 'pos' }, `${d > 0 ? '+' : '-'}${fmtNum(Math.abs(d), decimals)}${u ? ' ' + u : ''}`) : (e.right ? el('span.delta', { text: String(e.right) }) : null));
  })) : el('p.muted', { text: emptyText });
  const body = el('div.stack', {}, summary || intro ? el('p', { text: summary || intro }) : null, before || null, list,
    warnings.length ? el('div.banner.warn.inline', {}, icon('alert'), el('ul.warn-list', {}, warnings.map((w) => el('li', { text: String(w) })))) : null, after || null);
  const yes = el('button.btn', { type: 'button', class: danger ? 'danger' : 'primary', on: { click: () => dlg.close(true) } }, confirmText);
  const no = el('button.btn.secondary', { type: 'button', on: { click: () => dlg.close(false) } }, cancelText);
  const dlg = openDialog({ title, body, actions: [no, yes] });
  (danger ? no : yes).focus();
  return dlg.closed.then((r) => r === true);
}

/* ---------------------------------------------------------------- forms: fields + errors */
/**
 * field({label, name, type, value, help, required, optional, inputmode, placeholder, options:[{value,label}], unit, min,max,step, maxlength, autocomplete, rows})
 * -> {el, input, set(v), get()} . select options are text only.
 */
export function field(o) {
  const id = o.id || `f-${o.name}-${Math.random().toString(36).slice(2, 7)}`;
  let input;
  const common = { id, name: o.name, required: o.required ? true : undefined, disabled: o.disabled, 'aria-describedby': `${id}-help ${id}-err`, autocomplete: o.autocomplete || (o.type === 'password' ? 'off' : undefined) };
  if (o.type === 'select') {
    input = el('select.select', common, (o.options || []).map((x) => el('option', { value: x.value, selected: String(x.value) === String(o.value ?? ''), disabled: x.disabled }, x.label)));
  } else if (o.type === 'textarea') {
    input = el('textarea.textarea', { ...common, rows: o.rows || 3, maxlength: o.maxlength, placeholder: o.placeholder }); input.value = o.value ?? '';
  } else {
    const numeric = o.type === 'number' || o.type === 'int' || o.type === 'decimal';
    input = el('input.input', {
      ...common, type: numeric ? 'text' : (o.type || 'text'), inputmode: o.inputmode || (o.type === 'int' ? 'numeric' : numeric ? 'decimal' : o.type === 'tel' ? 'tel' : undefined),
      placeholder: o.placeholder, maxlength: o.maxlength, min: o.min, max: o.max, step: o.step, pattern: o.pattern, autocapitalize: o.autocapitalize || (numeric ? undefined : undefined),
      spellcheck: 'false', 'data-kind': numeric ? o.type : undefined,
    });
    input.value = o.value ?? '';
  }
  const err = el('p.error', { id: `${id}-err`, role: 'alert' });
  let control = input;
  if (o.unit && input.tagName === 'INPUT') control = el('div.input-group.has-unit', {}, input, el('span.unit', { text: o.unit }));
  const root = el('div.field', { dataset: { name: o.name } },
    o.label ? el('label', { for: id }, o.label, o.required ? el('span.req', { 'aria-hidden': 'true', text: '*' }) : (o.optional ? el('span.opt', { text: '(optional)' }) : null)) : null,
    control, o.help ? el('p.help', { id: `${id}-help`, text: o.help }) : el('span', { id: `${id}-help` }), err);
  return {
    el: root, input, error: err,
    get: () => (input.type === 'checkbox' ? input.checked : input.value),
    set: (v) => { if (input.type === 'checkbox') input.checked = !!v; else input.value = v ?? ''; },
    setError: (m) => setFieldError(root, m),
  };
}
export function setFieldError(root, msg) {
  const f = root.closest ? (root.classList.contains('field') ? root : root.closest('.field')) : null;
  if (!f) return;
  let e = f.querySelector(':scope > .error');
  if (!e) { e = el('p.error', { role: 'alert' }); f.append(e); }
  f.classList.toggle('has-error', !!msg);
  e.textContent = msg || '';
  const inp = f.querySelector('input,select,textarea');
  if (inp) { if (msg) inp.setAttribute('aria-invalid', 'true'); else inp.removeAttribute('aria-invalid'); }
}
export function clearFormError(form) {
  $$('.field.has-error', form).forEach((f) => setFieldError(f, ''));
  $$('[aria-invalid]', form).forEach((i) => i.removeAttribute('aria-invalid'));
  const box = form.querySelector('.form-error'); if (box) clear(box);
}
/**
 * formError(form, err) - inline field errors (422 `fields`, incl. dotted/indexed keys such as `lab.ph_min`, `lines[0].qty`),
 * focuses the first invalid control, shows message + hint (+ "Ref: <requestId>" on 5xx) in the .form-error box.
 * Returns true when the error was shown.
 */
export function formError(form, err) {
  if (!err || err.redirecting || err.cancelled || err.aborted) return false;
  clearFormError(form);
  let box = form.querySelector('.form-error');
  if (!box) { box = el('div.form-error', { role: 'alert', 'aria-live': 'assertive' }); form.prepend(box); }
  const unmatched = []; let firstEl = null;
  const fields = err.fields || {};
  for (const [name, msg] of Object.entries(fields)) {
    let inp = null;
    try { inp = form.querySelector(`[name="${CSS.escape(name)}"]`); } catch { /* bad selector */ }
    if (!inp) { const base = name.replace(/\[\d+\]/g, '').split('.').pop(); try { inp = form.querySelector(`[name="${CSS.escape(base)}"]`); } catch { /* ignore */ } }
    if (inp) { setFieldError(inp, String(msg)); firstEl = firstEl || inp; } else unmatched.push(`${name}: ${msg}`);
  }
  const ref = errRef(err);
  const hasFields = Object.keys(fields).length > 0;
  const lines = [];
  if (!hasFields || unmatched.length || err.status !== 422) lines.push(err.message);
  if (unmatched.length) lines.push(...unmatched);
  const tone = err.network ? 'warn' : 'err';
  if (lines.length || err.hint || ref || hasFields) {
    box.append(el('div.banner.inline', { class: tone }, icon(err.network ? 'wifi-off' : 'alert'),
      el('div.grow', {}, el('b', { text: lines.length ? lines.join(' ') : 'Please fix the highlighted fields.' }),
        err.hint ? el('div', { class: 'small', style: { fontWeight: 500 }, text: err.hint }) : null,
        ref ? el('span.ref', { text: ref }) : null)));
  }
  const target = firstEl || null;
  if (target) { target.focus({ preventScroll: false }); target.scrollIntoView({ block: 'center', behavior: 'smooth' }); }
  else box.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  return true;
}
export function formBanner(form, msg, tone = 'ok') {
  let box = form.querySelector('.form-error');
  if (!box) { box = el('div.form-error'); form.prepend(box); }
  clear(box); box.append(el('div.banner.inline', { class: tone }, icon(tone === 'ok' ? 'check-circle' : 'info'), el('div.grow', { text: msg })));
}

/** Read named controls of a form into a plain object (strings; checkboxes -> boolean; multi-check groups -> arrays). */
export function readForm(form) {
  const out = {};
  for (const c of form.elements) {
    if (!c.name || c.disabled) continue;
    if (c.type === 'checkbox') {
      if (c.dataset.group) { (out[c.dataset.group] ||= []); if (c.checked) out[c.dataset.group].push(c.value); } else out[c.name] = c.checked;
    } else if (c.type === 'radio') { if (c.checked) out[c.name] = c.value; }
    else out[c.name] = c.value;
  }
  return out;
}

/* ---------------------------------------------------------------- empty / skeleton / error states */
export function empty({ icon: ic = 'package', title = 'Nothing here yet', text = '', action, tone } = {}) {
  return el('div.empty', { class: tone || '' }, el('div.ico', {}, icon(ic)), el('h3', { text: title }), text ? el('p', { text }) : null,
    action ? el('button.btn.secondary', { type: 'button', on: { click: action.onClick } }, action.icon ? icon(action.icon) : null, action.label) : null);
}
export function errorState(err, retry) {
  const e = err || {};
  return empty({
    icon: e.network ? 'wifi-off' : 'alert', tone: 'error', title: e.network ? 'Cannot reach the server' : (e.status === 403 ? 'Not allowed' : 'Could not load this'),
    text: [e.message, e.hint, errRef(e)].filter(Boolean).join(' '), action: retry ? { label: 'Try again', icon: 'refresh', onClick: retry } : null,
  });
}
export function skeleton(n = 3, kind = 'line') { return el('div', { 'aria-hidden': 'true' }, Array.from({ length: n }, () => el('span.skeleton', { class: kind }))); }
export function skeletonCards(n = 3) { return el('div.stack', { 'aria-busy': 'true' }, Array.from({ length: n }, () => el('div.card', {}, el('span.skeleton.title'), el('span.skeleton.line'), el('span.skeleton.line', { style: { width: '70%' } })))); }
export function spinner(label = 'Loading...') { return el('div.loading-screen', { role: 'status' }, icon('spinner', 'spin lg'), label); }

/* ---------------------------------------------------------------- table -> cards */
/**
 * cardsTable({columns, rows, rowClass(row), onRow(row), empty:{...}})
 * column: {key, label, cls:'k'|'s'|'a'|'num'|'w'|'', render(row)->Node|string, value(row), hideMobile:true}
 * 'k' = key title, 's' = status badge (top right on mobile), 'a' = actions (full width), 'w' = full width cell.
 * Desktop renders a normal table; <768px each row becomes a card (CSS only).
 */
export function cardsTable({ columns, rows, rowClass, onRow, emptyState, caption }) {
  if (!rows || !rows.length) return emptyState ? empty(emptyState) : empty({ title: 'No records' });
  const thead = el('thead', {}, el('tr', {}, columns.map((c) => el('th', { class: c.cls === 'num' ? 'num' : '', scope: 'col' }, c.label ?? ''))));
  const tbody = el('tbody', {}, rows.map((r) => {
    const tr = el('tr', { class: [rowClass ? rowClass(r) : '', onRow ? 'clickable' : ''].filter(Boolean).join(' ') },
      columns.map((c) => {
        const v = c.render ? c.render(r) : (c.value ? c.value(r) : r[c.key]);
        return el('td', { class: [c.cls, c.hideMobile ? 'hide-m' : ''].filter(Boolean).join(' '), 'data-label': c.label || '' }, v === null || v === undefined || v === '' ? (c.cls === 'a' ? null : '-') : v);
      }));
    if (onRow) tr.addEventListener('click', (e) => { if (e.target.closest('button,a,input,select,label')) return; onRow(r); });
    return tr;
  }));
  return el('div.table-wrap', {}, el('table.tbl.table-cards', {}, caption ? el('caption.sr-only', { text: caption }) : null, thead, tbody));
}
/** Pager: pager({total, limit, offset, onPage(offset)}) */
export function pager({ total, limit, offset, onPage }) {
  const from = total ? offset + 1 : 0, to = Math.min(total, offset + limit);
  return el('div.pager', {}, el('span.info', { text: total ? `${from}-${to} of ${fmtNum(total, 0)}` : 'No results' }),
    el('div.btn-row', {},
      el('button.btn.secondary.sm', { type: 'button', disabled: offset <= 0, on: { click: () => onPage(Math.max(0, offset - limit)) } }, icon('chevron-left', 'sm'), 'Previous'),
      el('button.btn.secondary.sm', { type: 'button', disabled: offset + limit >= total, on: { click: () => onPage(offset + limit) } }, 'Next', icon('chevron-right', 'sm'))));
}

/* ---------------------------------------------------------------- segmented / tabs */
/** segmented({options:[{value,label}], value, onChange}) -> {el, set(v), value} */
export function segmented({ options, value, onChange, label }) {
  let cur = value;
  const root = el('div.seg', { role: 'group', 'aria-label': label || 'Options' });
  const btns = options.map((o) => el('button', { type: 'button', 'aria-pressed': String(o.value === cur), on: { click: () => set(o.value, true) } }, o.label));
  btns.forEach((b) => root.append(b));
  function set(v, fire) { cur = v; btns.forEach((b, i) => b.setAttribute('aria-pressed', String(options[i].value === v))); if (fire && onChange) onChange(v); }
  return { el: root, set: (v) => set(v, false), get value() { return cur; } };
}
/** tabs({tabs:[{id,label,icon?}], active, onChange}) -> {el, select(id)} (roving arrow keys). */
export function tabs({ tabs: list, active, onChange, label }) {
  const root = el('div.tabs', { role: 'tablist', 'aria-label': label || 'Sections' });
  const btns = list.map((t) => el('button', { type: 'button', role: 'tab', id: `tab-${t.id}`, 'aria-selected': 'false', tabindex: '-1', dataset: { tab: t.id }, on: { click: () => select(t.id, true) } }, t.icon ? icon(t.icon, 'sm') : null, t.label));
  btns.forEach((b) => root.append(b));
  function select(id, fire) {
    btns.forEach((b) => { const on = b.dataset.tab === id; b.setAttribute('aria-selected', String(on)); b.tabIndex = on ? 0 : -1; if (on) b.scrollIntoView({ block: 'nearest', inline: 'center' }); });
    if (fire && onChange) onChange(id);
  }
  root.addEventListener('keydown', (e) => {
    const i = btns.findIndex((b) => b.getAttribute('aria-selected') === 'true');
    if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') { const n = btns[(i + (e.key === 'ArrowRight' ? 1 : btns.length - 1)) % btns.length]; n.focus(); select(n.dataset.tab, true); e.preventDefault(); }
  });
  select(active || list[0].id, false);
  return { el: root, select: (id) => select(id, false) };
}
export function collapsible(title, content, { open = false, icon: ic } = {}) {
  return el('details.collapse', { open }, el('summary', {}, el('span.row', { style: { gap: '8px' } }, ic ? icon(ic) : null, title), icon('chevron-down')), el('div.body', {}, content));
}
export function progress(pct, tone) {
  const v = Math.max(0, Math.min(100, Number(pct) || 0));
  const t = tone || (v >= 100 ? 'bad' : v >= 90 ? 'warn' : '');
  const bar = el('i'); bar.style.width = v + '%';
  return el('div.progress', { class: t, role: 'progressbar', 'aria-valuenow': Math.round(v), 'aria-valuemin': 0, 'aria-valuemax': 100 }, bar);
}

/* ---------------------------------------------------------------- quantity input with live echo */
/**
 * qtyInput({name, label, unit, min=0, max, decimals=0, value, cratesPerPallet, onInput, help, required})
 * -> {el, input, get value(): number|null, get raw(), set(v), valid, max, setMax(n|null), setUnit(u)}
 * `unit` may be 'containers' (singular derived: "1 container") or {one:'box', other:'boxes'}.
 * For crates (decimals 0 + cratesPerPallet): live "= 2 pallets + 10 crates" echo plus a "Pallets + crates" entry mode.
 * Always shows "Check: you entered N" so a typo like 1200 vs 120 is visible; clamps to [min,max] on blur and on the +/- buttons.
 * setMax() updates the limit later (e.g. when the free stock changes); the echo turns red when the value is over it.
 */
export function qtyInput(o) {
  const dp = o.decimals ?? 0; const cpp = toNum(o.cratesPerPallet) || 0;
  let max = o.max === undefined || o.max === null || !Number.isFinite(Number(o.max)) ? null : Number(o.max);
  let unitSpec = o.unit;
  const id = `q-${o.name}-${Math.random().toString(36).slice(2, 6)}`;
  const input = el('input.input', { id, name: o.name, type: 'text', inputmode: dp ? 'decimal' : 'numeric', autocomplete: 'off', placeholder: o.placeholder ?? '0', 'aria-describedby': `${id}-echo ${id}-err`, required: o.required ? true : undefined });
  input.value = o.value ?? '';
  const parse = () => { const n = toNum(input.value); return Number.isFinite(n) ? n : null; };
  const echo = el('div.qty-echo', { id: `${id}-echo`, 'aria-live': 'polite' });
  const err = el('p.error', { id: `${id}-err`, role: 'alert' });
  const lo = () => o.min ?? 0;
  const clamp = () => {
    let n = parse(); if (n === null) return;
    if (n < lo()) n = lo(); if (max !== null && n > max) n = max;
    n = dp ? Math.round(n * 10 ** dp) / 10 ** dp : Math.round(n); input.value = String(n);
  };
  const refresh = () => {
    const n = parse(); clear(echo); echo.classList.remove('warn', 'bad');
    if (input.value.trim() === '') return;
    if (n === null || n < 0) { echo.classList.add('bad'); echo.append(el('span.chip', { text: 'Enter a valid number' })); return; }
    if (!dp && !Number.isInteger(n)) { echo.classList.add('bad'); echo.append(el('span.chip', { text: 'Whole numbers only' })); return; }
    if (max !== null && n > max) { echo.classList.add('bad'); echo.append(el('span.chip', { text: `More than the maximum (${fmtNum(max, dp)})` })); return; }
    const u = unitLabel(unitSpec || (cpp ? 'crates' : ''), n);
    echo.append('Check: you entered ', el('b', { text: fmtNum(n, dp) + (u ? ' ' + u : '') }));
    if (cpp && !dp) echo.append(el('span.chip', { text: '= ' + fmtCrates(n, cpp) }));
  };
  input.addEventListener('input', () => { setFieldError(root, ''); refresh(); if (o.onInput) o.onInput(parse()); });
  input.addEventListener('blur', () => { clamp(); refresh(); if (o.onInput) o.onInput(parse()); });
  const step = (d) => { const n = (parse() ?? 0) + d; input.value = String(Math.max(lo(), max !== null ? Math.min(max, n) : n)); input.dispatchEvent(new Event('input', { bubbles: true })); };
  const stepper = el('div.stepper', {}, el('button.btn.secondary', { type: 'button', 'aria-label': 'Decrease', on: { click: () => step(-1) } }, icon('minus')), input,
    el('button.btn.secondary', { type: 'button', 'aria-label': 'Increase', on: { click: () => step(1) } }, icon('plus')));
  // pallets + loose crates entry mode
  let palletMode = false; const pIn = el('input.input', { type: 'text', inputmode: 'numeric', autocomplete: 'off', 'aria-label': 'Pallets', placeholder: 'Pallets' });
  const lIn = el('input.input', { type: 'text', inputmode: 'numeric', autocomplete: 'off', 'aria-label': 'Loose crates', placeholder: 'Loose crates' });
  const pbox = el('div.grid.cols-2', { hidden: true }, pIn, lIn);
  const sync = () => { input.value = String((Math.floor(toNum(pIn.value) || 0)) * cpp + (Math.floor(toNum(lIn.value) || 0))); input.dispatchEvent(new Event('input', { bubbles: true })); };
  pIn.addEventListener('input', sync); lIn.addEventListener('input', sync);
  const modeBtn = cpp && !dp ? el('button.mode-switch', { type: 'button', on: { click: () => {
    palletMode = !palletMode; pbox.hidden = !palletMode; stepper.hidden = palletMode;
    modeBtn.textContent = palletMode ? 'Enter total crates instead' : `Enter as pallets + crates (${cpp} per pallet)`;
    if (palletMode) { const n = parse() || 0; pIn.value = String(Math.floor(n / cpp) || ''); lIn.value = String(n % cpp || ''); pIn.focus(); }
  } } }, `Enter as pallets + crates (${cpp} per pallet)`) : null;
  const root = el('div.field', { dataset: { name: o.name } }, o.label ? el('label', { for: id }, o.label, o.required ? el('span.req', { 'aria-hidden': 'true', text: '*' }) : null) : null,
    stepper, pbox, modeBtn, echo, o.help ? el('p.help', { text: o.help }) : null, err);
  refresh();
  return {
    el: root, input, error: err,
    get value() { return parse(); }, get raw() { return input.value; },
    get max() { return max; },
    set(v) { input.value = v ?? ''; refresh(); },
    setMax(v) { max = v === undefined || v === null || !Number.isFinite(Number(v)) ? null : Number(v); refresh(); },
    setUnit(u) { unitSpec = u; refresh(); },
    clamp() { clamp(); refresh(); },
    get valid() { const n = parse(); return n !== null && n >= lo() && (max === null || n <= max) && (dp || Number.isInteger(n)); },
  };
}

/* ---------------------------------------------------------------- misc */
export async function copyText(text) {
  try { await navigator.clipboard.writeText(text); toast.ok('Copied'); } catch { toast.warn('Could not copy - select and copy manually'); }
}
/**
 * printHeader({title, subtitle, docNo, date}) -> <header class="print-header"> with logomark + "Neersagar" + plant name (from /api/ref/settings).
 * Visible only when printing. Put it first inside the printable region (gate pass, GRN, count sheet...).
 */
export function printHeader({ title = '', subtitle = '', docNo = '', date } = {}) {
  const plant = config.plantName && config.plantName !== 'Neersagar' ? config.plantName : (config.settings && config.settings.plant_code ? `Plant ${config.settings.plant_code}` : 'Mineral water plant');
  return el('header.print-header.print-only', {},
    el('div.ph-brand', {}, icon('neersagar', 'xl'), el('div', {}, el('b.wordmark', { text: 'Neersagar' }), el('div', { text: plant }))),
    el('div.ph-doc', {}, title ? el('b', { text: title }) : null, docNo ? el('div.mono', { text: docNo }) : null, subtitle ? el('div', { text: subtitle }) : null,
      el('div.small', { text: date || fmtDateTime(new Date().toISOString()) })));
}
export function setTitle(t) { document.title = t ? `Neersagar ERP \u2013 ${t}` : 'Neersagar ERP'; }
export function safeNext(next, fallback = '/') {
  if (typeof next !== 'string' || !next.startsWith('/') || next.startsWith('//') || next.startsWith('/\\') || /[\r\n]/.test(next)) return fallback;
  if (/^\/(login|setup)\.html/.test(next)) return fallback;
  return next;
}
/** Run `fn` while a region shows a skeleton; on error show errorState with retry. */
export async function loadInto(region, fn, { skeleton: sk = () => skeletonCards(2), keep = false } = {}) {
  if (!keep || !region.firstChild) mount(region, sk());
  try { const node = await fn(); mount(region, node); return true; }
  catch (e) { if (e && (e.redirecting || e.aborted)) return false; mount(region, errorState(e, () => loadInto(region, fn, { skeleton: sk, keep }))); return false; }
}
