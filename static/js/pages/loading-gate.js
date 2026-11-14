// loading-gate.js - printable gate pass (A4 or 80 mm thermal) + shared printable-document frame used by dispatch notes.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { el, icon, cratesNode, kv } from '/js/pages/loading-common.js';

/** Print the document element. thermal=true => 80 mm roll (named @page in loading.css). */
export function printDoc(docEl, { thermal = false, title } = {}) {
  const oldTitle = document.title;
  docEl.classList.toggle('print-thermal', thermal);
  document.body.classList.toggle('print-thermal-mode', thermal);
  if (title) document.title = title;
  const done = () => { docEl.classList.remove('print-thermal'); document.body.classList.remove('print-thermal-mode'); document.title = oldTitle; window.removeEventListener('afterprint', done); };
  window.addEventListener('afterprint', done);
  window.print();
}

/** Signature boxes, printed at the bottom of a document. */
export const signatures = (names) => el('div.sign-row', {}, names.map((n) => el('div.sign', {}, el('div.sign-line'), el('div.sign-name', { text: n }))));

/**
 * docFrame({docEl-children..., printTitle}) -> {root, doc}  (actions above the sheet; sheet = what is printed)
 */
export function docFrame({ header, body, actions, banner, printTitle }) {
  const doc = el('article.card.gp-doc', {}, header, body);
  const a4 = el('button.btn.primary', { type: 'button', on: { click: () => printDoc(doc, { title: printTitle }) } }, el('span.btn-icon', {}, icon('print')), 'Print (A4)');
  const th = el('button.btn.secondary', { type: 'button', on: { click: () => printDoc(doc, { thermal: true, title: printTitle }) } }, icon('print'), 'Thermal slip (80 mm)');
  const bar = el('div.no-print.gp-actions', {}, el('div.btn-row', {}, actions || null, a4, th));
  return { root: el('div.stack', {}, bar, banner || null, doc), doc };
}

export async function mountGatePass(host, { id, ctx }) {
  const back = el('button.btn.ghost.sm.back.no-print', { type: 'button', on: { click: () => ctx.go(`trip=${id}`) } }, icon('chevron-left', 'sm'), 'Back to trip');
  host.append(back);
  let g;
  try { g = await api.get(`/api/loading/trips/${encodeURIComponent(id)}/gate-pass`); }
  catch (e) {
    if (e.code === 'INVALID_STATE') { ui.mount(host, back, ui.empty({ icon: 'ban', title: 'No gate pass for this trip', text: 'A gate pass exists only after the trip has been dispatched (and not voided).', action: { label: 'Back to trip', onClick: () => ctx.go(`trip=${id}`) } })); return {}; }
    throw e;
  }
  const justIssued = ctx.justIssued === id; ctx.justIssued = null;
  const items = g.items || [];
  const cols = [
    { label: 'Product', cls: 'k', render: (r) => [el('b', { text: r.product_name }), el('small', { text: r.set_label ? `Label ${r.set_label}` : '' })] },
    { label: 'Batch', render: (r) => r.batch_no },
    { label: 'Expiry', render: (r) => ui.fmtDate(r.expiry_date) },
    { label: 'Quantity', cls: 'num w', render: (r) => cratesNode(r.crates, r.crates_per_pallet) },
    { label: 'Bottles', cls: 'num', render: (r) => ui.fmtNum(r.crates * r.units_per_crate, 0) },
  ];
  const header = ui.printHeader({ title: 'GATE PASS', docNo: g.gate_pass_no, subtitle: g.location_name, date: ui.fmtDateTime(g.issued_at) });
  const t = g.totals || {};
  const body = el('div.stack.gp-body', {},
    el('div.gp-title', {}, el('h2', { text: 'Gate pass' }), el('div.mono.gp-no', { text: g.gate_pass_no || '' }), el('div.small.muted', { text: `${g.plant_name || ''}${g.location_name ? ' - ' + g.location_name : ''}` })),
    kv([['Trip', g.trip_no], ['Issued', `${ui.fmtDateTime(g.issued_at)}${g.issued_by_name ? ' by ' + g.issued_by_name : ''}`], ['Vehicle', `${g.vehicle.reg_no}${g.vehicle.name ? ' (' + g.vehicle.name + ')' : ''}`],
      ['Driver', `${g.driver_name || g.vehicle.driver_name || '-'}${g.vehicle.driver_phone ? ' - ' + g.vehicle.driver_phone : ''}`], ['Route', g.route], ['Status', ui.badge(g.status, g.status === 'DISPATCHED' ? 'On the road' : undefined)]]),
    ui.cardsTable({ columns: cols, rows: items, caption: 'Items on this gate pass' }),
    el('div.gp-totals', {}, el('div', {}, el('span.muted', { text: 'Total crates' }), el('b.big', { text: ui.fmtNum(t.crates, 0) })),
      el('div', {}, el('span.muted', { text: 'Pallets + loose' }), el('b.big', { text: `${ui.fmtNum(t.pallets, 0)} + ${ui.fmtNum(t.loose_crates, 0)}` })),
      el('div', {}, el('span.muted', { text: 'Bottles' }), el('b.big', { text: ui.fmtNum(t.bottles, 0) }))),
    g.notes ? el('p.small', {}, el('b', { text: 'Notes: ' }), g.notes) : null,
    signatures(['Loaded by (store)', 'Checked by (gate security)', 'Driver']));
  const actions = g.status === 'DISPATCHED' ? el('button.btn.secondary', { type: 'button', on: { click: () => ctx.go(`settle=${id}`) } }, icon('check-circle'), 'Settle trip') : null;
  const banner = justIssued ? el('div.banner.ok.inline.no-print', {}, icon('check-circle'), el('div.grow', {}, el('b', { text: `Gate pass ${g.gate_pass_no} issued` }), el('div.small', { style: { fontWeight: 500 }, text: 'Stock has left. Print it now and give it to the driver.' }))) : null;
  const { root } = docFrame({ header, body, actions, banner, printTitle: g.gate_pass_no || 'Gate pass' });
  ui.mount(host, back, root);
  return {};
}
