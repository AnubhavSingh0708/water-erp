// profile.html - account, change password, notification subscriptions, Web Push, godown, install, sign out.
import { boot, ERP, logout } from '/js/shell.js';
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import * as pwa from '/js/pwa.js';
import { policy, passwordRules, wirePasswordToggle } from '/js/pwcheck.js';
import { visiblePages } from '/js/nav.js';

const { el, icon, toast } = ui;
const { me } = await boot({ page: 'profile', title: 'My profile' });
const main = document.getElementById('main');

const AREAS = [
  ['Stock & materials', ['LOW_STOCK', 'REORDER_RECOMMENDED', 'STOCK_EXPIRY']],
  ['Production & lab QC', ['QC_PENDING', 'QC_APPROVED', 'QC_REJECTED', 'WATER_OUT_OF_RANGE']],
  ['Loading & dispatch', ['GATE_PASS_ISSUED', 'TRIP_SETTLED']],
  ['Returnable jars', ['JAR_OVERDUE', 'JAR_FLEET_ALERT']],
  ['Maintenance & vehicles', ['MAINTENANCE_DUE', 'VEHICLE_DOC_EXPIRY']],
];

/* ------------------------------------------------------------------ account */
function accountCard() {
  const cur = () => ERP.me || me;
  const pages = visiblePages(cur()).filter((p) => !p.child);
  return el('section.card', {}, el('div.row.nowrap', {}, el('span.avatar', { text: ui.initials(cur().full_name || cur().username) }),
    el('div.grow', {}, el('h2', { text: cur().full_name || cur().username }), el('p.muted.small', { text: `@${cur().username}${cur().is_admin ? ' - Administrator' : ''}` }))),
    el('hr'), el('p.section-title', { text: 'You can open' }),
    el('div.chips', {}, pages.map((p) => el('a.badge.info.plain', { href: p.href }, icon(p.icon), p.label)), el('span.badge.plain', {}, icon('user'), 'My profile')));
}

/* ------------------------------------------------------------------ godowns */
function godownCard() {
  const g = ERP.godown;
  if (!g.list.length) return null;
  const card = el('section.card');
  const paint = () => {
    ui.mount(card, el('div.card-head', {}, el('h2', {}, icon('pin'), 'Godown')),
      g.multiple ? el('p.muted.small', { text: 'Pick the godown you are working in. Stock lists and entries use it. You can also switch from the top bar.' }) : el('p.muted.small', { text: 'You work in a single godown.' }),
      el('div.checks', { role: 'radiogroup', 'aria-label': 'Active godown' }, g.list.map((x) => el('label.check', {},
        el('input', { type: 'radio', name: 'godown', value: x.id, checked: x.id === g.cur, disabled: !g.multiple, on: { change: () => { g.set(x.id); toast.ok(`Working in ${x.name}`); } } }),
        el('span', {}, x.name, el('span.sub', { text: [x.code, x.city].filter(Boolean).join(' - ') }))))));
  };
  paint(); g.onChange(paint);
  return card;
}

