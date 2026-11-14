// index.html: decide where to go. No key -> first-run setup or login; key -> first allowed page.
import { api, getKey, clearKey } from '/js/api.js';
import { firstAllowed } from '/js/nav.js';
import { el, icon, mount, setConfig } from '/js/ui.js';

const msg = document.getElementById('msg');
async function go() {
  if (!getKey()) {
    try {
      const s = await api.get('/api/setup/status', null, { noAuthRedirect: true, retries: 2 });
      location.replace(s && s.setup_required ? '/setup.html' : '/login.html');
    } catch (e) { fail(e); }
    return;
  }
  try {
    const me = await api.get('/api/me', null, { noAuthRedirect: true, retries: 2 });
    try { const s = await api.get('/api/ref/settings', null, { retries: 0 }); setConfig({ qrLabels: !!s.qr_labels_enabled }); } catch { /* optional */ }
    location.replace(firstAllowed(me).href);
  } catch (e) {
    if (e.status === 401) { clearKey(); location.replace('/login.html'); return; }
    fail(e);
  }
}
function fail(e) {
  mount(msg, el('div.stack.tight.center', {}, el('div.banner.warn.inline', {}, icon('wifi-off'), el('div.grow', {}, el('b', { text: e.message || 'Cannot reach the server' }), el('div', { text: e.hint || '' }))),
    el('button.btn.primary.block', { type: 'button', on: { click: () => { mount(msg, 'Opening...'); go(); } } }, icon('refresh'), 'Try again')));
  msg.classList.remove('loading-screen');
}
go();
