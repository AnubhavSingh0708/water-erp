// loading-common.js - helpers shared by the floor pages (loading, jars, maintenance, qr-labels):
// pallet+crate text, review dialog (dry run), dry-run -> review -> commit flow (CONFIRM_REQUIRED aware),
// guided error explanations, busy/"Not sent - Retry" button handling, safe SVG import, small layout bits.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';

export const { el, icon } = ui;

/* ------------------------------------------------------------------ crates: pallets + loose, everywhere */
/** "2 pallets + 10 crates" (falls back to "130 crates" without a pallet size). */
export const crates = (n, cpp) => ui.fmtCrates(n, cpp);
/** Node: pallets+loose text plus the plain total in small grey when it adds information ("= 130"). */
export function cratesNode(n, cpp, { total = true } = {}) {
  const v = Math.round(ui.toNum(n));
  const showTotal = total && Number.isFinite(v) && ui.toNum(cpp) > 0 && v >= ui.toNum(cpp);
  return el('span.crates', {}, crates(n, cpp), showTotal ? el('small.muted', { text: ` (${ui.fmtNum(v, 0)})` }) : null);
}

export const ZONE_LABEL = {
  FG_READY: 'Ready stock', QC_QUARANTINE: 'QC quarantine', SCRAP_YARD: 'Scrap yard', JAR_EMPTY: 'Empty containers', JAR_CLEAN: 'Clean containers',
  CUSTOMER: 'At customer', RM_STORE: 'Raw store', BLOW_FLOOR: 'Blow floor',
};

/* ------------------------------------------------------------------ dates */
export const daysBetween = (fromYmd, toYmd) => Math.round((Date.parse(toYmd + 'T00:00:00Z') - Date.parse(fromYmd + 'T00:00:00Z')) / 86400000);
export function dueText(daysLeft) {
  const d = Math.round(ui.toNum(daysLeft));
  if (!Number.isFinite(d)) return '';
  if (d < 0) return `${-d} day${d === -1 ? '' : 's'} overdue`;
  if (d === 0) return 'due today';
  return `in ${d} day${d === 1 ? '' : 's'}`;
}

/* ------------------------------------------------------------------ safe SVG (server QR codes only) */
export function svgNode(markup) {
  try {
    const doc = new DOMParser().parseFromString(String(markup || ''), 'image/svg+xml');
    const root = doc.documentElement;
    if (!root || root.nodeName.toLowerCase() !== 'svg' || doc.querySelector('parsererror')) return null;
    root.querySelectorAll('script, foreignObject, iframe, object, embed').forEach((n) => n.remove());
    const walk = (n) => { for (const a of [...(n.attributes || [])]) if (/^on/i.test(a.name) || /javascript:/i.test(a.value)) n.removeAttribute(a.name); [...n.children].forEach(walk); };
    walk(root);
    return document.importNode(root, true);
  } catch { return null; }
}

/* ------------------------------------------------------------------ review dialog (dry run result) */
/**
 * reviewDialog({title, intro, before:Node, effects, warnings, confirmText, cancelText, danger, unit, cpp})
 * Shows what will happen: one row per stock effect (friendly zone name, signed crates), warnings, optional extra nodes.
 * Resolves true only on the explicit confirm button.
 */
export function reviewDialog({ title = 'Review before saving', intro, before, after, effects = [], warnings = [], confirmText = 'Confirm', cancelText = 'Go back and edit', danger = false, unit = 'crates', emptyText = 'No stock changes.' } = {}) {
  const rows = effects.filter((e) => e && (ui.toNum(e.delta) !== 0 || e.note)).map((e) => {
    const d = ui.toNum(e.delta);
    const zone = ZONE_LABEL[e.zone] || e.zone || '';
    const sub = [zone, e.after !== undefined && e.after !== null && d !== 0 ? `then ${ui.fmtNum(e.after, 0)} left` : '', e.note].filter(Boolean).join(' - ');
    return el('div.effect', {}, el('div.grow', {}, el('b', { text: e.item_name || e.item || '' }), el('small', { text: sub })),
      d !== 0 ? el('span.delta', { class: d < 0 ? 'neg' : 'pos' }, `${d > 0 ? '+' : '-'}${ui.fmtNum(Math.abs(d), 0)} ${unit}`) : null);
  });
  const body = el('div.stack', {},
    intro ? el('p', { text: intro }) : null, before || null,
    rows.length ? el('div.effects', {}, rows) : el('p.muted', { text: emptyText }),
    warnings && warnings.length ? el('div.banner.warn.inline', {}, icon('alert'), el('ul.warn-list', {}, warnings.map((w) => el('li', { text: String(w) })))) : null,
    after || null);
  const yes = el('button.btn', { type: 'button', class: danger ? 'danger' : 'primary', on: { click: () => dlg.close(true) } }, confirmText);
  const no = el('button.btn.secondary', { type: 'button', on: { click: () => dlg.close(false) } }, cancelText);
  const dlg = ui.openDialog({ title, body, actions: [no, yes] });
  yes.focus();
  return dlg.closed.then((r) => r === true);
}

