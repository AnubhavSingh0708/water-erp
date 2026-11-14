// loading-scan.js - QR mode (only when features.qr_labels is on): scan / type a crate-set label and add it to the open trip.
// Camera: BarcodeDetector when the browser has it, otherwise the vendored jsQR (classic script /js/jsQR.js). Torch + camera switch.
// Partial load (fewer crates than the label): the server splits the label; we offer to print the new residual label.
import { api } from '/js/api.js';
import * as ui from '/js/ui.js';
import { el, icon, cratesNode, crates, run, svgNode, loadScript } from '/js/pages/loading-common.js';

/** Pull the label code out of a scanned URL (".../loading.html?qr=CODE") or a typed/pasted code. */
export function extractCode(text) {
  const s = String(text || '').trim();
  try { const u = new URL(s); const c = u.searchParams.get('qr'); if (c) return c.trim().toUpperCase(); } catch { /* not a URL */ }
  const m = /[?&]qr=([0-9A-Za-z]+)/.exec(s); if (m) return m[1].toUpperCase();
  return s.replace(/\s+/g, '').toUpperCase();
}

/** Print one label (SVG QR + text) on its own: a temporary print root, everything else is hidden by loading.css (body.printing-label). */
export function printLabel(set, plant) {
  const svg = svgNode(set.svg);
  const root = el('div.label-print-root', {}, el('div.label', {}, svg || null, el('div.label-txt', {}, el('b.big', { text: set.set_label }), el('div', { text: set.product_name }), el('div', { text: `Batch ${set.batch_no}` }),
    el('div', { text: `${set.crates} crates - exp ${ui.fmtDate(set.expiry_date)}` }), el('div.mono', { text: set.code }), plant ? el('div.small', { text: plant }) : null)));
  document.body.append(root); document.body.classList.add('printing-label');
  const done = () => { root.remove(); document.body.classList.remove('printing-label'); window.removeEventListener('afterprint', done); };
  window.addEventListener('afterprint', done); window.print();
}

function showResidual(set, ctx) {
  const svg = svgNode(set.svg);
  const body = el('div.stack', {}, el('div.banner.warn.inline', {}, icon('alert'), el('div.grow', {}, el('b', { text: 'A new label was created for the rest' }), el('div.small', { style: { fontWeight: 500 }, text: `The label you scanned now travels with the load. Print this one and stick it on the ${set.crates} crates that stay behind.` }))),
    el('div.label-preview', {}, svg ? el('div.qr-svg', {}, svg) : null, el('div', {}, el('b.big', { text: set.set_label }), el('div', { text: set.product_name }), el('div.small.muted', { text: `Batch ${set.batch_no} - ${crates(set.crates, set.crates_per_pallet)}` }), el('div.mono', { text: set.code }))));
  const print = el('button.btn.primary', { type: 'button', on: { click: () => printLabel(set, ui.config.plantName) } }, el('span.btn-icon', {}, icon('print')), 'Print new label');
  const later = el('button.btn.secondary', { type: 'button', on: { click: () => dlg.close() } }, 'Later');
  const dlg = ui.openDialog({ title: `New label ${set.set_label}`, body, actions: [later, print] });
  print.focus();
}

/**
 * openScanner({trip, ctx, code?, onAdded(res), refresh, onClose}) -> {close, update}
 */