/* ------------------------------------------------------------------ password */
function passwordCard() {
  const cur = el('input.input', { type: 'password', name: 'current_password', autocomplete: 'current-password', maxlength: 200, 'data-nodraft': '1', id: 'pw-cur' });
  const nw = el('input.input', { type: 'password', name: 'new_password', autocomplete: 'new-password', maxlength: 200, 'data-nodraft': '1', id: 'pw-new' });
  const rep = el('input.input', { type: 'password', name: 'confirm', autocomplete: 'new-password', maxlength: 200, 'data-nodraft': '1', id: 'pw-rep' });
  const tg = el('button.addon', { type: 'button', 'aria-label': 'Show passwords', 'aria-pressed': 'false' }, icon('eye'));
  wirePasswordToggle(tg, [cur, nw, rep]);
  const meter = el('div.pw-meter', { 'data-score': '0', 'aria-hidden': 'true' }, el('i'), el('i'), el('i'), el('i'));
  const rules = el('ul.rules');
  const paint = () => passwordRules(rules, meter, nw.value, me.username); nw.addEventListener('input', paint); paint();
  const submit = el('button.btn.primary', { type: 'submit' }, el('span.btn-icon', {}, icon('key')), 'Change password');
  const form = el('form.stack', { novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }),
    el('div.field', {}, el('label', { for: 'pw-cur', text: 'Current password' }), cur, el('p.error')),
    el('div.field', {}, el('label', { for: 'pw-new', text: 'New password' }), el('div.input-group', {}, nw, tg), meter, rules, el('p.error')),
    el('div.field', {}, el('label', { for: 'pw-rep', text: 'Repeat new password' }), rep, el('p.error')),
    el('p.help', { text: 'Changing your password signs you out on all your other devices.' }), submit);
  const act = api.action();
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    const f = {};
    if (!cur.value) f.current_password = 'Enter your current password';
    if (!policy(nw.value, me.username).ok) f.new_password = 'At least 10 characters and not the same as your username';
    if (nw.value !== rep.value) f.confirm = 'Passwords do not match';
    if (Object.keys(f).length) { ui.formError(form, { status: 422, fields: f, message: '' }); return; }
    await api.busy(submit, async () => {
      try { await act.post('/api/me/password', { current_password: cur.value, new_password: nw.value }); form.reset(); paint(); ui.formBanner(form, 'Password changed. Your other sessions were signed out.'); toast.ok('Password changed'); }
      catch (e) { ui.formError(form, e); }
    });
  });
  return el('section.card', {}, el('div.card-head', {}, el('h2', {}, icon('lock'), 'Change password')), form);
}

/* ------------------------------------------------------------------ notifications */
function notificationsCard() {
  const card = el('section.card'); let types = []; let base = []; let loaded = false;
  const form = el('form.stack', { novalidate: true, id: 'subs' });
  const save = el('button.btn.primary', { type: 'submit', disabled: true }, el('span.btn-icon', {}, icon('save')), 'Save notification choices');
  const boxes = new Map();
  const dirty = () => { const now = [...boxes].filter(([, b]) => b.checked).map(([c]) => c).sort(); return JSON.stringify(now) !== JSON.stringify([...base].sort()); };
  const upd = () => { save.disabled = !dirty(); };
  function render() {
    ui.clear(form); boxes.clear();
    form.append(el('div.form-error', { role: 'alert' }));
    const known = new Set(AREAS.flatMap((a) => a[1]));
    const groups = [...AREAS, ['Other', types.map((t) => t.code).filter((c) => !known.has(c))]];
    for (const [name, codes] of groups) {
      const items = codes.map((c) => types.find((t) => t.code === c)).filter(Boolean);
      if (!items.length) continue;
      form.append(el('fieldset.field', {}, el('legend.label', { text: name }), el('div.checks', {}, items.map((t) => {
        const b = el('input', { type: 'checkbox', name: 'sub', value: t.code, checked: base.includes(t.code), on: { change: upd } }); boxes.set(t.code, b);
        return el('label.check', {}, b, el('span', { text: t.label }));
      }))));
    }
    form.append(save); upd();
  }
  const act = api.action();
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault();
    const subscribed = [...boxes].filter(([, b]) => b.checked).map(([c]) => c);
    await api.busy(save, async () => {
      try { await act.put('/api/notifications/subscriptions', { subscribed }); base = subscribed; toast.ok('Notification choices saved'); upd(); }
      catch (e) { ui.formError(form, e); }
    });
  });
  async function refresh() {
    if (loaded && dirty()) return;   // keep the user's unsaved ticks
    try {
      const [t, s] = await Promise.all([api.get('/api/notifications/types'), api.get('/api/notifications/subscriptions')]);
      types = t; base = s.subscribed || []; loaded = true; render();
    } catch (e) { if (!e.redirecting) ui.mount(card, el('div.card-head', {}, el('h2', {}, icon('bell'), 'Notifications')), ui.errorState(e, refresh)); }
  }
  ui.mount(card, el('div.card-head', {}, el('h2', {}, icon('bell'), 'What should alert me?')), el('p.muted.small', { text: 'Choose which events appear in your bell and are pushed to your phone. You only receive alerts for pages you can open.' }), form);
  refresh();
  card.refresh = refresh;
  return card;
}

