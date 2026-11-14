// loading-settle.js - settle a dispatched trip: per item delivered / returned / damaged (must add up to what was loaded),
// returnable-container customer lines (delivered, empties collected, damaged, lost), dry-run review, confirm-aware commit.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { ERP } from '/js/shell.js';
import { el, icon, cratesNode, crates, run, explain, dryRunReview, actionFor } from '/js/pages/loading-common.js';

const int = (v) => { const n = ui.toNum(v); return Number.isFinite(n) && n >= 0 && Number.isInteger(n) ? n : null; };

export async function mountSettle(host, { id, ctx }) {
  const [trip, customers] = await Promise.all([api.get(`/api/loading/trips/${encodeURIComponent(id)}`), api.get('/api/ref/customers').catch(() => [])]);
  const back = el('button.btn.ghost.sm.back', { type: 'button', on: { click: () => ctx.go(`trip=${id}`) } }, icon('chevron-left', 'sm'), 'Back to trip');
  if (trip.status !== 'DISPATCHED') {
    ui.mount(host, back, ui.empty({ icon: 'info', title: trip.status === 'SETTLED' ? 'This trip is already settled' : `This trip is ${trip.status}`, text: 'Only a trip that is on the road can be settled.', action: { label: 'Open trip', onClick: () => ctx.go(`trip=${id}`) } }));
    return {};
  }
  const items = trip.items || [];
  const warnPct = Number(ui.config.settings.damage_warn_pct) || 0;
  const rows = items.map((it, i) => ({ it, i, auto: true }));
  const returnable = [...new Map(items.filter((it) => it.kind === 'RETURNABLE').map((it) => [it.product_id, it.product_name])).entries()];
  const lines = [];   // {product_id, customer_id, delivered, empties_collected, damaged, lost}

  const form = el('form.stack.loose', { id: 'settle-form', novalidate: true }, el('div.form-error', { role: 'alert' }));
  const jarsData = el('textarea', { name: 'jarlines', hidden: true });
  form.append(jarsData);
  const sumBox = el('div.settle-sum', { 'aria-live': 'polite' });
  const rowsBox = el('div.stack');
  const jarsBox = el('div.stack');
  form.append(sumBox, rowsBox, jarsBox);

  /* ------------------------------------------------------------ item rows */
  const num = (name, label, val, onIn) => {
    const f = ui.field({ label, name, type: 'int', value: val, inputmode: 'numeric', autocomplete: 'off' });
    f.input.addEventListener('input', onIn); f.input.addEventListener('focus', () => f.input.select());
    return f;
  };
  rows.forEach((r) => {
    const it = r.it; const loaded = it.crates;
    r.d = num(`items[${r.i}].delivered`, 'Delivered', String(loaded), () => { r.auto = false; update(); });
    r.r = num(`items[${r.i}].returned`, 'Brought back', '0', () => { autoD(r); update(); });
    r.x = num(`items[${r.i}].damaged`, 'Damaged', '0', () => { autoD(r); update(); });
    r.check = el('div.settle-check', { role: 'status' });
    r.el = el('section.card.settle-item', { dataset: { item: it.id } },
      el('div', {}, el('b', { text: it.product_name }), el('div.small.muted', { text: `${it.batch_no} - exp ${ui.fmtDate(it.expiry_date)}` }),
        el('div.settle-loaded', {}, 'Loaded: ', el('b', {}, cratesNode(loaded, it.crates_per_pallet)))),
      el('div.settle-grid', {}, r.d.el, r.r.el, r.x.el), r.check);
  });
  function autoD(r) { if (!r.auto) return; const v = r.it.crates - (int(r.r.get()) || 0) - (int(r.x.get()) || 0); r.d.set(String(Math.max(0, v))); }
  const vals = (r) => ({ d: int(r.d.get()), r: int(r.r.get()), x: int(r.x.get()) });
  const balanced = (r) => { const v = vals(r); return v.d !== null && v.r !== null && v.x !== null && v.d + v.r + v.x === r.it.crates; };

  /* ------------------------------------------------------------ jar lines */
  const jarState = () => returnable.map(([pid, name]) => {
    const dl = rows.filter((r) => r.it.product_id === pid).reduce((s, r) => s + (vals(r).d || 0), 0);
    const mine = lines.filter((l) => l.product_id === pid);
    const sumD = mine.reduce((s, l) => s + (int(l.f.d.get()) || 0), 0);
    return { pid, name, delivered: dl, lines: mine, sumD };
  });
  function addLine(pid, pre = {}) {
    const l = { product_id: pid };
    const sel = ui.field({ label: 'Customer', name: `jars[${lines.length}].customer_id`, type: 'select', value: pre.customer_id || '', options: [{ value: '', label: 'Choose customer...' }, ...customers.map((c) => ({ value: c.id, label: `${c.name}${c.code ? ' (' + c.code + ')' : ''}` }))] });
    const n = lines.length;
    l.f = { c: sel, d: num(`jars[${n}].delivered`, 'Delivered', String(pre.delivered ?? ''), changed), e: num(`jars[${n}].empties_collected`, 'Empties collected', String(pre.empties_collected ?? '0'), changed), x: num(`jars[${n}].damaged`, 'Damaged', String(pre.damaged ?? '0'), changed), l: num(`jars[${n}].lost`, 'Lost', String(pre.lost ?? '0'), changed) };
    sel.input.addEventListener('change', changed);
    const rm = el('button.btn.ghost.sm.icon', { type: 'button', 'aria-label': 'Remove this customer line', on: { click: () => { lines.splice(lines.indexOf(l), 1); renderJars(); changed(); } } }, icon('trash', 'sm'));
    l.el = el('div.jar-line', {}, el('div.row.between', { style: { flexWrap: 'nowrap' } }, el('div.grow', {}, sel.el), rm), el('div.jar-grid', {}, l.f.d.el, l.f.e.el, l.f.x.el, l.f.l.el));
    lines.push(l); return l;
  }
  function renderJars() {
    ui.clear(jarsBox);
    jarState().forEach((s) => {
      const addBtn = el('button.btn.secondary.sm', { type: 'button', on: { click: () => { const rem = Math.max(0, s.delivered - s.sumD); const l = addLine(s.pid, { delivered: rem }); renderJars(); changed(); l.f.c.input.focus(); } } }, icon('plus', 'sm'), 'Add customer');
      jarsBox.append(el('section.card.jar-card', { dataset: { product: s.pid } }, el('div.card-head', {}, el('h2', {}, icon('jar'), `Containers at customers - ${s.name}`)),
        el('p.small.muted', { text: 'Record who received the filled containers and what they gave back. The total delivered to customers must equal what you marked as delivered above.' }),
        el('div.jar-total', { 'data-total': s.pid }), ...s.lines.map((l) => l.el), addBtn));
    });
    paintJarTotals();
  }
  function paintJarTotals() {
    jarState().forEach((s) => {
      const box = jarsBox.querySelector(`[data-total="${s.pid}"]`); if (!box) return;
      const ok = s.sumD === s.delivered;
      const e = s.lines.reduce((a, l) => a + (int(l.f.e.get()) || 0), 0), x = s.lines.reduce((a, l) => a + (int(l.f.x.get()) || 0), 0), lo = s.lines.reduce((a, l) => a + (int(l.f.l.get()) || 0), 0);
      ui.mount(box, el('div.chk', { class: ok ? 'ok' : 'bad' }, icon(ok ? 'check-circle' : 'alert', 'sm'), ok ? ` Delivered to customers: ${s.sumD} of ${s.delivered} - matches` : ` Delivered to customers: ${s.sumD} of ${s.delivered} - ${s.sumD < s.delivered ? `${s.delivered - s.sumD} not assigned to a customer yet` : `${s.sumD - s.delivered} too many`}`),
        el('div.small.muted', { text: `Empties collected ${e} - damaged ${x} - lost ${lo}` }));
    });
  }
  function persistJars() {
    jarsData.value = JSON.stringify(lines.map((l) => ({ p: l.product_id, c: l.f.c.get(), d: l.f.d.get(), e: l.f.e.get(), x: l.f.x.get(), l: l.f.l.get() })));
    jarsData.dispatchEvent(new Event('input', { bubbles: true }));
  }
  function changed() { persistJars(); update(); }

  /* ------------------------------------------------------------ live arithmetic + summary */
  let submitBtn = null; const noteEl = el('span.note');
  function update() {
    let allOk = true; let tl = 0, td = 0, tr = 0, tx = 0;
    rows.forEach((r) => {
      const v = vals(r); const loaded = r.it.crates; tl += loaded;
      const bad = v.d === null || v.r === null || v.x === null;
      const sum = bad ? NaN : v.d + v.r + v.x; const ok = !bad && sum === loaded; if (!ok) allOk = false;
      td += v.d || 0; tr += v.r || 0; tx += v.x || 0;
      const cpp = r.it.crates_per_pallet;
      ui.mount(r.check, ok ? el('div.chk.ok', {}, icon('check-circle', 'sm'), ` ${v.d} delivered + ${v.r} back + ${v.x} damaged = ${sum} - matches the ${loaded} loaded`)
        : bad ? el('div.chk.bad', {}, icon('alert', 'sm'), ' Enter whole numbers (0 or more) in all three boxes')
          : el('div.chk.bad', {}, icon('alert', 'sm'), ` ${v.d} + ${v.r} + ${v.x} = ${sum}, but ${loaded} were loaded - ${sum < loaded ? `${loaded - sum} crates not accounted for` : `${sum - loaded} too many`}`),
      cpp && ok && (v.r || v.x) ? el('div.small.muted', { text: [v.r ? `Back: ${crates(v.r, cpp)}` : '', v.x ? `Damaged: ${crates(v.x, cpp)}` : ''].filter(Boolean).join(' - ') }) : null);
      r.el.classList.toggle('bad', !ok);
    });
    const pct = tl ? Math.round((tx * 100) / tl) : 0;
    ui.mount(sumBox, el('div.ld-stats', {}, [['Loaded', tl, ''], ['Delivered', td, 'ok'], ['Brought back', tr, tr ? 'warn' : ''], ['Damaged', tx, tx ? 'bad' : '']].map(([l, v, t]) => el('div.ld-stat', { class: t }, el('b', { text: ui.fmtNum(v, 0) }), el('span', { text: l })))),
      warnPct && tx && pct > warnPct ? el('div.banner.warn.inline', {}, icon('alert'), el('div.grow', { text: `Damaged is ${pct}% of the load (limit ${warnPct}%). You will be asked to confirm.` })) : null);
    paintJarTotals();
    const jarsOk = jarState().every((s) => s.sumD === s.delivered) && lines.every((l) => l.f.c.get());
    if (submitBtn) { submitBtn.disabled = !(allOk && jarsOk); noteEl.textContent = allOk ? (jarsOk ? 'Everything adds up. Review and settle.' : 'Assign every delivered container to a customer.') : 'Each item must add up before you can settle.'; }
  }

  /* ------------------------------------------------------------ build + draft */
  rows.forEach((r) => rowsBox.append(r.el));
  host.append(back, el('div.page-head', {}, el('div', {}, el('p.sub', { text: `${trip.trip_no} - ${trip.vehicle_reg}${trip.route_name ? ' - ' + trip.route_name : ''}` }), el('p.small.muted', { text: 'Count what reached customers, what came back and what broke. Each line must add up to what was loaded.' }))), form);
  const draft = ERP.drafts.bind(form, { page: 'loading', id: `settle-${id}` });
  try { (JSON.parse(jarsData.value || '[]') || []).forEach((x) => { if (returnable.some(([p]) => p === x.p)) addLine(x.p, { customer_id: x.c, delivered: x.d, empties_collected: x.e, damaged: x.x, lost: x.l }); }); } catch { /* ignore a bad draft */ }
  // draft restore only touched existing controls (items); recompute auto flags for restored delivered values
  rows.forEach((r) => { if (int(r.d.get()) !== r.it.crates - (int(r.r.get()) || 0) - (int(r.x.get()) || 0)) r.auto = false; });
  renderJars();

  submitBtn = el('button.btn.primary', { type: 'submit', form: 'settle-form' }, el('span.btn-icon', {}, icon('check-circle')), 'Review and settle');
  host.append(el('div.action-bar', {}, noteEl, submitBtn));
  update();

  const act = actionFor(`settle:${id}`);
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault(); ui.clearFormError(form);
    const body = { items: rows.map((r) => { const v = vals(r); return { item_id: r.it.id, delivered: v.d, returned: v.r, damaged: v.x }; }) };
    if (lines.length) body.customer_jars = lines.map((l) => ({ customer_id: l.f.c.get(), product_id: l.product_id, delivered: int(l.f.d.get()) || 0, empties_collected: int(l.f.e.get()) || 0, damaged: int(l.f.x.get()) || 0, lost: int(l.f.l.get()) || 0 }));
    const tl = items.reduce((s, i) => s + i.crates, 0); const tx = body.items.reduce((s, i) => s + i.damaged, 0); const tr = body.items.reduce((s, i) => s + i.returned, 0); const td = body.items.reduce((s, i) => s + i.delivered, 0);
    const before = el('div.stack.tight', {}, el('div.ld-stats', {}, [['Loaded', tl, ''], ['Delivered', td, 'ok'], ['Back', tr, tr ? 'warn' : ''], ['Damaged', tx, tx ? 'bad' : '']].map(([l, v, t]) => el('div.ld-stat', { class: t }, el('b', { text: ui.fmtNum(v, 0) }), el('span', { text: l })))),
      tr ? el('p.small', { text: `${ui.fmtNum(tr, 0)} crates go back into ready stock.` }) : null, tx ? el('p.small', { text: `${ui.fmtNum(tx, 0)} damaged crates go to the scrap yard.` }) : null);
    const r = await run(submitBtn, async () => {
      const rev = await dryRunReview({ path: `/api/loading/trips/${encodeURIComponent(id)}/settle`, body, review: { title: 'Settle trip?', before, confirmText: 'Settle trip', intro: `${trip.trip_no} - ${trip.vehicle_reg}. This closes the trip.`, confirmTitle: 'Check the damaged count' } });
      if (!rev.ok) return null;
      return act.postConfirm(`/api/loading/trips/${encodeURIComponent(id)}/settle`, rev.body);
    }, { form, refresh: () => ctx.go(`trip=${id}`) });
    if (r) { draft.clear(); ui.toast.ok(`Trip ${r.trip_no} settled`, { sub: r.settlement ? `Delivered ${r.settlement.delivered}, back ${r.settlement.returned}, damaged ${r.settlement.damaged}` : '' }); ctx.go(`trip=${id}`); }
  });
  void explain;
  return {};
}
