// admin-users.js - Users tab (CRUD via crud.js + unlock / reset password / revoke sessions) and Sessions tab.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { createCrud } from '/js/crud.js';
import { policy, passwordRules, wirePasswordToggle } from '/js/pwcheck.js';

const { el, icon, toast } = ui;
const A = '/api/admin';

function resetPasswordDialog(user) {
  const pw = el('input.input', { type: 'password', name: 'new_password', autocomplete: 'new-password', maxlength: 200, 'aria-label': 'New password', 'data-nodraft': '1' });
  const toggle = el('button.addon', { type: 'button', 'aria-label': 'Show password', 'aria-pressed': 'false' }, icon('eye'));
  wirePasswordToggle(toggle, [pw]);
  const meter = el('div.pw-meter', { 'data-score': '0', 'aria-hidden': 'true' }, el('i'), el('i'), el('i'), el('i'));
  const rules = el('ul.rules');
  const paint = () => passwordRules(rules, meter, pw.value, user.username); pw.addEventListener('input', paint); paint();
  const form = el('form.stack', { novalidate: true, id: 'pwform', autocomplete: 'off' }, el('div.form-error', { role: 'alert' }),
    el('p', {}, 'Set a new password for ', el('b', { text: user.full_name || user.username }), '. Their other sessions are signed out.'),
    el('div.field', {}, el('label', { for: 'np', text: 'New password' }), el('div.input-group', {}, Object.assign(pw, { id: 'np' }), toggle), meter, rules, el('p.error')));
  const save = el('button.btn.primary', { type: 'submit', form: 'pwform' }, el('span.btn-icon', {}, icon('key')), 'Set password');
  const cancel = el('button.btn.secondary', { type: 'button' }, 'Cancel');
  const dlg = ui.openDialog({ title: 'Reset password', body: form, actions: [cancel, save] });
  cancel.addEventListener('click', () => dlg.close());
  const act = api.action();
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    if (!policy(pw.value, user.username).ok) { ui.formError(form, { status: 422, fields: { new_password: 'At least 10 characters and not the same as the username' }, message: '' }); return; }
    if (!(await ui.confirmDialog({ title: 'Reset this password?', message: `${user.username} will have to sign in again with the new password.`, confirmText: 'Reset password', danger: true }))) return;
    await api.busy(save, async () => {
      try { await act.post(`${A}/users/${user.id}/password`, { new_password: pw.value }); toast.ok('Password reset'); dlg.close(true); }
      catch (e) { ui.formError(form, e); }
    });
  });
}

export function mountUsers(host) {
  const crud = createCrud(host, {
    entity: 'users', title: 'Users', singular: 'user', endpoint: `${A}/users`, icon: 'users', wide: true, noDelete: false, deactivateLabel: 'Deactivate',
    deactivateMessage: (r) => `${r.username} will no longer be able to sign in and all their sessions end. History and audit records stay.`,
    columns: [
      { label: 'User', cls: 'k', render: (r) => [el('b', { text: r.full_name }), el('small', { text: '@' + r.username })] },
      { label: 'Access', render: (r) => (r.is_admin ? ui.badge('INFO', 'Administrator', 'info') : `${(r.allowed_pages || []).length} pages`) },
      { label: 'Godowns', hideMobile: true, render: (r) => ((r.allowed_location_ids || []).length ? `${r.allowed_location_ids.length} allowed` : 'All') },
      { label: 'Last sign-in', render: (r) => (r.last_login_at ? ui.fmtRelative(r.last_login_at) : 'never') },
      { label: 'State', render: (r) => [r.is_locked ? ui.badge('LOCKED', 'Locked out') : null, r.active_sessions ? el('span.small.muted', { text: `${r.active_sessions} session${r.active_sessions === 1 ? '' : 's'}` }) : null] },
    ],
    fields: [
      { name: 'username', label: 'Username', type: 'text', required: true, readOnlyOnEdit: true, maxlength: 40, help: '3-40 characters: letters, digits, . - _ (cannot be changed later).' },
      { name: 'full_name', label: 'Full name', type: 'text', required: true, maxlength: 120 },
      { name: 'password', label: 'Initial password', type: 'password', createOnly: true, required: true, maxlength: 200, help: 'At least 10 characters. Share it privately; the user can change it in their profile.', full: true },
      { name: 'is_admin', label: 'Administrator (all pages and the admin console)', type: 'bool', full: true },
      { name: 'allowed_pages', label: 'Pages this user may open', type: 'checks', optionsFrom: { url: `${A}/pages`, value: 'key', label: 'label', filter: (p) => p.key !== 'admin' }, showWhen: (v) => !v.is_admin, help: 'The dashboard is included automatically for most roles - tick what is needed.' },
      { name: 'allowed_location_ids', label: 'Godown restriction', type: 'checks', optionsFrom: { url: `${A}/locations?active=true`, value: 'id', label: (r) => r.name, sub: (r) => r.code }, help: 'Leave all unticked to allow every godown. Tick some to limit this user to them.', emptyText: 'No godowns defined yet.' },
      { name: 'subscribed_notifications', label: 'Notifications they receive by default', type: 'checks', optionsFrom: { url: `${A}/notification-types`, value: 'code', label: 'label' } },
    ],
    rowActions: [
      { label: 'Unlock', icon: 'unlock', show: (r) => r.is_locked, onClick: async (r, c, btn) => api.busy(btn, async () => { try { await api.post(`${A}/users/${r.id}/unlock`, {}); toast.ok('Account unlocked'); c.refresh(); } catch (e) { ui.toastError(e); } }) },
      { label: 'Reset password', icon: 'key', onClick: (r) => resetPasswordDialog(r) },
      { label: 'End sessions', icon: 'logout', show: (r) => r.active_sessions > 0, onClick: async (r, c, btn) => {
        if (!(await ui.confirmDialog({ title: `Sign ${r.username} out?`, message: `This ends ${r.active_sessions} active session(s). They keep their drafts and can sign in again.`, confirmText: 'Sign out', danger: true }))) return;
        await api.busy(btn, async () => { try { const x = await api.post(`${A}/users/${r.id}/revoke-sessions`, {}); toast.ok(`${(x && x.revoked) ?? 0} session(s) ended`); c.refresh(); } catch (e) { ui.toastError(e); } });
      } },
    ],
  });
  crud.refresh();
  return { refresh: () => crud.refresh() };
}

