// api.js - the only place that talks to /api/*.
// Cookie -> X-Auth-Key, JSON helpers, idempotency keys reused on retries, automatic retry with back-off,
// normalised errors, 401 -> login redirect, dry-run + confirm flows, authenticated downloads.

const COOKIE = 'erp_key';
const TIMEOUT_MS = 30000;
const BACKOFF = [600, 1600, 3600];

export function getKey() {
  const m = document.cookie.split('; ').find((c) => c.startsWith(COOKIE + '='));
  if (!m) return '';
  try { return decodeURIComponent(m.slice(COOKIE.length + 1)); } catch { return ''; }
}
export function clearKey() { document.cookie = COOKIE + '=; Max-Age=0; Path=/; SameSite=Strict'; document.cookie = COOKIE + '=; Max-Age=0; Path=/; SameSite=Strict; Secure'; }

/** Idempotency-Key: 8-64 chars [A-Za-z0-9_-]. */
export function newKey() {
  try { if (crypto.randomUUID) return crypto.randomUUID(); } catch { /* insecure context */ }
  const b = new Uint8Array(16);
  (crypto.getRandomValues ? crypto.getRandomValues(b) : b.forEach((_, i) => (b[i] = Math.floor(Math.random() * 256))));
  const h = [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const emit = (ok, detail = {}) => { try { window.dispatchEvent(new CustomEvent('erp:net', { detail: { ok, ...detail } })); } catch { /* ignore */ } };

function buildUrl(path, query) {
  if (!query) return path;
  const sp = new URLSearchParams();
  for (const [k, v] of Object.entries(query)) {
    if (v === undefined || v === null || v === '') continue;
    if (Array.isArray(v)) v.forEach((x) => sp.append(k, String(x)));
    else sp.set(k, String(v));
  }
  const qs = sp.toString();
  if (!qs) return path;
  return path + (path.includes('?') ? '&' : '?') + qs;
}

const DEFAULT_HINTS = {
  NETWORK: 'Check the Wi-Fi / mobile data. Your entries are kept - press Retry when the connection is back.',
  TIMEOUT: 'The server took too long. Nothing is lost - press Retry (the same request id is reused, so nothing is saved twice).',
};

/** Normalise any failure to {status, code, message, hint, fields, details, requestId, network}. */
function normaliseError(status, body, headers, extra = {}) {
  const e = (body && body.error) || {};
  const requestId = (headers && headers.get && headers.get('x-request-id')) || '';
  const err = {
    status, code: e.code || (status ? `HTTP_${status}` : 'NETWORK'),
    message: e.message || '', hint: e.hint || '', fields: e.fields || null, details: e.details || null,
    requestId, network: false, ...extra,
  };
  if (!err.message) {
    err.message = status === 502 || status === 503 || status === 504 ? 'The server is busy or restarting.'
      : status === 404 ? 'Not found.' : status === 403 ? 'You are not allowed to do this.' : status === 401 ? 'Your session has ended.'
      : status >= 500 ? 'Something went wrong on the server.' : 'The request could not be completed.';
  }
  if (!err.hint && DEFAULT_HINTS[err.code]) err.hint = DEFAULT_HINTS[err.code];
  return err;
}

export function loginUrl(next) {
  const here = next ?? (location.pathname + location.search + location.hash);
  const safe = here && !/login\.html|setup\.html/.test(here) && here !== '/' ? `?next=${encodeURIComponent(here)}` : '';
  return '/login.html' + safe;
}
let redirecting = false;
export function goLogin(next) {
  if (redirecting) return;
  redirecting = true;
  clearKey();
  location.replace(loginUrl(next));
}

const RETRY_STATUS = new Set([502, 503, 504]);

/**
 * request(method, path, {body, query, key, retries, timeout, noAuthRedirect, noRetry429, signal, raw, headers})
 * Resolves with parsed JSON (null for 204) or the full wrapper when `raw:true` => {data,status,headers,requestId,replayed}.
 * Rejects with a normalised error object.
 */
export async function request(method, path, opts = {}) {
  const isWrite = method !== 'GET' && method !== 'HEAD';
  const url = buildUrl(path, opts.query);
  const isDry = /[?&]dry_run=/.test(url);
  const key = isWrite && !isDry ? (opts.key || newKey()) : null;   // generated ONCE per call, reused by every retry below
  const maxRetries = opts.retries ?? 3;
  let attempt = 0;
  for (;;) {
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(new DOMException('timeout', 'TimeoutError')), opts.timeout ?? TIMEOUT_MS);
    if (opts.signal) opts.signal.addEventListener('abort', () => ctl.abort(opts.signal.reason), { once: true });
    const headers = { Accept: 'application/json', ...(opts.headers || {}) };
    const k = getKey();
    if (k) headers['X-Auth-Key'] = k;
    if (key) headers['Idempotency-Key'] = key;
    let body;
    if (opts.body !== undefined && opts.body !== null) { headers['Content-Type'] = 'application/json'; body = JSON.stringify(opts.body); }
    let res = null; let failure = null;
    try {
      res = await fetch(url, { method, headers, body, signal: ctl.signal, credentials: 'same-origin', cache: 'no-store' });
    } catch (ex) {
      const timeout = ex && (ex.name === 'TimeoutError' || (ctl.signal.aborted && ctl.signal.reason && ctl.signal.reason.name === 'TimeoutError'));
      if (opts.signal && opts.signal.aborted) throw normaliseError(0, null, null, { code: 'ABORTED', message: 'Cancelled.', network: false, aborted: true });
      failure = normaliseError(0, null, null, {
        code: timeout ? 'TIMEOUT' : 'NETWORK', network: true,
        message: timeout ? 'The server did not answer in time.' : 'Cannot reach the server. Nothing was sent or saved.',
      });
    } finally { clearTimeout(timer); }

    if (failure) {
      emit(false, { reason: failure.code });
      if (attempt < maxRetries) { await sleep(BACKOFF[attempt] * (0.8 + Math.random() * 0.4)); attempt++; continue; }
      failure.requestKey = key;
      throw failure;
    }

    emit(true);
    const requestId = res.headers.get('x-request-id') || '';
    const replayed = res.headers.get('idempotent-replay') === 'true';
    let data = null;
    if (res.status !== 204) {
      const text = await res.text().catch(() => '');
      if (text) { try { data = JSON.parse(text); } catch { data = null; } }
    }
    if (res.ok) {
      if (opts.raw) return { data, status: res.status, headers: res.headers, requestId, replayed };
      return data;
    }
    const err = normaliseError(res.status, data, res.headers);
    err.requestKey = key;
    if (res.status === 401 && !opts.noAuthRedirect && err.code !== 'LOCKED') {
      err.redirecting = true;
      goLogin();
      throw err;
    }
    const retryable = RETRY_STATUS.has(res.status)
      || (res.status === 429 && !opts.noRetry429 && err.code !== 'LOCKED')
      || (res.status === 409 && err.code === 'IN_PROGRESS');
    if (retryable && attempt < maxRetries) {
      const ra = Number(res.headers.get('retry-after'));
      await sleep(Math.min(ra > 0 ? ra * 1000 : BACKOFF[attempt], 8000) * (0.8 + Math.random() * 0.4));
      attempt++; continue;
    }
    if (res.status === 503 || res.status === 502 || res.status === 504) emit(false, { reason: 'HTTP_' + res.status });
    throw err;
  }
}

const withQuery = (path, query) => buildUrl(path, query);

export const api = {
  request,
  get: (path, query, opts) => request('GET', path, { ...opts, query }),
  post: (path, body, opts) => request('POST', path, { ...opts, body: body ?? {} }),
  put: (path, body, opts) => request('PUT', path, { ...opts, body: body ?? {} }),
  patch: (path, body, opts) => request('PATCH', path, { ...opts, body: body ?? {} }),
  del: (path, opts) => request('DELETE', path, opts),
  newKey,
  /** Dry run: POST path?dry_run=1 -> {dry_run:true, effects:[{item,zone,delta,after}], warnings:[]} (rolled back server side). */
  dryRun: (path, body, opts) => request('POST', withQuery(path, { dry_run: 1 }), { ...opts, body: body ?? {} }),
  /** Download a file via authenticated fetch -> Blob -> save. Works for ?format=csv. */
  async download(path, { query, filename } = {}) {
    const url = buildUrl(path, query);
    let res;
    try { res = await fetch(url, { headers: { 'X-Auth-Key': getKey() }, credentials: 'same-origin', cache: 'no-store' }); }
    catch { emit(false); throw normaliseError(0, null, null, { code: 'NETWORK', network: true, message: 'Cannot reach the server. Nothing was downloaded.' }); }
    if (res.status === 401) { goLogin(); throw normaliseError(401, null, res.headers, { redirecting: true }); }
    if (!res.ok) { let b = null; try { b = await res.json(); } catch { /* not json */ } throw normaliseError(res.status, b, res.headers); }
    const blob = await res.blob();
    const cd = res.headers.get('content-disposition') || '';
    const m = /filename\*?=(?:UTF-8'')?"?([^";]+)"?/i.exec(cd);
    const name = filename || (m && decodeURIComponent(m[1])) || path.split('/').pop().split('?')[0] || 'download';
    saveBlob(blob, name);
    return { name, size: blob.size };
  },
  /** GET a text/blob resource with the auth header (e.g. an SVG). */
  async fetchBlob(path, query) {
    const res = await fetch(buildUrl(path, query), { headers: { 'X-Auth-Key': getKey() }, cache: 'no-store' });
    if (!res.ok) throw normaliseError(res.status, null, res.headers);
    return res.blob();
  },
};

