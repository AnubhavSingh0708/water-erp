// nav.js - navigation registry, sidebar / bottom nav / More drawer, page guard, godown switcher.
import { el, icon, clear, openDialog, config, initials, toast } from '/js/ui.js';
import { api } from '/js/api.js';

/** Registry of ALL pages. `perm` = key in /api/me allowed_pages that grants it (default = key). */
export const PAGES = [
  { key: 'dashboard', href: '/dashboard.html', label: 'Dashboard', short: 'Home', icon: 'dashboard' },
  { key: 'raw-materials', href: '/raw-materials.html', label: 'Raw materials', short: 'Materials', icon: 'box' },
  { key: 'production', href: '/production.html', label: 'Production', short: 'Production', icon: 'factory' },
  { key: 'qc', href: '/qc.html', label: 'Lab QC', short: 'QC', icon: 'flask' },
  { key: 'warehouse', href: '/warehouse.html', label: 'Warehouse', short: 'Stock', icon: 'warehouse' },
  { key: 'godowns', href: '/godowns.html', label: 'Godown transfers', short: 'Transfers', icon: 'transfer', perm: 'warehouse', child: true },
  { key: 'qr-labels', href: '/qr-labels.html', label: 'QR labels', short: 'QR', icon: 'qr', needsQr: true },
  { key: 'loading', href: '/loading.html', label: 'Loading & dispatch', short: 'Loading', icon: 'truck' },
  { key: 'jars', href: '/jars.html', label: 'Returnable jars', short: 'Jars', icon: 'jar' },
  { key: 'maintenance', href: '/maintenance.html', label: 'Maintenance', short: 'Maintain', icon: 'wrench' },
  { key: 'admin', href: '/admin.html', label: 'Administration', short: 'Admin', icon: 'shield', adminOnly: true },
];
export const PROFILE = { key: 'profile', href: '/profile.html', label: 'My profile', short: 'Profile', icon: 'user' };

export function isAllowed(me, def) {
  if (!me) return false;
  if (def.key === 'profile') return true;
  if (def.adminOnly) return !!me.is_admin;
  if (def.needsQr && !config.qrLabels) return false;
  const perm = def.perm || def.key;
  return !!me.is_admin || (me.allowed_pages || []).includes(perm);
}
export const visiblePages = (me) => PAGES.filter((p) => isAllowed(me, p));
export function firstAllowed(me) { const v = visiblePages(me).filter((p) => !p.child); return v.length ? v[0] : PROFILE; }

/** Page guard: false (and a redirect + flash toast) when the user may not open `pageKey`. */
export function guard(me, pageKey) {
  if (pageKey === 'profile' || pageKey === 'public') return true;
  const def = PAGES.find((p) => p.key === pageKey);
  if (!def) return true;
  if (isAllowed(me, def)) return true;
  const target = firstAllowed(me);
  try { sessionStorage.setItem('erp.flash', JSON.stringify({ msg: `You do not have access to ${def.label}.`, type: 'warn' })); } catch { /* ignore */ }
  if (target.href !== location.pathname) location.replace(target.href);
  return false;
}
export function showFlash() {
  try { const f = sessionStorage.getItem('erp.flash'); if (f) { sessionStorage.removeItem('erp.flash'); const o = JSON.parse(f); toast(o.msg, { type: o.type || 'info', ms: 6000 }); } } catch { /* ignore */ }
}

const link = (def, cls, current, label) => el('a', { class: cls, href: def.href, 'aria-current': def.key === current ? 'page' : undefined }, icon(def.icon), el('span.lbl', { text: label ?? def.label }));

/** Build sidebar (>=1024), bottom nav (mobile) from /api/me. Safe to call again when permissions change. */
export function renderNav(me, current, { onLogout } = {}) {
  const items = visiblePages(me);
  const side = document.getElementById('sidebar');
  if (side) {
    clear(side);
    side.append(
      el('a.brand', { href: firstAllowed(me).href }, el('span.brand-mark', {}, icon('drop-logo', 'lg')), el('span', {}, el('b', { text: config.plantName || 'Water ERP' }), el('span.small', { text: 'Plant operations' }))),
      el('nav', { 'aria-label': 'Main' }, items.map((d) => link(d, 'nav-item' + (d.child ? ' child' : ''), current))),
      el('div.side-foot', {},
        el('div.who', {}, el('b', { text: me.full_name || me.username }), me.is_admin ? 'Administrator' : '@' + me.username),
        link(PROFILE, 'nav-item', current),
        el('button.nav-item', { type: 'button', on: { click: onLogout } }, icon('logout'), 'Sign out')));
  }
  const bn = document.getElementById('bottomnav');
  if (bn) {
    clear(bn);
    const top = items.filter((d) => !d.child);
    let shown = top;
    const needMore = top.length > 5;
    if (needMore) {
      shown = top.slice(0, 4);
      const cur = top.find((d) => d.key === current);
      if (cur && !shown.includes(cur)) shown[3] = cur;
    }
    shown.forEach((d) => bn.append(link(d, '', current, d.short)));
    if (needMore || items.length > top.length || true) {
      const more = el('button', { type: 'button', 'aria-haspopup': 'dialog', 'aria-label': 'More pages', on: { click: () => openMore(me, current, onLogout) } }, icon('more'), el('span.lbl', { text: 'More' }));
      bn.append(more);
    }
  }
}

