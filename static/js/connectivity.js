// connectivity.js - health ping every 10 s + online/offline events + failed-fetch hints -> sticky banner + app-bar indicator.
import { el, icon, toast, clear } from '/js/ui.js';

const PING_MS = 10000;
let state = { online: navigator.onLine, server: 'ok', since: Date.now() };   // server: ok | unreachable | restarting | degraded
const subs = new Set();
let timer = 0, banner = null, host = null, started = false, pinging = false, wasDown = false;

export function status() { return { ...state }; }
export function isDown() { return !state.online || state.server !== 'ok'; }
export function onChange(fn) { subs.add(fn); return () => subs.delete(fn); }
function set(next) {
  const changed = next.online !== state.online || next.server !== state.server;
  state = { ...state, ...next, since: changed ? Date.now() : state.since };
  if (changed) { render(); subs.forEach((f) => { try { f(status()); } catch { /* ignore */ } }); }
  if (changed && !isDown() && wasDown) { wasDown = false; toast.ok('Back online', { ms: 2500 }); window.dispatchEvent(new CustomEvent('erp:reconnected')); }
  if (isDown()) wasDown = true;
}
/** Server told us it is restarting (SSE `server_restarting`) - shows the banner immediately. */
export function markRestarting() { set({ server: 'restarting' }); schedule(2000); }

async function ping() {
  if (pinging) return; pinging = true;
  const ctl = new AbortController(); const t = setTimeout(() => ctl.abort(), 6000);
  try {
    const r = await fetch('/api/health', { cache: 'no-store', signal: ctl.signal, headers: { Accept: 'application/json' } });
    if (r.ok) set({ online: navigator.onLine || true, server: 'ok' });
    else {
      let j = null; try { j = await r.json(); } catch { /* ignore */ }
      set({ online: true, server: j && j.status === 'shutting_down' ? 'restarting' : (j && j.status === 'degraded') ? 'degraded' : 'unreachable' });
    }
  } catch { set({ server: navigator.onLine ? 'unreachable' : 'unreachable', online: navigator.onLine }); }
  finally { clearTimeout(t); pinging = false; schedule(isDown() ? 3000 : PING_MS); }
}
function schedule(ms) { clearTimeout(timer); timer = setTimeout(ping, ms); }

function render() {
  if (!host) return;
  if (banner) { banner.remove(); banner = null; }
  if (!isDown()) return;
  let text, sub, ic = 'wifi-off', tone = 'err';
  if (!state.online) { text = 'You are offline'; sub = 'Nothing is lost - your entries are kept on this phone and can be sent when you are back online.'; }
  else if (state.server === 'restarting') { text = 'Server restarting...'; sub = 'This takes a few seconds. Your entries are kept; the page reconnects by itself.'; tone = 'warn'; ic = 'refresh'; }
  else if (state.server === 'degraded') { text = 'Database temporarily unavailable'; sub = 'Nothing was saved from the last attempt. Your entries are kept - try again in a moment.'; tone = 'warn'; ic = 'alert'; }
  else { text = 'Server unreachable - nothing is lost, your entries are kept'; sub = 'We keep retrying. You can continue typing; press Retry on a form once the banner is gone.'; }
  banner = el('div.banner', { class: tone, role: 'status', 'aria-live': 'polite' }, icon(ic, state.server === 'restarting' ? 'spin' : ''),
    el('div.grow', {}, el('b', { text }), el('div', { style: { fontWeight: 500 }, text: sub })),
    el('button.btn.secondary.sm', { type: 'button', on: { click: () => { schedule(0); } } }, 'Check now'));
  host.append(banner);
}

export function start(bannerHost) {
  host = bannerHost || document.getElementById('banners');
  if (started) { render(); return; }
  started = true;
  window.addEventListener('online', () => { set({ online: true }); schedule(0); });
  window.addEventListener('offline', () => set({ online: false }));
  // api.js tells us about every fetch outcome: a failure triggers an immediate re-check, a success proves we are fine
  window.addEventListener('erp:net', (e) => {
    if (e.detail && e.detail.ok === false) { schedule(250); }
    else if (e.detail && e.detail.ok && state.server !== 'ok') set({ server: 'ok', online: true });
  });
  document.addEventListener('visibilitychange', () => { if (!document.hidden) schedule(0); });
  schedule(PING_MS);
  render();
}
export function stop() { clearTimeout(timer); started = false; if (host && banner) clear(host); }
export default { start, stop, status, isDown, onChange, markRestarting };
