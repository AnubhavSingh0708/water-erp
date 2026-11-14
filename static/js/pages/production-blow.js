// Blow molding tab: preforms in, empty bottles out, scrap % live, history.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import * as L from '/js/pages/warehouse-lib.js';

const { el, icon } = ui;
// `GET /api/production/preform-stock` may not exist on an older server: probe once per page load, then stay quiet.
let stockEndpoint = 'unknown';   // 'unknown' | 'yes' | 'no'

export function mountBlow(panel, ctx) {
  const histHost = el('div.compact'); let offset = 0;
  const holder = el('div.stack'); panel.append(holder);
  let products = [], materials = [], equipment = [];

  async function init() {
    ui.mount(holder, ui.skeletonCards(2));
    try { [products, materials, equipment] = await Promise.all([L.refProducts(), L.refMaterials(), L.refEquipment().catch(() => [])]); } catch (e) { ui.mount(holder, ui.errorState(e, init)); return; }
    build();
  }
  function build() {
    const matById = new Map(materials.map((m) => [m.id, m]));
    const blowable = products.filter((p) => p.kind === 'BOTTLED' && p.preform_material_id && p.bottle_material_id);
    const preforms = materials.filter((m) => m.category === 'PREFORM'); const bottles = materials.filter((m) => m.category === 'EMPTY_BOTTLE');
    if (!blowable.length) { ui.mount(holder, ui.empty({ icon: 'factory', title: 'No product is set up for blow molding', text: 'An administrator must link each bottled product to its preform and empty bottle under Administration > Master data > Products.' })); return; }
    const hour = Number((ui.fmtTime(new Date().toISOString()) || '12:00').split(':')[0]);
    const prod = ui.field({ label: 'Product being made', name: 'product', type: 'select', required: true, options: [{ value: '', label: 'Choose product...' }, ...blowable.map((p) => ({ value: p.id, label: p.name }))] });
    const preSel = ui.field({ label: 'Preform used', name: 'preform_material_id', type: 'select', options: preforms.map((m) => ({ value: m.id, label: m.name })), help: 'Filled from the product - change only if a different preform was used.' });
    const botSel = ui.field({ label: 'Empty bottle made', name: 'bottle_material_id', type: 'select', options: bottles.map((m) => ({ value: m.id, label: m.name })) });
    const shift = ui.field({ label: 'Shift', name: 'shift', type: 'select', options: L.optionsOf(L.SHIFTS), value: hour >= 19 || hour < 6 ? 'NIGHT' : 'DAY' });
    const eq = ui.field({ label: 'Machine', name: 'equipment_id', type: 'select', optional: true, options: [{ value: '', label: 'Not specified' }, ...equipment.map((e) => ({ value: e.id, label: e.name }))] });
    const date = ui.field({ label: 'Run date', name: 'run_date', type: 'date', value: ui.todayPlant() }); date.input.max = ui.todayPlant(); date.input.min = L.todayMinus(31);
    const issued = L.unitInput({ name: 'preforms_issued', label: 'Preforms issued to the machine', unit: 'pcs', inputmode: 'numeric', placeholder: 'e.g. 8000', required: true });
    const good = L.unitInput({ name: 'good_bottles', label: 'Good bottles produced', unit: 'pcs', inputmode: 'numeric', placeholder: 'e.g. 7900', required: true });
    const scrapBox = el('div.echo', { 'aria-live': 'polite' });
    const avail = el('div.avail', { 'aria-live': 'polite', hidden: true });
    const notes = ui.field({ label: 'Note', name: 'notes', type: 'textarea', optional: true, rows: 2, maxlength: 500 });
    const warnPct = Number(L.settings().scrap_warn_pct ?? 5);
    const form = el('form.stack', { id: 'blow-form', novalidate: true, autocomplete: 'off' }, el('div.form-error', { role: 'alert' }),
      el('div.card.stack', {}, el('p.section-sub', { text: 'A preform is the small test-tube-shaped PET piece. The blow molding machine heats and blows it into an empty bottle. Returnable 20 L jars are not blown - they are washed and refilled.' }),
        el('div.form-grid', {}, prod.el, shift.el, preSel.el, botSel.el, eq.el, date.el)),
      el('div.card.stack', {}, avail, el('div.form-grid', {}, issued.el, good.el), scrapBox, notes.el));
    const submit = L.btn('Review and save', 'check', { kind: 'primary', type: 'submit', form: 'blow-form' });
    const flow = L.makeFlow();

    /* ---- preform stock pre-check: what is really in the raw store of the production site ---- */
    let stock = null; let stockSeq = 0; let stockAt = 0;
    const issuedNow = () => L.dec(issued.get());
    function paintStock() {
      ui.clear(avail); avail.className = 'avail';
      if (!stock || stockEndpoint === 'no') { avail.hidden = true; return; }
      avail.hidden = false;
      if (stock.error) { avail.classList.add('muted'); avail.append(icon('wifi-off', 'sm'), el('span', { text: 'Could not check the preform stock right now - the server will still check it when you save.' })); return; }
      const have = ui.toNum(stock.available); const min = ui.toNum(stock.min_stock); const unit = stock.unit || 'pcs'; const i = issuedNow();
      const where = stock.location_name ? ` at ${stock.location_name}` : '';
      let tone = 'ok'; let icn = 'check-circle'; let msg = null;
      if (have <= 0) { tone = 'bad'; icn = 'alert'; msg = `There are no ${stock.name || 'preforms'} in stock${where}. Ask the store to receive some before you record a run.`; }
      else if (i !== null && i > have) { tone = 'bad'; icn = 'alert'; msg = `You entered ${ui.fmtNum(i, 0)} ${unit} but only ${ui.fmtNum(have, 0)} ${unit} are in stock${where}. Check the number, or ask the store to receive more preforms.`; }
      else if (min > 0 && have - (i || 0) <= min) { tone = 'warn'; icn = 'alert'; msg = i ? `Only ${ui.fmtNum(have - i, 0)} ${unit} will be left after this run - at or below the minimum of ${ui.fmtNum(min, 0)}. Tell the store to reorder.` : `Low stock - the minimum is ${ui.fmtNum(min, 0)} ${unit}. Tell the store to reorder.`; }
      avail.classList.add(tone);
      avail.append(icon(icn, 'sm'), el('div.grow', {}, el('b.block', { text: `Available: ${ui.fmtNum(have, 0)} ${unit}` }), el('span.small.block', { text: `${stock.name || ''}${where}` }), msg ? el('div.small.avail-msg', { text: msg }) : null));
    }
    async function checkStock({ force = false } = {}) {
      const site = ctx.site(); const pre = preSel.get();
      if (stockEndpoint === 'no' || !site || !pre || !prod.get()) { stock = null; paintStock(); return; }
      if (!force && stock && !stock.error && stock.material_id === pre && stock.location_id === site.id && Date.now() - stockAt < 20000) { paintStock(); return; }
      const my = ++stockSeq;
      try {
        const d = await api.get('/api/production/preform-stock', { preform_material_id: pre, location_id: site.id }, { retries: 0, timeout: 8000 });
        if (my !== stockSeq) return;
        stockEndpoint = 'yes'; stock = d; stockAt = Date.now();
      } catch (e) {
        if (my !== stockSeq || (e && (e.redirecting || e.aborted))) return;
        if (e && (e.status === 404 || e.status === 405)) stockEndpoint = 'no';
        stock = stockEndpoint === 'no' ? null : { error: true };
      }
      paintStock();
    }
    ctx.refreshers.blowStock = () => checkStock({ force: true });

    prod.input.addEventListener('change', () => {
      const p = blowable.find((x) => x.id === prod.get()); if (!p) return;
      preSel.set(p.preform_material_id); botSel.set(p.bottle_material_id); calc(); checkStock();
    });
    function calc() {
      ui.clear(scrapBox); scrapBox.className = 'echo';
      const i = L.dec(issued.get()), g = L.dec(good.get());
      issued.echo(issued.get() === '' ? '' : (L.qtyProblem(issued.get(), { whole: true }) || ''), 'bad'); good.echo(good.get() === '' ? '' : (L.qtyProblem(good.get(), { whole: true, allowZero: true }) || ''), 'bad');
      if (i === null || g === null || i <= 0) return;
      if (g > i) { scrapBox.classList.add('bad'); scrapBox.append(icon('alert', 'sm'), 'Good bottles cannot be more than the preforms issued.'); return; }
      const scrap = i - g; const pct = (scrap / i) * 100; const pre = matById.get(preSel.get());
      const kg = pre && pre.unit_weight_g ? (scrap * ui.toNum(pre.unit_weight_g)) / 1000 : null;
      scrapBox.classList.add(pct > warnPct ? 'warn' : 'ok');
      scrapBox.append(icon(pct > warnPct ? 'alert' : 'check-circle', 'sm'), `Scrap: ${ui.fmtNum(scrap, 0)} pcs = ${ui.fmtNum(pct, 2)} %${kg !== null ? ` (${ui.fmtNum(kg, 2)} kg)` : ''}`,
        ...(pct > warnPct ? [el('span', { text: ` - above the ${warnPct} % limit: please write a note below.` })] : []));
      notes.el.querySelector('label').firstChild.textContent = pct > warnPct ? 'Note (required for high scrap)' : 'Note';
    }
    [issued.input, good.input, preSel.input].forEach((x) => x.addEventListener('input', calc));
    preSel.input.addEventListener('change', () => { calc(); checkStock(); });
    issued.input.addEventListener('input', paintStock);
    const getState = () => ({ product: prod.get(), pre: preSel.get(), bot: botSel.get(), shift: shift.get(), eq: eq.get(), issued: issued.get(), good: good.get(), notes: notes.get() });
    const setState = (s) => { prod.set(s.product || ''); if (s.pre) preSel.set(s.pre); if (s.bot) botSel.set(s.bot); shift.set(s.shift || shift.get()); eq.set(s.eq || ''); issued.set(s.issued || ''); good.set(s.good || ''); notes.set(s.notes || ''); calc(); };
    const draft = L.stateDraft({ page: 'production', id: 'blow', form, get: getState, set: setState });
    draft.restore(); calc(); checkStock();

    form.addEventListener('submit', async (ev) => {
      ev.preventDefault(); ui.clearFormError(form);
      let bad = false;
      if (!prod.get()) { prod.setError('Choose the product'); bad = true; } else prod.setError('');
      const pi = L.qtyProblem(issued.get(), { whole: true }); issued.setError(pi); if (pi) bad = true;
      const pg = L.qtyProblem(good.get(), { whole: true, allowZero: true }); good.setError(pg); if (pg) bad = true;
      const i = L.dec(issued.get()), g = L.dec(good.get());
      if (!pi && !pg && g > i) { good.setError('More good bottles than preforms issued - check both numbers'); bad = true; }
      if (!pi && !pg && g <= i && ((i - g) / i) * 100 > warnPct && notes.get().trim() === '') { notes.setError(`Scrap is above ${warnPct} % - write what happened (e.g. machine jam, bad preform batch).`); bad = true; } else notes.setError('');
      if (!bad && stockEndpoint !== 'no') {            // fresh figure right before saving: someone may have used preforms meanwhile
        await checkStock({ force: true });
        if (stock && !stock.error && i > ui.toNum(stock.available)) {
          const unit = stock.unit || 'pcs'; const where = stock.location_name ? ` at ${stock.location_name}` : '';
          issued.setError(`Only ${ui.fmtNum(stock.available, 0)} ${unit} of ${stock.name || 'this preform'} are in stock${where}, but you entered ${ui.fmtNum(i, 0)}. Lower the number, or ask the store to receive more preforms first.`);
          bad = true;
        }
      }
      if (bad) { form.querySelector('.has-error input, .has-error select, .has-error textarea')?.focus(); return; }
      const site = ctx.site();
      const body = { shift: shift.get(), preform_material_id: preSel.get(), bottle_material_id: botSel.get(), preforms_issued: issued.get(), good_bottles: good.get(),
        ...(site ? { location_id: site.id } : {}), ...(date.get() ? { run_date: date.get() } : {}), ...(eq.get() ? { equipment_id: eq.get() } : {}), ...(notes.get().trim() ? { notes: notes.get().trim() } : {}) };
      const r = await L.submitDocument({ flow, form, button: submit, path: '/api/production/blow-runs', body, title: 'Review blow molding run', confirmText: 'Save run', sendConfirm: false,
        summary: 'Preforms leave the raw store, good bottles go to the blow floor and scrap is booked to the scrap yard.' });
      if (r) { draft.clear(); ui.toast.ok(`Saved ${r.run_no}`); (r.warnings || []).forEach((w) => ui.toast.warn(w)); setState({}); loadHist(); checkStock({ force: true }); ctx.refreshers.batches && ctx.refreshers.batches(); }
    });
    const csv = L.btn('CSV', 'download', { kind: 'secondary', sm: true, onClick: () => L.guarded(csv, () => api.download('/api/production/blow-runs', { query: { format: 'csv', ...L.readParam() } })) });
    ui.mount(holder, form, L.actionBar(submit), el('div.row.between', {}, el('h2', { text: 'Recent runs' }), csv), histHost);
    loadHist();
  }
  async function loadHist() {
    await L.refreshInto(histHost, async () => {
      const d = await api.get('/api/production/blow-runs', { limit: 10, offset });
      const warn = Number(L.settings().scrap_warn_pct ?? 5);
      return el('div.stack.tight', {}, ui.cardsTable({
        columns: [
          { label: 'Run', cls: 'k', render: (r) => [el('b.nowrap', { text: r.run_no }), el('small', { text: `${ui.fmtDate(r.run_date)} - ${r.shift.toLowerCase()}${r.equipment_name ? ` - ${r.equipment_name}` : ''}` })] },
          { label: 'Scrap', cls: 's', render: (r) => (ui.toNum(r.scrap_pct) > warn ? ui.badge('WARNING', `${ui.fmtNum(r.scrap_pct, 2)} %`) : ui.badge('OK', `${ui.fmtNum(r.scrap_pct, 2)} %`)) },
          { label: 'Preforms in', cls: 'num', render: (r) => `${ui.fmtNum(r.preforms_issued, 0)} ${r.preform_name ? '' : ''}pcs` }, { label: 'Good bottles', cls: 'num', render: (r) => `${ui.fmtNum(r.good_bottles, 0)} pcs` },
          { label: 'Scrap', cls: 'num', render: (r) => `${ui.fmtNum(r.scrap_pcs, 0)} pcs${r.scrap_weight_kg ? ` (${ui.fmtNum(r.scrap_weight_kg, 2)} kg)` : ''}` },
          { label: 'Preform', cls: 'w', hideMobile: true, render: (r) => `${r.preform_name} -> ${r.bottle_name}` }, { label: 'Note', cls: 'w', hideMobile: true, render: (r) => r.notes },
        ], rows: d.items, emptyState: { icon: 'factory', title: 'No blow runs yet', text: 'Record each machine run above so preform use and scrap stay visible.' },
      }), d.total > 10 ? ui.pager({ total: d.total, limit: 10, offset, onPage: (o) => { offset = o; loadHist(); } }) : null);
    });
  }
  ctx.refreshers.blow = loadHist;
  init();
}
