// setup.html - first-run administrator creation (needs the one-time setup token from the server log).
import { api, getKey } from '/js/api.js';
import { el, icon, formError, setFieldError, clearFormError, mount } from '/js/ui.js';
import * as conn from '/js/connectivity.js';
import * as drafts from '/js/drafts.js';
import { policy, passwordRules, wirePasswordToggle } from '/js/pwcheck.js';

conn.start(document.getElementById('banners'));
const form = document.getElementById('setup');
const stateEl = document.getElementById('state');
const $ = (id) => document.getElementById(id);
let tokenRequired = true;

async function init() {
  try {
    const s = await api.get('/api/setup/status', null, { noAuthRedirect: true, retries: 2 });
    if (!s.setup_required) {
      mount(stateEl, el('div.stack.tight.center', {}, el('div.banner.ok.inline', {}, icon('check-circle'), el('div.grow', {}, el('b', { text: 'Setup is already done.' }), el('div', { text: 'An administrator exists, so this screen is switched off.' }))),
        el('a.btn.primary.block', { href: getKey() ? '/' : '/login.html' }, 'Continue')));
      stateEl.classList.remove('loading-screen'); return;
    }
    tokenRequired = s.token_required !== false;
    $('token-field').hidden = !tokenRequired;
    stateEl.hidden = true; form.hidden = false;
    drafts.setUser('setup'); drafts.bind(form, { page: 'setup', id: 'admin' });
    (tokenRequired ? $('setup_token') : $('full_name')).focus();
  } catch (e) {
    mount(stateEl, el('div.stack.tight', {}, el('div.banner.warn.inline', {}, icon('wifi-off'), el('div.grow', {}, el('b', { text: e.message }), el('div', { text: e.hint || '' }))),
      el('button.btn.primary.block', { type: 'button', on: { click: () => { stateEl.hidden = false; mount(stateEl, 'Checking...'); init(); } } }, icon('refresh'), 'Try again')));
    stateEl.classList.remove('loading-screen');
  }
}

const pw = $('password'), meter = $('meter'), rules = $('rules');
const paint = () => passwordRules(rules, meter, pw.value, $('username').value);
pw.addEventListener('input', paint); $('username').addEventListener('input', paint); paint();
wirePasswordToggle($('toggle'), [pw, $('confirm')]);

form.addEventListener('submit', async (ev) => {
  ev.preventDefault(); clearFormError(form);
  const v = { setup_token: $('setup_token').value.trim(), full_name: $('full_name').value.trim(), username: $('username').value.trim(), password: pw.value, confirm: $('confirm').value };
  const fields = {};
  if (tokenRequired && !v.setup_token) fields.setup_token = 'Enter the setup code from the server log';
  if (!v.full_name) fields.full_name = 'Enter your name';
  if (!/^[A-Za-z0-9._-]{3,40}$/.test(v.username)) fields.username = '3-40 characters: letters, digits, . - _';
  const p = policy(v.password, v.username);
  if (!p.ok) fields.password = 'Does not meet the password rules below';
  if (v.password !== v.confirm) fields.confirm = 'Passwords do not match';
  if (Object.keys(fields).length) { formError(form, { fields, status: 422, message: '' }); return; }
  const body = { username: v.username, full_name: v.full_name, password: v.password };
  if (tokenRequired) body.setup_token = v.setup_token;
  await api.busy($('submit'), async () => {
    try {
      const r = await api.post('/api/setup/admin', body, { noAuthRedirect: true, retries: 1 });
      drafts.clear('setup', 'admin');
      if (!getKey() && r && r.key) document.cookie = `erp_key=${encodeURIComponent(r.key)}; Path=/; SameSite=Strict; Max-Age=2592000${location.protocol === 'https:' ? '; Secure' : ''}`;
      location.replace('/');
    } catch (e) {
      if (e.status === 403) setFieldError($('setup_token'), 'That setup code is not valid. Copy it again from the server log.');
      if (e.code === 'SETUP_DISABLED') { init(); return; }
      formError(form, e);
    }
  });
});
init();
