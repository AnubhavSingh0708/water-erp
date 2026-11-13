// login.html
import { api, getKey, clearKey } from '/js/api.js';
import { el, icon, safeNext, formError, setFieldError, clearFormError, mount } from '/js/ui.js';
import * as conn from '/js/connectivity.js';
import { registerSW } from '/js/pwa.js';

registerSW();
conn.start(document.getElementById('banners'));
const form = document.getElementById('login');
const user = document.getElementById('username');
const pass = document.getElementById('password');
const btn = document.getElementById('submit');
const next = safeNext(new URLSearchParams(location.search).get('next'), '/');

try { const last = localStorage.getItem('erp.last_user'); if (last) user.value = last; } catch { /* ignore */ }
(user.value ? pass : user).focus();

// already signed in? skip the form
(async () => {
  if (!getKey()) return;
  try { await api.get('/api/me', null, { noAuthRedirect: true, retries: 0 }); location.replace(next); } catch { clearKey(); }
})();

document.getElementById('toggle').addEventListener('click', (e) => {
  const b = e.currentTarget; const show = pass.type === 'password';
  pass.type = show ? 'text' : 'password'; b.setAttribute('aria-pressed', String(show)); b.setAttribute('aria-label', show ? 'Hide password' : 'Show password');
  b.replaceChildren(icon(show ? 'eye-off' : 'eye'));
});

api.get('/api/ref/settings', null, { retries: 0, noAuthRedirect: true }).then((s) => { if (s && s.plant_name) { document.getElementById('plant').textContent = s.plant_name === 'Neersagar' ? 'Neersagar ERP' : s.plant_name; } }).catch(() => {});
api.get('/api/setup/status', null, { retries: 0, noAuthRedirect: true }).then((s) => {
  if (s && s.setup_required) mount(document.getElementById('foot'), 'First time here? ', el('a', { href: '/setup.html', style: { color: 'inherit', fontWeight: 700 }, text: 'Create the administrator account' }));
}).catch(() => {});

form.addEventListener('submit', async (ev) => {
  ev.preventDefault();
  clearFormError(form);
  const username = user.value.trim(), password = pass.value;
  let bad = false;
  if (!username) { setFieldError(user, 'Enter your username'); bad = true; }
  if (!password) { setFieldError(pass, 'Enter your password'); bad = true; }
  if (bad) { (username ? pass : user).focus(); return; }
  await api.busy(btn, async () => {
    try {
      const r = await api.post('/api/auth/login', { username, password }, { noAuthRedirect: true, noRetry429: true, retries: 1 });
      try { localStorage.setItem('erp.last_user', username); localStorage.removeItem('erp.me'); } catch { /* ignore */ }
      if (!getKey() && r && r.key) {   // cookie refused (e.g. plain http on a LAN name): keep the session working anyway
        document.cookie = `erp_key=${encodeURIComponent(r.key)}; Path=/; SameSite=Strict; Max-Age=2592000${location.protocol === 'https:' ? '; Secure' : ''}`;
      }
      location.replace(next);
    } catch (e) {
      pass.value = '';
      if (e.status === 401) { formError(form, { ...e, fields: null, hint: 'Check the username and password, then try again.' }); user.focus(); }
      else if (e.status === 429 || e.code === 'LOCKED') formError(form, { ...e, hint: e.hint || 'Too many attempts. Wait a few minutes, or ask an administrator to unlock the account.' });
      else formError(form, e);
    }
  });
});