function openMore(me, current, onLogout) {
  const items = visiblePages(me);
  const tile = (d) => el('a.nav-tile', { href: d.href, 'aria-current': d.key === current ? 'page' : undefined }, icon(d.icon), d.short === d.label ? d.label : d.label);
  const dlg = openDialog({
    title: 'All pages',
    body: el('div.stack', {},
      el('div.nav-grid', {}, items.map(tile), tile(PROFILE),
        el('button.nav-tile', { type: 'button', on: { click: () => { dlg.close(); onLogout(); } } }, icon('logout'), 'Sign out')),
      el('p.small.muted.center', { text: `${me.full_name || me.username}${me.is_admin ? ' - Administrator' : ''}` })),
    sheet: true,
  });
}

/* --------------------------------------------------------------------------------------------- godown switcher */
const MAIN = '00000000-0000-0000-0000-000000000001';
const subs = new Set();
const store = { get(k) { try { return localStorage.getItem(k); } catch { return null; } }, set(k, v) { try { localStorage.setItem(k, v); } catch { /* ignore */ } } };

export const godown = {
  list: [], cur: null, allowAll: false, userId: 'anon', loaded: false,
  /** Load the godowns the caller may use. Never throws (a missing endpoint = no switcher). */
  async init(me, { allowAll = false } = {}) {
    this.userId = me.id || 'anon'; this.allowAll = allowAll;
    try { const l = await api.get('/api/ref/locations', null, { retries: 1 }); this.list = Array.isArray(l) ? l.filter((x) => x.is_active !== false) : []; }
    catch (e) { if (e && e.redirecting) throw e; this.list = []; }
    this.loaded = true;
    const saved = store.get(`erp.godown.${this.userId}`);
    const ok = (id) => id && (this.list.some((g) => g.id === id) || (id === 'all' && this.allowAll && this.list.length > 1));
    this.cur = ok(saved) ? saved : (this.list.find((g) => g.id === MAIN) || this.list[0] || {}).id || null;
    if (this.list.length <= 1 && this.cur === 'all') this.cur = this.list[0]?.id || null;
    return this;
  },
  get multiple() { return this.list.length > 1; },
  /** Active godown id, 'all', or null when the server has no godown list. */
  current() { return this.cur; },
  currentObj() { return this.list.find((g) => g.id === this.cur) || null; },
  currentName() { return this.cur === 'all' ? 'All godowns' : (this.currentObj() || {}).name || ''; },
  /** Query/body fragment: {location_id}. Merge into requests: api.get(url, {...ERP.godown.param(), q}) */
  param() { return this.cur ? { location_id: this.cur } : {}; },
  set(id) {
    if (id === this.cur) return;
    this.cur = id; store.set(`erp.godown.${this.userId}`, id);
    subs.forEach((f) => { try { f(id); } catch (e) { console.error(e); } });
    window.dispatchEvent(new CustomEvent('erp:godown', { detail: { location_id: id, location: this.currentObj() } }));
  },
  onChange(fn) { subs.add(fn); return () => subs.delete(fn); },
  /** App-bar button (null when only one godown). Opens a sheet to choose. */
  button() {
    if (!this.multiple) return null;
    const lbl = el('span.lbl'); const btn = el('button.ab-btn.godown-btn', { type: 'button', 'aria-haspopup': 'dialog', 'aria-label': 'Change godown' }, icon('pin'), lbl, icon('chevron-down', 'sm'));
    const paint = () => { lbl.textContent = this.cur === 'all' ? 'All godowns' : ((this.currentObj() || {}).code || (this.currentObj() || {}).name || 'Godown'); btn.setAttribute('aria-label', `Godown: ${this.currentName()}. Change`); };
    paint(); this.onChange(paint);
    btn.addEventListener('click', () => this.chooser());
    return btn;
  },
  chooser() {
    const rows = [...(this.allowAll ? [{ id: 'all', name: 'All godowns', code: '', kind: '', city: 'Combined view' }] : []), ...this.list];
    const dlg = openDialog({
      title: 'Choose godown',
      body: el('div.checks', { role: 'radiogroup', 'aria-label': 'Godown' }, rows.map((g) => el('label.check', {},
        el('input', { type: 'radio', name: 'godown', value: g.id, checked: g.id === this.cur, on: { change: () => { this.set(g.id); dlg.close(); } } }),
        el('span', {}, g.name, el('span.sub', { text: [g.code, g.kind === 'PLANT' ? 'Plant' : g.kind === 'GODOWN' ? 'Godown' : '', g.city].filter(Boolean).join(' - ') }))))),
    });
  },
};
