// shell.js - one-line bootstrap for every authenticated page:
//   import { boot, ERP } from '/js/shell.js';
//   const { me } = await boot({ page: 'warehouse', title: 'Warehouse' });
// Does: auth check -> /api/me -> page guard -> app bar + nav + godown switcher + bell + connectivity banner + live events + SW.
import { api, getKey, goLogin, clearKey } from '/js/api.js';
import * as ui from '/js/ui.js';
import * as drafts from '/js/drafts.js';
import * as conn from '/js/connectivity.js';
import { live } from '/js/events.js';
import * as navmod from '/js/nav.js';

const { el, icon, clear, toast, openDialog, fmtRelative, config } = ui;
const { godown } = navmod;

export const ERP = { api, ui, drafts, conn, live, godown, nav: navmod, config, me: null, busy: api.busy };
window.ERP = ERP;

const ME_CACHE = 'erp.me';
const lsGet = (k) => { try { return localStorage.getItem(k); } catch { return null; } };
const lsSet = (k, v) => { try { localStorage.setItem(k, v); } catch { /* ignore */ } };

/* --------------------------------------------------------------------------------------------- DOM scaffold */
function scaffold(title) {
  if (document.querySelector('.shell')) return;
  const main = document.getElementById('main') || el('main#main.page');
  if (!main.parentNode) document.body.append(main);
  main.classList.add('page'); main.setAttribute('tabindex', '-1');
  const shell = el('div.shell', {},
    el('aside#sidebar.sidebar', { 'aria-label': 'Navigation' }),
    el('div.shell-main', {},
      el('header#appbar.appbar', {}, el('span.brand-mark', {}, icon('drop-logo')), el('h1#page-title', { text: title || '' })),
      el('div#banners.banner-host'), main));
  document.body.prepend(shell);
  document.body.prepend(el('a.skip', { href: '#main', text: 'Skip to content' }));
  document.body.append(el('nav#bottomnav.bottom-nav', { 'aria-label': 'Pages' }), el('div#toasts.toasts', { 'aria-live': 'polite' }));
}

/* --------------------------------------------------------------------------------------------- app bar tools */
let bellBtn = null, bellCount = null, liveEl = null;
function buildTools(page, { godownAll }) {
  const bar = document.getElementById('appbar'); if (!bar) return;
  const old = bar.querySelector('.tools'); if (old) old.remove();
  liveEl = el('span.live', { 'data-state': live.status, role: 'status', 'aria-label': 'Live updates' }, el('span.dot'), el('span.lbl', { text: 'Live' }));
  live.onStatus((s) => {
    if (!liveEl) return;
    const off = conn.status();
    const st = !off.online ? 'offline' : s;
    liveEl.dataset.state = st;
    liveEl.querySelector('.lbl').textContent = { live: 'Live', reconnecting: 'Reconnecting...', paused: 'Paused', off: 'Not live', offline: 'Offline' }[st] || '';
  });
  conn.onChange(() => live.status && liveEl && (liveEl.dataset.state = !conn.status().online ? 'offline' : live.status));
  bellCount = el('span.count', { hidden: true });
  bellBtn = el('button.ab-btn', { type: 'button', 'aria-label': 'Notifications', 'aria-haspopup': 'dialog', on: { click: openInbox } }, icon('bell'), bellCount);
  bar.append(el('div.tools', {}, liveEl, godown.button(), bellBtn));
}
async function refreshBell() {
  try {
    const r = await api.get('/api/notifications/inbox/unread-count', null, { retries: 0 });
    const n = (r && r.count) || 0;
    if (bellCount) { bellCount.hidden = n === 0; bellCount.textContent = n > 99 ? '99+' : String(n); }
    if (bellBtn) bellBtn.setAttribute('aria-label', n ? `Notifications, ${n} unread` : 'Notifications');
  } catch (e) { /* bell is best-effort */ }
}

const SEV = { CRITICAL: 'bad', HIGH: 'bad', WARNING: 'warn', MEDIUM: 'warn', INFO: '', LOW: '' };
async function openInbox() {
  const list = el('div.stack.tight', {}, ui.skeletonCards(2));
  const markAll = el('button.btn.secondary.sm', { type: 'button' }, icon('check', 'sm'), 'Mark all read');
  const dlg = openDialog({ title: 'Notifications', body: list, actions: [markAll] });
  async function load() {
    try {
      const r = await api.get('/api/notifications/inbox', { limit: 30 });
      const items = (r && r.items) || [];
      clear(list);
      if (!items.length) { list.append(ui.empty({ icon: 'bell', title: 'All caught up', text: 'New alerts (low stock, QC decisions, overdue jars...) appear here. Choose which ones you get in your profile.' })); markAll.hidden = true; return; }
      markAll.hidden = false;
      items.forEach((a) => list.append(el('button.inbox-item', { type: 'button', class: [a.read ? '' : 'unread', SEV[String(a.severity).toUpperCase()] || ''].join(' '), on: { click: () => open(a) } },
        el('span.ico', {}, icon(SEV[String(a.severity).toUpperCase()] === 'bad' ? 'alert' : SEV[String(a.severity).toUpperCase()] === 'warn' ? 'alert' : 'info')),
        el('span.grow', {}, el('b', { text: a.title }), a.body ? el('span.small', { text: a.body }) : null, el('time', { text: fmtRelative(a.created_at), datetime: a.created_at })))));
    } catch (e) { clear(list); list.append(ui.errorState(e, load)); }
  }
  async function open(a) {
    try { if (!a.read) await api.post(`/api/notifications/inbox/${encodeURIComponent(a.id)}/read`, {}, { retries: 1 }); } catch { /* ignore */ }
    refreshBell();
    const u = ui.safeNext(a.url || '', '');
    if (u) location.href = u; else load();
  }
  markAll.addEventListener('click', () => api.busy(markAll, async () => { try { await api.post('/api/notifications/inbox/read-all', {}); toast.ok('Marked as read'); } catch (e) { ui.toastError(e); } refreshBell(); load(); }));
  load();
  dlg.closed.then(refreshBell);
}