/**
 * dryRunReview({path, body, review:{...reviewDialog opts}}) -> {ok:boolean, body:finalBody, dry}
 * dry_run -> (CONFIRM_REQUIRED dialog -> resend with confirm:true) -> review dialog. Throws API errors (QC_LOCKED, ...).
 */
export async function dryRunReview({ path, body, review = {} }) {
  let final = body;
  const dry = await api.withConfirm((b) => { final = b; return api.dryRun(path, b); }, body, {
    confirmTitle: review.confirmTitle || 'Please check before continuing', confirmText: 'Yes, continue',
  });
  const warnings = [...(review.warnings || []), ...((dry && dry.warnings) || [])];
  const ok = await reviewDialog({ ...review, effects: (dry && dry.effects) || [], warnings });
  return { ok, body: final, dry };
}

/* ------------------------------------------------------------------ guided errors */
const DOC_NAME = { INSURANCE: 'Insurance', FITNESS: 'Fitness certificate', PUC: 'Pollution (PUC)', PERMIT: 'Permit' };
function notice({ title, text, list, tone = 'warn', actions = [], ic = 'alert' }) {
  const body = el('div.stack.tight', {}, el('div.banner.inline', { class: tone }, icon(ic), el('div.grow', {}, el('b', { text: title }), text ? el('div.small', { style: { fontWeight: 500 }, text }) : null)),
    list && list.length ? el('ul.warn-list.plain', {}, list.map((x) => el('li', { text: x }))) : null);
  const ok = el('button.btn.primary', { type: 'button', on: { click: () => dlg.close(true) } }, 'OK, got it');
  const dlg = ui.openDialog({ title: 'Cannot continue', body, actions: [...actions.map((a) => el('button.btn.secondary', { type: 'button', on: { click: () => { dlg.close(true); a.onClick(); } } }, a.label)), ok] });
  ok.focus();
  return dlg.closed;
}

/**
 * explain(err, {refresh, openTrip, form}) - turns API errors into guidance. Returns true when handled.
 * 422 with `form` -> inline field errors; network -> calm "nothing was sent" toast; everything else -> dialog / toast.
 */
export function explain(e, ctx = {}) {
  if (!e || e.redirecting || e.cancelled || e.aborted) return true;
  const d = e.details || {};
  const n = (v) => ui.fmtNum(v, 0);
  if (e.network) { ui.toast.warn('Not sent - the server cannot be reached.', { sub: 'Nothing was saved. Your entries are kept - tap Retry when the connection is back.', ms: 7000 }); return true; }
  switch (e.code) {
    case 'QC_LOCKED':
      notice({ title: `${d.batch_no ? 'Batch ' + d.batch_no : 'This batch'} is not approved`, text: `Lab QC status: ${d.qc_status || 'not approved'}. Only QC-approved stock can leave the plant.`, list: ['Pick another batch that shows APPROVED.', 'Or ask the QC lab to decide this batch first.'] });
      break;
    case 'RESERVED_STOCK':
      notice({ title: 'That stock is reserved for a loading trip', text: `${n(d.reserved)} crates are reserved, so they cannot be taken out of ready stock.`, list: (d.trips || []).map((t) => `${t.trip_no}: ${n(t.crates)} crates`).concat(['Remove the crates from that trip first, then try again.']) });
      break;
    case 'CAPACITY_EXCEEDED':
      notice({ title: 'The vehicle is too full', text: `It holds ${n(d.capacity)} crates. ${n(d.loaded)} are loaded and you are adding ${n(d.adding)} - that is ${n(d.over_by)} over.`, list: [`You can add at most ${n(Math.max(0, ui.toNum(d.capacity) - ui.toNum(d.loaded)))} more crates.`, 'Or use a bigger vehicle / a second trip.'] });
      break;
    case 'INSUFFICIENT_STOCK': {
      const avail = d.available;
      notice({ title: 'Not enough stock', text: e.message || '', list: avail !== undefined && avail !== null ? [`Available right now: ${n(avail)}${d.required !== undefined ? ` (you asked for ${n(d.required)})` : ''}.`, 'The screen was updated - enter a smaller number.'] : ['The screen was updated - check the numbers and try again.'] });
      if (ctx.refresh) ctx.refresh();
      break;
    }
    case 'VEHICLE_DOCS_EXPIRED':
      notice({ title: `${d.vehicle || 'This vehicle'} has expired documents`, text: 'The gate pass is blocked until the paperwork is valid.',
        list: (d.expired || []).map((x) => `${DOC_NAME[x.doc] || x.doc} expired on ${ui.fmtDate(x.expiry_date)}`).concat(['Ask the manager to renew it (Maintenance page) or use another vehicle.']) });
      break;
    case 'INVALID_STATE':
      notice({ title: 'This was changed by someone else', text: e.message || 'The record is no longer in the state this screen showed.', list: ['The screen has been refreshed. Check the new state and try again if still needed.'] });
      if (ctx.refresh) ctx.refresh();
      break;
    case 'DUPLICATE':
      if (d.trip_id && ctx.openTrip) notice({ title: 'This vehicle already has an open trip', text: e.message || '', actions: [{ label: `Open ${d.trip_no || 'that trip'}`, onClick: () => ctx.openTrip(d.trip_id) }] });
      else if (ctx.form) return ui.formError(ctx.form, e);
      else ui.toastError(e);
      break;
    case 'FEATURE_DISABLED':
      notice({ title: 'This feature is switched off', text: e.message || '', list: ['An administrator can change it in Settings -> Features.'], tone: 'info', ic: 'info' });
      break;
    default:
      if (ctx.form && ui.formError(ctx.form, e)) return true;
      ui.toastError(e);
  }
  return true;
}