export function saveBlob(blob, name) {
  const u = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = u; a.download = name; a.rel = 'noopener';
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(u), 10000);
}

/**
 * One user action = one Idempotency-Key. The key is reused for every retry of the SAME request
 * (same method, path and body, ignoring `confirm`) and replaced as soon as the body changes or after success.
 *
 *   const act = api.action();
 *   await act.post('/api/admin/customers', body);     // double-click / retry / "Not sent - Retry" => same key
 */
api.action = function action() {
  let sig = ''; let key = '';
  const run = (method, path, body, opts = {}) => {
    const { confirm, ...rest } = (body && typeof body === 'object' && !Array.isArray(body)) ? body : {};
    const s = method + ' ' + path + ' ' + JSON.stringify(Array.isArray(body) ? body : rest);
    if (s !== sig || !key) { sig = s; key = newKey(); }
    return request(method, path, { ...opts, body: method === 'DELETE' ? undefined : body, key }).then((r) => { key = ''; sig = ''; return r; });
  };
  return {
    get key() { return key; },
    reset() { key = ''; sig = ''; },
    post: (p, b, o) => run('POST', p, b ?? {}, o),
    put: (p, b, o) => run('PUT', p, b ?? {}, o),
    del: (p, o) => run('DELETE', p, undefined, o),
    /** POST with the CONFIRM_REQUIRED flow (dialog listing details.warnings, then resend with confirm:true, same key). */
    postConfirm: (p, b, o) => withConfirm((body) => run('POST', p, body, o), b ?? {}, o),
    putConfirm: (p, b, o) => withConfirm((body) => run('PUT', p, body, o), b ?? {}, o),
  };
};

