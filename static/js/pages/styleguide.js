// styleguide.html - every component in one place (visual review + copy-paste reference for page authors).
import * as ui from '/js/ui.js';
import { api } from '/js/api.js';

const { el, icon } = ui;
const main = document.getElementById('main');
const sec = (title, ...body) => el('section.card', {}, el('div.card-head', {}, el('h2', { text: title })), el('div.stack', {}, ...body));
const swatch = (name, v, fg = '#fff') => el('div', { style: { background: `var(${v})`, color: fg, padding: '10px', borderRadius: '8px', fontSize: '.8rem', fontWeight: 700 } }, name);

const icons = ['dashboard', 'box', 'factory', 'flask', 'warehouse', 'truck', 'jar', 'wrench', 'shield', 'qr', 'pin', 'transfer', 'bell', 'user', 'users', 'logout', 'check-circle', 'x-circle', 'alert', 'info', 'plus', 'edit', 'trash', 'search', 'refresh', 'eye', 'wifi-off', 'lock', 'key', 'download', 'print', 'filter', 'calendar', 'droplet', 'clock', 'copy', 'list', 'history', 'package', 'ban', 'save', 'neersagar'];

const tableRows = [
  { name: 'PLT1-B-261113-0007', sub: '500 ml, 100 crates', status: 'QUARANTINE', qty: 100, exp: '2027-05-12' },
  { name: 'PLT1-B-261113-0006', sub: '1 L, 60 crates', status: 'APPROVED', qty: 60, exp: '2027-05-10' },
  { name: 'PLT1-B-261112-0003', sub: '20 L jar, 30 crates', status: 'REJECTED', qty: 30, exp: '2027-05-01' },
];