/* ------------------------------------------------------------------ busy + retry buttons */
/**
 * run(btn, fn, ctx) - disables the button with a spinner while `fn` runs.
 * Network failure: label becomes "Not sent - Retry" (entries kept, same Idempotency-Key is reused by the caller's api.action()).
 * Returns fn's result, or undefined on error / cancel.
 */
export async function run(btn, fn, ctx = {}) {
  const label = btn ? btn.dataset.label || (btn.dataset.label = btn.lastChild ? btn.lastChild.textContent : '') : '';
  try {
    const r = await api.busy(btn, fn);
    if (btn && label) btn.lastChild.textContent = label;
    return r;
  } catch (e) {
    if (btn && label) btn.lastChild.textContent = e && e.network ? 'Not sent - Retry' : label;
    explain(e, ctx);
    return undefined;
  }
}

/* ------------------------------------------------------------------ small layout bits */
export function capacityBar(loaded, capacity, cpp) {
  const cap = Math.max(1, ui.toNum(capacity) || 1); const l = ui.toNum(loaded) || 0;
  const pct = Math.min(100, Math.round((l * 100) / cap));
  const tone = l >= cap ? 'bad' : pct >= 95 ? 'warn' : '';
  return el('div.capbar', {}, el('div.progress-label', {}, el('span', {}, el('b', { text: ui.fmtNum(l, 0) }), ` of ${ui.fmtNum(cap, 0)} crates`), el('span', { text: `${pct}%` })),
    ui.progress(pct, tone), pct >= 95 ? el('div.small.cap-warn', {}, icon('alert', 'sm'), l >= cap ? ' Vehicle is full' : ` Almost full - only ${ui.fmtNum(cap - l, 0)} crates of room left`) : null);
}
export function kv(rows) {
  return el('dl.kv', {}, rows.filter(Boolean).flatMap(([k, v]) => [el('dt', { text: k }), el('dd', {}, v === undefined || v === null || v === '' ? '-' : v)]));
}
export function section(title, ic, ...kids) {
  return el('section.card', {}, el('div.card-head', {}, el('h2', {}, ic ? icon(ic) : null, title)), ...kids);
}
/** Skeleton for list pages. */
export const skel = (n = 3) => ui.skeletonCards(n);

/** Sticky "changed by someone else" style note that auto-hides. */
export function chip(text, tone = 'warn') { return el('span.badge.plain', { class: tone }, icon('info'), text); }

/** Load a script once (classic script, same origin) - used for the vendored jsQR UMD build. */
const loaded = new Map();
export function loadScript(src) {
  if (!loaded.has(src)) loaded.set(src, new Promise((res, rej) => { const s = document.createElement('script'); s.src = src; s.onload = res; s.onerror = () => rej(new Error('Could not load ' + src)); document.head.append(s); }));
  return loaded.get(src);
}

/** Build a `<select>` options list from rows. */
export const opts = (rows, value = (r) => r.id, label = (r) => r.name, first) => [...(first ? [first] : []), ...rows.map((r) => ({ value: value(r), label: label(r) }))];

/** Debounced "live refresh while a dialog is open" guard: true when any modal dialog is open. */
export const modalOpen = () => !!document.querySelector('dialog[open]');
