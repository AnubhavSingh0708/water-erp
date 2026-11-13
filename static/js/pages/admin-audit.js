// admin-audit.js - Audit tab: filters + paging + details drawer.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';

const { el, icon } = ui;
const LIMIT = 50;

export function mountAudit(host) {
  let offset = 0; let data = { items: [], total: 0 }; let loaded = false;
  const f = { user: '', action: '', entity: '', from: '', to: '' };
  const inputs = {};
  const fld = (name, label, type = 'text', ph = '') => {
    const x = ui.field({ label, name, type, placeholder: ph, maxlength: 100, autocomplete: 'off' }); inputs[name] = x.input; return x.el;
  };
  const form = el('form.card.tight', { novalidate: true, role: 'search', 'aria-label': 'Audit filters' },
    el('div.form-grid', {}, fld('user', 'User', 'text', 'username'), fld('action', 'Action contains', 'text', 'e.g. USER_UPDATE'), fld('entity', 'Entity', 'text', 'e.g. user, product'),
      el('div.grid.cols-2', {}, fld('from', 'From', 'date'), fld('to', 'To', 'date'))),
    el('div.btn-row.end', { style: { marginTop: '12px' } }, el('button.btn.ghost', { type: 'button', on: { click: reset } }, 'Clear'), el('button.btn.primary', { type: 'submit' }, icon('search'), 'Apply filters')));
  const list = el('div', { style: { marginTop: '12px' }, 'aria-live': 'polite' });
  host.append(form, list);
  form.addEventListener('submit', (e) => { e.preventDefault(); Object.keys(f).forEach((k) => (f[k] = inputs[k].value.trim())); offset = 0; refresh(); });
  function reset() { Object.keys(f).forEach((k) => { f[k] = ''; inputs[k].value = ''; }); offset = 0; refresh(); }

  function paint(err) {
    if (err) { ui.mount(list, ui.errorState(err, refresh)); return; }
    ui.mount(list,
      ui.cardsTable({
        columns: [
          { label: 'Action', cls: 'k', render: (r) => [el('b.mono', { text: r.action }), el('small', { text: `${r.entity}${r.entity_id ? ' ' + String(r.entity_id).slice(0, 8) : ''}` })] },
          { label: 'Who', render: (r) => r.username || 'system' }, { label: 'When', render: (r) => ui.fmtDateTime(r.at) },
          { label: 'IP', render: (r) => r.ip || '-', hideMobile: true },
          { label: '', cls: 'a', render: (r) => el('button.btn.secondary.sm', { type: 'button', on: { click: () => details(r) } }, icon('list', 'sm'), 'Details') },
        ], rows: data.items, emptyState: { icon: 'history', title: 'No audit entries', text: 'Nothing matches these filters.' },
      }),
      data.total > LIMIT || offset ? ui.pager({ total: data.total, limit: LIMIT, offset, onPage: (o) => { offset = o; refresh(true); window.scrollTo({ top: 0, behavior: 'smooth' }); } }) : el('p.muted.small', { text: `${data.total} entr${data.total === 1 ? 'y' : 'ies'}` }));
  }
  function details(r) {
    const pretty = JSON.stringify(r.details ?? {}, null, 2);
    ui.openDialog({
      title: r.action, wide: true,
      body: el('div.stack', {}, el('dl.kv', {}, el('dt', { text: 'When' }), el('dd', { text: ui.fmtDateTime(r.at) }), el('dt', { text: 'Who' }), el('dd', { text: r.username || 'system' }),
        el('dt', { text: 'Entity' }), el('dd', { text: `${r.entity}${r.entity_id ? ' / ' + r.entity_id : ''}` }), el('dt', { text: 'IP' }), el('dd', { text: r.ip || '-' })),
        el('pre.json', { tabindex: '0', text: pretty })),
      actions: [el('button.btn.secondary', { type: 'button', on: { click: () => ui.copyText(pretty) } }, icon('copy'), 'Copy JSON')],
    });
  }
  async function refresh(skel) {
    if (!loaded || skel) ui.mount(list, ui.skeletonCards(4));
    try { data = await api.get('/api/admin/audit', { ...f, limit: LIMIT, offset }); loaded = true; paint(); } catch (e) { if (!e.redirecting) paint(e); }
  }
  refresh();
  return { refresh: () => refresh() };
}