main.append(
  sec('Colour', el('div.grid.auto', {}, swatch('brand-800', '--brand-800'), swatch('brand-600', '--brand-600'), swatch('brand-500', '--brand-500'), swatch('brand-100', '--brand-100', '#0d1d28'), swatch('ink', '--ink'), swatch('ink-3 (muted)', '--ink-3'), swatch('ok', '--ok'), swatch('warn', '--warn'), swatch('bad', '--bad')),
    el('p.small.muted', { text: 'Status is never colour alone: every badge has an icon and a text label. Contrast >= 4.5:1.' })),
  sec('Type & numbers', el('h1', { text: 'Heading 1 - Neersagar' }), el('h2', { text: 'Heading 2' }), el('h3', { text: 'Heading 3' }), el('p', { text: 'Body text is at least 16 px. Numbers are tabular: 1,234.50 crates / 0.0001 / 99,999.' }), el('p.small.muted', { text: 'Small muted helper text.' })),
  sec('Icons', el('div.row', { style: { gap: '14px' } }, icons.map((n) => el('span', { title: n, style: { display: 'inline-flex', flexDirection: 'column', alignItems: 'center', gap: '2px', width: '64px', fontSize: '.62rem', color: 'var(--ink-3)' } }, icon(n, 'lg'), n)))),
  sec('Buttons', el('div.btn-row', {}, el('button.btn.primary', { type: 'button' }, icon('save'), 'Primary'), el('button.btn.secondary', { type: 'button' }, 'Secondary'), el('button.btn.danger', { type: 'button' }, icon('trash'), 'Danger'), el('button.btn.danger.outline', { type: 'button' }, 'Danger outline'), el('button.btn.ghost', { type: 'button' }, 'Ghost'), el('button.btn.primary', { type: 'button', disabled: true }, 'Disabled'),
    (() => { const b = el('button.btn.primary', { type: 'button' }, el('span.btn-icon', {}, icon('check')), 'Click: busy 2 s'); b.addEventListener('click', () => api.busy(b, ui.sleep(2000))); return b; })()),
    el('button.btn.primary.block', { type: 'button' }, 'Full width 48 px primary')),
  sec('Badges', el('div.row', {}, ['APPROVED', 'FG_READY', 'OK', 'QUARANTINE', 'LOW', 'DUE_SOON', 'REJECTED', 'CRITICAL', 'OVERDUE', 'OUT', 'DISPATCHED', 'DRAFT', 'INACTIVE'].map((s) => ui.badge(s)))),
  sec('Form controls', (() => {
    const form = el('form.form-grid', { novalidate: true });
    const a = ui.field({ label: 'Text field', name: 'a', placeholder: 'Type here', help: 'Helper text explains the field.', required: true });
    const b = ui.field({ label: 'With unit', name: 'b', type: 'decimal', unit: 'kg', value: '12.5' });
    const c = ui.field({ label: 'Select', name: 'c', type: 'select', options: [{ value: '1', label: 'First' }, { value: '2', label: 'Second' }], value: '2' });
    const d = ui.field({ label: 'Field with error', name: 'd', value: 'oops' }); ui.setFieldError(d.el, 'This is what an inline error looks like.');
    const q = ui.qtyInput({ name: 'q', label: 'Quantity (crates)', cratesPerPallet: 60, value: '130', max: 5000, help: 'Stepper + live pallets echo + typo check.' });
    const sw = el('label.switch', {}, el('input', { type: 'checkbox', checked: true }), el('span.track'), el('span', { text: 'Switch' }));
    const ck = el('div.checks.cols', {}, el('label.check', {}, el('input', { type: 'checkbox', checked: true }), el('span', {}, 'Checkbox', el('span.sub', { text: 'with sub text' }))), el('label.check', {}, el('input', { type: 'checkbox' }), 'Another'));
    form.append(a.el, b.el, c.el, d.el, el('div.full', {}, q.el), sw, el('div.full', {}, ck));
    return form;
  })()),
  sec('Segmented, tabs, collapsible', ui.segmented({ options: [{ value: 'a', label: 'Active' }, { value: 'b', label: 'Inactive' }, { value: 'c', label: 'All' }], value: 'a' }).el,
    ui.tabs({ tabs: [{ id: 'x', label: 'Users', icon: 'users' }, { id: 'y', label: 'Sessions', icon: 'phone' }, { id: 'z', label: 'Audit', icon: 'history' }], active: 'x' }).el,
    ui.collapsible('Collapsible section', el('p', { text: 'Hidden content appears here.' }), { icon: 'filter' })),
  sec('Banners', el('div.banner.err.inline', {}, icon('wifi-off'), el('div.grow', {}, el('b', { text: 'Server unreachable - nothing is lost, your entries are kept' }))), el('div.banner.warn.inline', {}, icon('refresh'), el('div.grow', {}, el('b', { text: 'Server restarting...' }))), el('div.banner.info.inline', {}, icon('history'), el('div.grow', {}, el('b', { text: 'Draft restored' }))), el('div.banner.ok.inline', {}, icon('check-circle'), el('div.grow', {}, el('b', { text: 'Saved' })))),
  sec('Progress (capacity)', el('div', {}, el('div.progress-label', {}, el('span', { text: 'Truck 1' }), el('span', { text: '120 / 400' })), ui.progress(30)), el('div', {}, el('div.progress-label', {}, el('span', { text: 'Truck 2' }), el('span', { text: '380 / 400' })), ui.progress(95)), el('div', {}, ui.progress(100))),
  sec('Table -> cards (resize below 768 px)', ui.cardsTable({
    columns: [{ label: 'Batch', cls: 'k', render: (r) => [el('b', { text: r.name }), el('small', { text: r.sub })] }, { label: 'QC', cls: 's', render: (r) => ui.badge(r.status) }, { label: 'Crates', cls: 'num', render: (r) => ui.fmtNum(r.qty, 0) }, { label: 'Expiry', render: (r) => ui.fmtDate(r.exp) },
      { label: '', cls: 'a', render: () => el('div.btn-row', {}, el('button.btn.secondary.sm', { type: 'button' }, 'Open'), el('button.btn.secondary.sm', { type: 'button' }, 'Print')) }],
    rows: tableRows,
  })),
  sec('Empty & skeleton', ui.empty({ icon: 'box', title: 'No purchase orders yet', text: 'Create your first order when stock runs low.', action: { label: 'New order', icon: 'plus', onClick: () => ui.toast('Clicked') } }), ui.skeletonCards(1)),
  sec('Toasts & dialogs', el('div.btn-row', {},
    el('button.btn.secondary', { type: 'button', on: { click: () => ui.toast.ok('Saved', { sub: 'GRN-261113-0001' }) } }, 'Toast ok'),
    el('button.btn.secondary', { type: 'button', on: { click: () => ui.toast.err('Could not save', { sub: 'Check Wi-Fi. Ref: 1234' }) } }, 'Toast error'),
    el('button.btn.secondary', { type: 'button', on: { click: () => ui.confirmDialog({ title: 'Deactivate user?', message: 'They can no longer sign in.', warnings: ['Stock is more than 3x usual.'], danger: true, confirmText: 'Deactivate' }) } }, 'Confirm dialog'),
    el('button.btn.secondary', { type: 'button', on: { click: () => ui.reviewSheet({ title: 'Review before saving', effects: [{ item: '500 ml', zone: 'FG_READY', delta: -40, after: 60 }, { item: 'Cap 28 mm', zone: 'RM_STORE', delta: 1200, after: 3400 }], warnings: ['Batch is within 10 % of expiry.'] }) } }, 'Review sheet'),
    el('button.btn.secondary', { type: 'button', on: { click: () => ui.promptDialog({ title: 'Void gate pass', label: 'Reason', min: 5, confirmText: 'Void', danger: true }) } }, 'Prompt'))),
  sec('Pallets + crates', el('p', { text: `130 crates at 60/pallet = ${ui.fmtCrates(130, 60)};  1 crate = ${ui.fmtCrates(1, 60)};  60 = ${ui.fmtCrates(60, 60)}` })),
);