/* ------------------------------------------------------------------ web push */
function pushCard() {
  const card = el('section.card'); const body = el('div.stack');
  ui.mount(card, el('div.card-head', {}, el('h2', {}, icon('phone'), 'Phone notifications (Web Push)')), body);
  async function paint() {
    ui.mount(body, ui.skeleton(2));
    const st = await pwa.pushStatus();
    const msg = (tone, ic, title, text) => el('div.banner.inline', { class: tone }, icon(ic), el('div.grow', {}, el('b', { text: title }), text ? el('div', { style: { fontWeight: 500 }, text }) : null));
    const parts = [];
    if (!st.supported) {
      parts.push(msg('warn', 'info', 'This browser cannot receive push notifications', pwa.isIOS() ? 'On iPhone/iPad: tap Share, choose "Add to Home Screen", open the app from the home screen, then come back here.' : 'Use Chrome on Android, or install the app to the home screen.'));
    } else if (!st.serverEnabled) {
      parts.push(msg('info', 'info', 'Push is not set up on the server yet', 'Ask an administrator to configure Web Push keys. You still get alerts in the bell inside the app.'));
    } else if (st.permission === 'denied') {
      parts.push(msg('err', 'ban', 'Notifications are blocked for this site', 'Open the browser menu > Site settings > Notifications and allow them, then reload this page.'));
    } else if (st.subscribed) {
      parts.push(msg('ok', 'check-circle', 'Push is ON for this device', 'Alerts you chose above arrive even when the app is closed.'));
      const off = el('button.btn.danger.outline', { type: 'button' }, icon('ban'), 'Turn off on this device');
      const test = el('button.btn.secondary', { type: 'button' }, icon('bell'), 'Send a test notification');
      off.addEventListener('click', () => api.busy(off, async () => { try { await pwa.pushDisable(); toast.ok('Push turned off on this device'); paint(); } catch (e) { ui.toastError(e); } }));
      test.addEventListener('click', () => api.busy(test, async () => { try { await pwa.pushTest(); toast.ok('Test sent - it should arrive in a few seconds'); } catch (e) { ui.toastError(e); } }));
      parts.push(el('div.btn-row', {}, test, off));
    } else {
      parts.push(msg('info', 'bell', 'Push is OFF on this device', 'Turn it on to hear about low stock, QC decisions and overdue jars even when the app is closed.'));
      const on = el('button.btn.primary', { type: 'button' }, icon('bell'), 'Turn on notifications');
      on.addEventListener('click', () => api.busy(on, async () => { try { await pwa.pushEnable(st.publicKey); toast.ok('Push turned on'); } catch (e) { if (e.code === 'PERMISSION') toast.warn(e.message); else ui.toastError(e); } paint(); }));
      parts.push(on);
    }
    ui.mount(body, parts);
  }
  paint();
  return card;
}

/* ------------------------------------------------------------------ install */
function installCard() {
  if (pwa.isStandalone()) return null;
  const card = el('section.card'); const body = el('div.stack.tight');
  const paint = () => {
    ui.mount(body, el('p.muted.small', { text: 'Add Neersagar ERP to the home screen for a full-screen app, faster start and push notifications.' }),
      pwa.canInstall() ? el('button.btn.primary', { type: 'button', on: { click: async () => { if (await pwa.promptInstall()) toast.ok('Installed'); paint(); } } }, icon('install'), 'Install app')
        : el('p.small', {}, icon('info', 'sm'), ' ', pwa.isIOS() ? 'Tap the Share button, then "Add to Home Screen".' : 'Open the browser menu and choose "Add to Home screen" / "Install app".'));
  };
  window.addEventListener('erp:installable', paint); window.addEventListener('erp:installed', () => card.remove());
  ui.mount(card, el('div.card-head', {}, el('h2', {}, icon('install'), 'Install the app')), body); paint();
  return card;
}

const subs = notificationsCard();
ui.mount(main, el('div.stack', {}, accountCard(), godownCard(), subs, pushCard(), passwordCard(), installCard(),
  el('button.btn.secondary.block', { type: 'button', on: { click: () => ui.confirmDialog({ title: 'Sign out?', message: 'Unsent drafts stay on this device.', confirmText: 'Sign out' }).then((y) => y && logout()) } }, icon('logout'), 'Sign out')));

ERP.live.start('profile', { users: () => subs.refresh(), settings: () => subs.refresh() });