/**
 * withConfirm(send, body, {confirmTitle, confirmText}) - `send(body)` returns a promise.
 * On 409 CONFIRM_REQUIRED shows the warnings and re-sends the same body with confirm:true if the user agrees.
 * Resolves undefined-ish `null` and rejects with {cancelled:true} if the user declines.
 */
export async function withConfirm(send, body, opts = {}) {
  try { return await send(body); } catch (err) {
    if (!err || err.code !== 'CONFIRM_REQUIRED') throw err;
    const ui = await import('/js/ui.js');
    const warnings = (err.details && (err.details.warnings || err.details.messages)) || [err.message];
    const ok = await ui.confirmDialog({
      title: opts.confirmTitle || 'Please check before continuing', message: opts.confirmMessage || '', warnings,
      confirmText: opts.confirmText || 'Yes, continue', cancelText: 'Cancel, go back', danger: !!opts.danger,
    });
    if (!ok) throw { cancelled: true, code: 'CANCELLED', message: 'Cancelled.' };
    return send({ ...body, confirm: true });
  }
}
api.withConfirm = withConfirm;

/** Disable a button, show a spinner while the promise (or function returning one) runs. Always restores. */
export function busy(button, work) {
  const p = typeof work === 'function' ? work() : work;
  if (!button) return p;
  button.disabled = true; button.classList.add('busy'); button.setAttribute('aria-busy', 'true');
  if (!button.querySelector('.spinner')) { const s = document.createElement('span'); s.className = 'spinner'; s.setAttribute('aria-hidden', 'true'); button.prepend(s); }
  const done = () => { button.disabled = false; button.classList.remove('busy'); button.removeAttribute('aria-busy'); };
  return Promise.resolve(p).then((r) => { done(); return r; }, (e) => { done(); throw e; });
}
api.busy = busy;

export default api;