export function openScanner({ trip, ctx, code, onAdded, refresh, onClose }) {
  let stream = null; let track = null; let running = false; let raf = 0; let detector = null; let canvas = null; let paused = false; let devices = []; let devIdx = 0; let torchOn = false;
  const act = api.action();
  const video = el('video.scan-video', { playsinline: true, muted: true, autoplay: true, 'aria-label': 'Camera view' });
  const frame = el('div.scan-frame', { hidden: true }, video, el('div.scan-aim', { 'aria-hidden': 'true' }));
  const camMsg = el('p.small.muted', { text: 'Point the camera at the QR sticker on the crates.' });
  const startBtn = el('button.btn.primary', { type: 'button', on: { click: () => startCamera() } }, el('span.btn-icon', {}, icon('qr')), 'Start camera');
  const torchBtn = el('button.btn.secondary.sm', { type: 'button', hidden: true, 'aria-pressed': 'false', on: { click: toggleTorch } }, 'Light on/off');
  const switchBtn = el('button.btn.secondary.sm', { type: 'button', hidden: true, on: { click: switchCamera } }, icon('refresh', 'sm'), 'Other camera');
  const manual = ui.field({ label: 'Or type the label code', name: 'qrcode', type: 'text', placeholder: 'e.g. 7K3M9Q2XAB', help: 'The 10 letters and numbers printed under the QR sticker.', maxlength: 64, autocapitalize: 'characters' });
  manual.input.addEventListener('input', () => { manual.input.value = manual.input.value.toUpperCase(); });
  const findBtn = el('button.btn.secondary', { type: 'button', on: { click: () => find(manual.get()) } }, icon('search', 'sm'), 'Find label');
  manual.input.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); find(manual.get()); } });
  const result = el('div.stack.scan-result', { 'aria-live': 'polite' });
  const body = el('div.stack', {}, frame, el('div.btn-row', {}, startBtn, torchBtn, switchBtn), camMsg, el('div.row.end', {}, el('div.grow', {}, manual.el), findBtn), result);
  const closeBtn = el('button.btn.secondary', { type: 'button', on: { click: () => dlg.close() } }, 'Done');
  const dlg = ui.openDialog({ title: `Scan label - ${trip.vehicle_reg}`, body, actions: [closeBtn], wide: false });
  dlg.closed.then(() => { stop(); if (onClose) onClose(); });

  /* ------------------------------------------------------------ camera */
  async function startCamera(deviceId) {
    stop();
    if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) { camMsg.textContent = 'This browser cannot open the camera here. Type the label code instead.'; return; }
    try {
      stream = await navigator.mediaDevices.getUserMedia({ video: deviceId ? { deviceId: { exact: deviceId } } : { facingMode: { ideal: 'environment' } }, audio: false });
    } catch (e) { camMsg.textContent = e && e.name === 'NotAllowedError' ? 'Camera permission was refused. Allow it in the browser, or type the label code below.' : 'Could not start the camera. Type the label code below.'; return; }
    track = stream.getVideoTracks()[0]; video.srcObject = stream; frame.hidden = false; startBtn.hidden = true; paused = false;
    try { await video.play(); } catch { /* autoplay policy: user already tapped */ }
    const caps = track.getCapabilities ? track.getCapabilities() : {};
    torchBtn.hidden = !caps.torch;
    try { devices = (await navigator.mediaDevices.enumerateDevices()).filter((d) => d.kind === 'videoinput'); } catch { devices = []; }
    switchBtn.hidden = devices.length < 2; devIdx = Math.max(0, devices.findIndex((d) => d.deviceId === (track.getSettings() || {}).deviceId));
    camMsg.textContent = 'Hold steady - the label is read automatically.';
    if ('BarcodeDetector' in window) { try { detector = new window.BarcodeDetector({ formats: ['qr_code'] }); } catch { detector = null; } }
    if (!detector) { try { await loadScript('/js/jsQR.js'); } catch { camMsg.textContent = 'The scanner could not be loaded. Type the label code below.'; return; } canvas = document.createElement('canvas'); }
    running = true; loop();
  }
  function stop() { running = false; cancelAnimationFrame(raf); if (stream) stream.getTracks().forEach((t) => t.stop()); stream = null; track = null; torchOn = false; if (video) video.srcObject = null; frame.hidden = true; }
  async function toggleTorch() { if (!track) return; torchOn = !torchOn; try { await track.applyConstraints({ advanced: [{ torch: torchOn }] }); torchBtn.setAttribute('aria-pressed', String(torchOn)); } catch { torchOn = false; ui.toast.warn('The light is not available on this camera'); } }
  function switchCamera() { if (devices.length < 2) return; devIdx = (devIdx + 1) % devices.length; startCamera(devices[devIdx].deviceId); }
  let last = 0;
  async function loop() {
    if (!running) return;
    raf = requestAnimationFrame(loop);
    const now = performance.now(); if (now - last < 160 || paused || video.readyState < 2) return; last = now;
    let text = '';
    try {
      if (detector) { const r = await detector.detect(video); if (r && r[0]) text = r[0].rawValue; }
      else if (window.jsQR) {
        const w = Math.min(640, video.videoWidth || 640); const h = Math.round(w * ((video.videoHeight || 480) / (video.videoWidth || 640)));
        canvas.width = w; canvas.height = h; const c = canvas.getContext('2d', { willReadFrequently: true }); c.drawImage(video, 0, 0, w, h);
        const img = c.getImageData(0, 0, w, h); const r = window.jsQR(img.data, w, h, { inversionAttempts: 'dontInvert' }); if (r) text = r.data;
      }
    } catch { /* a bad frame is fine */ }
    if (text && !paused) { paused = true; if (navigator.vibrate) navigator.vibrate(60); find(text); }
  }

  /* ------------------------------------------------------------ lookup + add */
  async function find(raw) {
    const c = extractCode(raw); if (!c) { manual.setError('Type the label code first'); return; }
    ui.mount(result, ui.spinner('Looking up the label...'));
    try {
      const set = await api.get(`/api/qr/sets/lookup/${encodeURIComponent(c)}`);
      showSet(set);
    } catch (e) {
      paused = false;
      if (e.status === 404) ui.mount(result, el('div.banner.err.inline', {}, icon('ban'), el('div.grow', {}, el('b', { text: `No label with code ${c}` }), el('div.small', { style: { fontWeight: 500 }, text: 'Check the code, or scan another sticker. A label deleted in QR labels no longer works.' }))));
      else { ui.clear(result); ui.toastError(e); }
    }
  }
  function showSet(set) {
    paused = true;
    const q = ui.qtyInput({ name: 'qrcrates', label: 'Crates to load from this label', cratesPerPallet: set.crates_per_pallet || 0, max: set.crates, value: String(set.crates), min: 1 });
    const split = el('div.small.muted'); const upd = () => { const n = q.value; ui.clear(split); if (n && n < set.crates) split.append(`Partial: ${crates(set.crates - n, set.crates_per_pallet)} stay on a new label that you can print.`); };
    q.input.addEventListener('input', upd);
    const add = el('button.btn.primary', { type: 'button', disabled: !set.loadable, on: { click: addIt } }, el('span.btn-icon', {}, icon('plus')), 'Add to trip');
    const again = el('button.btn.ghost', { type: 'button', on: { click: () => { ui.clear(result); paused = false; manual.set(''); } } }, 'Scan another');
    ui.mount(result, el('section.card.scan-set', {}, el('div.row.between', { style: { flexWrap: 'nowrap' } }, el('div.grow', {}, el('b.big', { text: `Label ${set.set_label}` }), el('div.small.muted', { text: `${set.product_name} - batch ${set.batch_no}` }), el('div.small.muted', { text: `exp ${ui.fmtDate(set.expiry_date)}` })), ui.badge(set.status)),
      el('div.row', {}, el('span', { text: 'On this label: ' }), el('b', {}, cratesNode(set.crates, set.crates_per_pallet))),
      set.loadable ? el('div.stack.tight', {}, q.el, split) : el('div.banner.warn.inline', {}, icon('lock'), el('div.grow', {}, el('b', { text: 'This label cannot be loaded' }), el('div.small', { style: { fontWeight: 500 }, text: set.loadable_reason || 'Not available' }))),
      el('div.btn-row', {}, again, set.loadable ? add : null)));
    async function addIt() {
      const n = q.value; if (!n || !q.valid) { q.input.focus(); return; }
      if (set.qc_status !== 'APPROVED') { const ok = await ui.confirmDialog({ title: 'Load unapproved stock?', message: `Batch ${set.batch_no} is ${set.qc_status}.`, warnings: ['These crates would leave the plant before QC approval.'], confirmText: 'Yes, load it' }); if (!ok) return; }
      const res = await run(add, () => act.post(`/api/loading/trips/${encodeURIComponent(trip.id)}/items`, { qr: set.code, crates: n }), { refresh });
      if (res) {
        ui.toast.ok(`Label ${set.set_label} added`, { sub: `${crates(n, set.crates_per_pallet)} - ${set.product_name}` }); (res.warnings || []).forEach((w) => ui.toast.warn(w));
        if (onAdded) await onAdded(res);
        ui.clear(result); manual.set(''); paused = false;
        if (res.residual_set) showResidual(res.residual_set, ctx);
      } else { paused = false; }
    }
  }
  if (code) find(code);
  return { close: () => dlg.close(), update() { /* the scanned set is re-validated by the server on add */ } };
}