/* --------------------------------------------------------------------------------------------- sessions */
function device(ua) {
  if (!ua) return 'Unknown device';
  const br = /Edg\//.test(ua) ? 'Edge' : /OPR\//.test(ua) ? 'Opera' : /Firefox\//.test(ua) ? 'Firefox' : /Chrome\//.test(ua) ? 'Chrome' : /Safari\//.test(ua) ? 'Safari' : 'Browser';
  const os = /Android/.test(ua) ? 'Android' : /iPhone|iPad/.test(ua) ? 'iOS' : /Windows/.test(ua) ? 'Windows' : /Mac OS/.test(ua) ? 'macOS' : /Linux/.test(ua) ? 'Linux' : '';
  return os ? `${br} on ${os}` : br;
}

export function mountSessions(host) {
  let rows = []; let q = '';
  const list = el('div');
  const search = el('input.input', { type: 'search', placeholder: 'Filter by user...', 'aria-label': 'Filter sessions by user', autocomplete: 'off' });
  const revokeAll = el('button.btn.danger.outline', { type: 'button' }, icon('logout'), 'Sign out everyone else');
  host.append(el('div.toolbar', {}, el('div.searchbar', {}, icon('search', 'sm'), search), el('div.toolbar-end', {}, revokeAll)), list);
  search.addEventListener('input', ui.debounce(() => { q = search.value.trim().toLowerCase(); paint(); }, 200));
  revokeAll.addEventListener('click', async () => {
    if (!(await ui.confirmDialog({ title: 'Sign out everyone else?', message: 'Every other session on every device ends. People keep their drafts and can sign in again. Your own session stays.', confirmText: 'Sign everyone out', danger: true }))) return;
    await api.busy(revokeAll, async () => { try { const r = await api.post(`${A}/sessions/revoke-all`, {}); toast.ok(`${(r && r.revoked) ?? 0} session(s) ended`); refresh(); } catch (e) { ui.toastError(e); } });
  });
  function paint(err) {
    if (err) { ui.mount(list, ui.errorState(err, refresh)); return; }
    const f = rows.filter((r) => !q || r.username.toLowerCase().includes(q));
    ui.mount(list, ui.cardsTable({
      columns: [
        { label: 'User', cls: 'k', render: (r) => [el('b', { text: r.username }), el('small', { text: device(r.user_agent) })] },
        { label: 'Status', cls: 's', render: (r) => (r.is_current ? ui.badge('OK', 'This device') : ui.badge('ACTIVE')) },
        { label: 'IP address', render: (r) => r.ip || '-' }, { label: 'Last active', render: (r) => ui.fmtRelative(r.last_used_at) },
        { label: 'Signed in', render: (r) => ui.fmtDateTime(r.created_at), hideMobile: true }, { label: 'Expires', render: (r) => ui.fmtDateTime(r.expires_at), hideMobile: true },
        { label: '', cls: 'a', render: (r) => el('div.btn-row', {}, el('button.btn.danger.outline.sm', { type: 'button', on: { click: (ev) => revoke(r, ev.currentTarget) } }, icon('logout', 'sm'), r.is_current ? 'Sign out here' : 'Revoke')) },
      ], rows: f, emptyState: { icon: 'lock', title: 'No active sessions', text: q ? 'No session matches this filter.' : '' },
    }));
  }
  async function revoke(r, btn) {
    const ok = await ui.confirmDialog({ title: r.is_current ? 'Sign out of this device?' : `Revoke ${r.username}'s session?`, message: r.is_current ? 'You will be taken to the sign-in page.' : 'That device is signed out immediately.', confirmText: r.is_current ? 'Sign out' : 'Revoke', danger: true });
    if (!ok) return;
    await api.busy(btn, async () => { try { await api.del(`${A}/sessions/${r.id}`); toast.ok('Session revoked'); refresh(); } catch (e) { ui.toastError(e); } });
  }
  let loaded = false;
  async function refresh() {
    if (!loaded) ui.mount(list, ui.skeletonCards(3));
    try { rows = await api.get(`${A}/sessions`); loaded = true; paint(); } catch (e) { if (!e.redirecting) paint(e); }
  }
  refresh();
  return { refresh };
}