/* --------------------------------------------------------------------------------------------- session */
export async function logout() {
  try { await api.post('/api/me/logout', {}, { retries: 0, noAuthRedirect: true }); } catch { /* the key may already be gone */ }
  try { localStorage.removeItem(ME_CACHE); } catch { /* ignore */ }
  clearKey(); location.replace('/login.html');
}

async function loadMe() {
  for (;;) {
    try {
      const me = await api.get('/api/me', null, { retries: 1 });
      lsSet(ME_CACHE, JSON.stringify(me)); return { me, cached: false };
    } catch (e) {
      if (e.redirecting) throw e;
      if (e.status === 401) { goLogin(); throw e; }
      if (e.network || e.status >= 500) {
        const c = lsGet(ME_CACHE);
        if (c) { try { const me = JSON.parse(c); toast.warn('Server unreachable - showing the last known menu', { ms: 5000 }); return { me, cached: true }; } catch { /* ignore */ } }
        const m = document.getElementById('main'); if (m) { clear(m); m.append(ui.errorState(e, null), el('p.center.muted', { text: 'Trying again automatically...' })); }
        await ui.sleep(4000); continue;
      }
      throw e;
    }
  }
}

async function loadSettings() {
  try {
    const s = await api.get('/api/ref/settings', null, { retries: 1 });
    ui.setConfig({ settings: s, plantName: s.plant_name || config.plantName, utcOffsetMinutes: Number.isFinite(Number(s.utc_offset_minutes)) ? Number(s.utc_offset_minutes) : config.utcOffsetMinutes, qrLabels: !!s.qr_labels_enabled });
  } catch (e) { if (e && e.redirecting) throw e; /* module not ready: QR off, browser time zone */ }
}

/* --------------------------------------------------------------------------------------------- boot */
let bootCtx = null;
export async function boot({ page, title, godownAll = false, guard = true, live: wantLive = true, livePage } = {}) {
  if (!getKey()) { goLogin(); return new Promise(() => {}); }
  scaffold(title);
  ui.setTitle(title);
  const h1 = document.getElementById('page-title'); if (h1 && title) h1.textContent = title;
  try { const { registerSW } = await import('/js/pwa.js'); registerSW(); } catch { /* optional */ }
  conn.start(document.getElementById('banners'));
  ui.mount(document.getElementById('main'), ui.skeletonCards(3));

  const { me } = await loadMe();
  ERP.me = me; ui.setConfig({ user: me }); drafts.setUser(me.id); drafts.purgeOld();
  await loadSettings();
  if (guard && !navmod.guard(me, page)) return new Promise(() => {});
  await godown.init(me, { allowAll: godownAll });
  navmod.showFlash();

  const doLogout = () => ui.confirmDialog({ title: 'Sign out?', message: 'Unsent drafts stay on this device.', confirmText: 'Sign out' }).then((y) => y && logout());
  const paintNav = () => navmod.renderNav(ERP.me, page, { onLogout: doLogout });
  buildTools(page, { godownAll });
  paintNav();
  refreshBell();
  ui.setTitle(title);
  document.documentElement.dataset.page = page || '';

  // --- shell-level live handlers (every page)
  live.internal({
    alerts: refreshBell,
    users: async () => {
      let fresh; try { fresh = await api.get('/api/me', null, { retries: 1 }); } catch { return; }
      const pick = (m) => JSON.stringify([m.allowed_pages, m.is_admin, m.full_name]);
      const changed = pick(ERP.me) !== pick(fresh);
      const locChanged = JSON.stringify(ERP.me.allowed_location_ids || []) !== JSON.stringify(fresh.allowed_location_ids || []);
      ERP.me = fresh; lsSet(ME_CACHE, JSON.stringify(fresh)); ui.setConfig({ user: fresh });
      if (!changed && !locChanged) return;
      toast.warn('Your access was updated by an administrator', { ms: 5000 });
      if (guard && !navmod.guard(fresh, page)) return;
      paintNav();
      if (locChanged) { await godown.init(fresh, { allowAll: godownAll }); buildTools(page, { godownAll }); }
    },
    settings: async () => { const q = config.qrLabels; await loadSettings(); if (q !== config.qrLabels) { paintNav(); if (guard) navmod.guard(ERP.me, page); } },
  });
  if (wantLive) live.start(livePage || page, typeof wantLive === 'object' ? wantLive : {});
  bootCtx = { me, page };
  return { me, ERP };
}

export default ERP;
