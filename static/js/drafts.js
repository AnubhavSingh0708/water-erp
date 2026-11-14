// drafts.js - autosave form entries to localStorage (per user + page + form), restore after reload / re-login,
// clear on success. Never stores passwords, tokens or the auth key.
import { el, icon, debounce } from '/js/ui.js';

const PREFIX = 'erp.draft.';
const MAX_AGE_MS = 7 * 24 * 3600 * 1000;
const SECRET = /pass|token|secret|key|otp|pin$/i;
let userId = 'anon';
export function setUser(id) { userId = id || 'anon'; }

const k = (page, form) => `${PREFIX}${userId}.${page}.${form}`;
function ls() { try { return window.localStorage; } catch { return null; } }

export function load(page, form) {
  const s = ls(); if (!s) return null;
  try {
    const raw = s.getItem(k(page, form)); if (!raw) return null;
    const d = JSON.parse(raw);
    if (!d || Date.now() - d.at > MAX_AGE_MS) { s.removeItem(k(page, form)); return null; }
    return d;
  } catch { return null; }
}
export function save(page, form, values) {
  const s = ls(); if (!s) return;
  try { if (values && Object.keys(values).length) s.setItem(k(page, form), JSON.stringify({ at: Date.now(), values })); else s.removeItem(k(page, form)); } catch { /* quota */ }
}
export function clear(page, form) { const s = ls(); if (s) try { s.removeItem(k(page, form)); } catch { /* ignore */ } }
export function purgeOld() {
  const s = ls(); if (!s) return;
  try { for (let i = s.length - 1; i >= 0; i--) { const key = s.key(i); if (key && key.startsWith(PREFIX)) { const d = JSON.parse(s.getItem(key) || 'null'); if (!d || Date.now() - d.at > MAX_AGE_MS) s.removeItem(key); } } } catch { /* ignore */ }
}

function eligible(c) {
  if (!c.name || c.disabled || c.dataset.nodraft !== undefined) return false;
  if (['password', 'hidden', 'file', 'submit', 'button', 'reset'].includes(c.type)) return false;
  return !SECRET.test(c.name);
}
function collect(form) {
  const v = {};
  for (const c of form.elements) {
    if (!eligible(c)) continue;
    if (c.type === 'checkbox') { const key = c.dataset.group ? `${c.dataset.group}[]${c.value}` : c.name; v[key] = c.checked; }
    else if (c.type === 'radio') { if (c.checked) v[c.name] = c.value; }
    else v[c.name] = c.value;
  }
  return v;
}
function apply(form, values) {
  let n = 0;
  for (const c of form.elements) {
    if (!eligible(c)) continue;
    let key = c.name, val;
    if (c.type === 'checkbox') { key = c.dataset.group ? `${c.dataset.group}[]${c.value}` : c.name; if (!(key in values)) continue; if (c.checked !== !!values[key]) { c.checked = !!values[key]; n++; } }
    else if (c.type === 'radio') { if (values[c.name] !== undefined && (c.checked = values[c.name] === c.value)) n++; }
    else { if (!(key in values)) continue; val = values[key]; if (c.value !== val) { c.value = val; n++; } }
    c.dispatchEvent(new Event('input', { bubbles: true })); c.dispatchEvent(new Event('change', { bubbles: true }));
  }
  return n;
}
const sig = (v) => JSON.stringify(v);

/**
 * bind(form, {page, id, banner:true}) -> {clear(), discard(), restore()}
 * Autosaves on input/change (debounced), restores previously saved values and shows a "Draft restored" banner.
 * Call handle.clear() after a successful submit.
 */
export function bind(form, { page, id = 'main', banner = true, baseline } = {}) {
  const initial = sig(baseline || collect(form));
  const save_ = debounce(() => { const v = collect(form); if (sig(v) === initial) clear(page, id); else save(page, id, v); }, 350);
  const onChange = () => save_();
  form.addEventListener('input', onChange); form.addEventListener('change', onChange);
  let bar = null;
  const restore = () => {
    const d = load(page, id); if (!d) return false;
    const n = apply(form, d.values); if (!n) return false;
    if (banner) {
      bar = el('div.banner.info.inline.draft-banner', { role: 'status' }, icon('history'),
        el('div.grow', {}, el('b', { text: 'Draft restored' }), ' - your earlier entries were kept. Check them before saving.'),
        el('button.btn.ghost.sm', { type: 'button', on: { click: () => handle.discard() } }, 'Discard draft'));
      form.prepend(bar);
    }
    return true;
  };
  const handle = {
    restore,
    clear() { save_.cancel(); clear(page, id); if (bar) { bar.remove(); bar = null; } },
    discard() { save_.cancel(); clear(page, id); if (bar) { bar.remove(); bar = null; } form.reset(); form.dispatchEvent(new Event('input', { bubbles: true })); },
    detach() { form.removeEventListener('input', onChange); form.removeEventListener('change', onChange); save_.cancel(); },
  };
  restore();
  return handle;
}
export default { bind, load, save, clear, setUser, purgeOld };
