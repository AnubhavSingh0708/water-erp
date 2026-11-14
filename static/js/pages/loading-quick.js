// loading-quick.js - "Quick add by product": one row per product with a crates/pallets stepper and "Add (oldest first)".
// The server (POST /api/loading/trips/{id}/items {product_id, crates}) allocates FEFO across APPROVED batches; the dry run shows
// which batches will be used before anything is reserved. Per-batch loading stays available below (loading-trips.js).
import * as ui from '/js/ui.js';
import { el, icon, crates, cratesNode, run, dryRunReview, actionFor } from '/js/pages/loading-common.js';

/** Split `n` crates over batches in the order given (already FEFO) -> [{batch, take}] (client-side preview only). */
export function planFefo(batches, n) {
  const plan = []; let left = n;
  for (const b of batches) { if (left <= 0) break; const take = Math.min(left, b.available); if (take > 0) { plan.push({ batch: b, take }); left -= take; } }
  return { plan, short: left };
}

/**
 * createQuickAdd({tripId, getTrip, getAvail, onAdded}) -> {el, update(), hasFocus()}
 * getTrip() -> trip object (capacity_crates, loaded_crates, location_id); getAvail() -> availability rows (FEFO ordered).
 * onAdded(response) is called after a successful add (parent reloads availability and re-renders).
 */
export function createQuickAdd({ tripId, getTrip, getAvail, onAdded }) {
  const rows = new Map();                       // product_id -> row refs (kept across updates so typed values survive live refreshes)
  const list = el('div.av-list');
  const empty = el('p.muted.small.qa-none', { hidden: true }, icon('info', 'sm'), ' No QC-approved stock is free to add by product right now.');
  const root = el('section.card.pad0.qa-card', { hidden: true },
    el('div.card-head', {}, el('h2', {}, icon('list'), 'Quick add by product')),
    el('div.av-wrap', {}, el('p.small.muted.qa-intro', { text: 'Type how many crates or pallets. The oldest batches are used first, and you see which ones before anything is reserved.' }), list, empty));
  const room = () => { const t = getTrip(); return Math.max(0, t.capacity_crates - t.loaded_crates); };

  function products() {
    const map = new Map();
    for (const r of getAvail()) {
      if (r.qc_status !== 'APPROVED' || !(r.available > 0)) continue;       // the product-level add only ever allocates APPROVED stock
      if (!map.has(r.product_id)) map.set(r.product_id, { id: r.product_id, name: r.product_name, cpp: r.crates_per_pallet || 0, batches: [], free: 0 });
      const p = map.get(r.product_id); p.batches.push(r); p.free += r.available;
    }
    return [...map.values()];
  }

  function build(p) {
    const act = actionFor(`trip:${tripId}:quick:${p.id}`);
    const ref = { p, cpp: p.cpp };
    const name = el('b', { text: p.name });
    const free = el('div.small.muted');
    const q = ui.qtyInput({ name: `quick-${p.id}`, label: 'How many crates?', cratesPerPallet: p.cpp, onInput: () => check() });
    const plan = el('div.small.qa-plan', { 'aria-live': 'polite' });
    const msg = el('div.ld-check', { role: 'status', 'aria-live': 'polite' });
    const allBtn = el('button.chip-btn', { type: 'button', on: { click: () => { q.set(String(ref.max)); q.input.dispatchEvent(new Event('input', { bubbles: true })); } } });
    const btn = el('button.btn.primary.block', { type: 'button', disabled: true, on: { click: submit } }, el('span.btn-icon', {}, icon('plus', 'sm')), 'Add (oldest first)');
    const root = el('article.av-row.qa-row', { dataset: { product: p.id }, role: 'group', 'aria-label': `Quick add ${p.name}` },
      el('div.av-top', {}, el('div.grow', {}, name, free)),
      q.el, el('div.chips.quick', {}, allBtn), plan, msg, btn);
    Object.assign(ref, { root, q, free, plan, msg, btn, allBtn, name });

    function check() {
      const n = q.value; const r = ref;
      ui.clear(plan); ui.clear(msg); btn.disabled = true;
      const lbl = btn.lastChild;
      if (!n || !q.valid) { lbl.textContent = 'Add (oldest first)'; return; }
      lbl.textContent = `Add ${ui.fmtNum(n, 0)} ${n === 1 ? 'crate' : 'crates'} (oldest first)`;
      const issues = [];
      if (n > r.p.free) issues.push(`Only ${ui.fmtNum(r.p.free, 0)} crates are free - enter at most ${ui.fmtNum(r.p.free, 0)}.`);
      else if (n > room()) issues.push(`Over the vehicle capacity by ${ui.fmtNum(n - room(), 0)} crates - you can add at most ${ui.fmtNum(room(), 0)} more.`);
      if (issues.length) { ui.mount(msg, issues.map((t) => el('div.banner.err.inline', {}, icon('ban'), el('div.grow', { text: t })))); return; }
      const { plan: pl } = planFefo(r.p.batches, n);
      ui.mount(plan, el('span.muted', { text: 'Will use: ' }), pl.map((x, i) => [i ? ' + ' : '', el('b', {}, `${ui.fmtNum(x.take, 0)} from `, el('span.nowrap', { text: x.batch.batch_no })), el('span.muted', { text: ` (exp ${ui.fmtDate(x.batch.expiry_date)})` })]));
      const after = getTrip().loaded_crates + n; const pct = Math.round((after * 100) / getTrip().capacity_crates);
      ui.mount(msg, el('div.small', { class: pct >= 95 ? 'warn-text' : 'muted', text: `After adding: ${ui.fmtNum(after, 0)} of ${ui.fmtNum(getTrip().capacity_crates, 0)} crates (${pct}%).${pct >= 95 ? ' The vehicle will be almost full.' : ''}` }));
      btn.disabled = false;
    }
    ref.check = check;

    async function submit() {
      const n = q.value; if (!n || !q.valid) return;
      const body = { product_id: p.id, crates: n };
      const url = `/api/loading/trips/${encodeURIComponent(tripId)}/items`;
      const res = await run(btn, async () => {
        const mapped = (e) => {
          const m = /reserves\s+([\d,]+)\s+crates/i.exec(e.note || ''); const f = /free before:\s*([\d,]+),\s*after:\s*([\d,]+)/i.exec(e.note || '');
          return m ? { ...e, zone: '', right: `${m[1]} crates`, note: f ? `Free now ${f[1]}, afterwards ${f[2]}` : '' } : e;
        };
        const rev = await dryRunReview({
          path: url, body,
          review: { title: 'Add oldest stock first?', intro: `${crates(n, ref.p.cpp)} of ${ref.p.name} will be reserved for this trip from these batches (oldest first):`, confirmText: 'Add to trip', unit: 'crates', mapEffects: mapped, emptyText: 'No batches were returned - nothing will be reserved.' },
        });
        if (!rev.ok) return null;
        return act.postConfirm(url, rev.body);
      }, { refresh: () => onAdded(null) });
      if (res) {
        const used = planFefo(ref.p.batches, n).plan.map((x) => `${ui.fmtNum(x.take, 0)} from ${x.batch.batch_no}`).join(', ');
        (res.warnings || []).forEach((w) => ui.toast.warn(w));
        ui.toast.ok(`Added ${crates(n, ref.p.cpp)} - ${ref.p.name}`, { sub: used ? `${used}.` : undefined });
        q.set('');
        await onAdded(res);
      }
    }
    return ref;
  }

  function update() {
    const t = getTrip();
    root.hidden = !t || t.status !== 'LOADING';
    if (root.hidden) return;
    const ps = products();
    const ids = new Set(ps.map((p) => p.id));
    for (const [id, ref] of rows) if (!ids.has(id)) { ref.root.remove(); rows.delete(id); }
    ps.forEach((p, i) => {
      let ref = rows.get(p.id);
      if (!ref) { ref = build(p); rows.set(p.id, ref); }
      ref.p = p;
      const max = Math.min(p.free, room()); ref.max = max;
      if (ref.q.setMax) ref.q.setMax(max);   // (older cached ui.js without setMax: the check() below still blocks over-limit entries)
      ui.mount(ref.free, 'Free to add: ', el('b', {}, cratesNode(p.free, p.cpp)), p.batches.length > 1 ? ` in ${p.batches.length} batches` : '');
      ref.allBtn.textContent = max < p.free ? `Fill vehicle (${ui.fmtNum(max, 0)})` : `All free (${ui.fmtNum(max, 0)})`;
      ref.allBtn.hidden = max <= 0;
      if (list.children[i] !== ref.root) list.insertBefore(ref.root, list.children[i] || null);
      ref.check();
    });
    empty.hidden = ps.length > 0;
  }
  return { el: root, update, hasFocus: () => root.contains(document.activeElement) };
}
