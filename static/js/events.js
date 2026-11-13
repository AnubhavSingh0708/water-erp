// events.js - realtime change feed (API_REFERENCE section 15).
//   ERP.live.start('warehouse', { stock: refreshStock, qc: refreshQc, '*': refreshAll })
// fetch + ReadableStream SSE parser (X-Auth-Key header; the key never goes into a URL), per-topic debounce 300 ms,
// handlers never run concurrently (coalesced), back-off reconnect 1 -> 30 s with jitter, refresh-all after every
// (re)connect and on `resync`, paused while the tab is hidden.
import { getKey, goLogin } from '/js/api.js';
import * as conn from '/js/connectivity.js';

const DEBOUNCE_MS = 300;
const internal = {};          // topic -> [fn]  (registered by shell.js: alerts, users, settings)
let handlers = {};            // page handlers
let page = null, ctl = null, running = false, stopped = true, backoff = 1000, retryTimer = 0;
let everConnected = false;
let status = 'off';           // live | reconnecting | paused | off
const statusSubs = new Set();
const timers = new Map();     // fn -> {t, inflight, again}

function setStatus(s) { if (s === status) return; status = s; statusSubs.forEach((f) => { try { f(s); } catch { /* ignore */ } }); }

function run(fn, immediate = false) {
  let st = timers.get(fn); if (!st) { st = { t: 0, inflight: false, again: false }; timers.set(fn, st); }
  const go = async () => {
    if (st.inflight) { st.again = true; return; }       // single in-flight refresh; coalesce
    st.inflight = true;
    try { await fn(); } catch (e) { /* handlers show their own errors */ if (!(e && (e.redirecting || e.aborted))) console.warn('live refresh failed', e && e.message); }
    finally { st.inflight = false; if (st.again) { st.again = false; run(fn); } }
  };
  clearTimeout(st.t);
  if (immediate) go(); else st.t = setTimeout(go, DEBOUNCE_MS);
}
function fnsFor(topic) {
  const out = new Set();
  [internal[topic], handlers[topic]].forEach((x) => (x || []).forEach?.((f) => out.add(f)));
  if (typeof handlers[topic] === 'function') out.add(handlers[topic]);
  if (typeof handlers['*'] === 'function' && topic !== 'alerts' && topic !== 'users') out.add(handlers['*']);
  return out;
}
function allFns() {
  const out = new Set();
  Object.values(internal).forEach((a) => a.forEach((f) => out.add(f)));
  Object.values(handlers).forEach((f) => out.add(f));
  return out;
}
const refreshAll = () => allFns().forEach((f) => run(f));

function onEvent(ev, data) {
  if (ev === 'hello') { backoff = 1000; setStatus('live'); return; }
  if (ev === 'change') {
    let topics = []; try { topics = JSON.parse(data).topics || []; } catch { return; }
    const fns = new Set(); topics.forEach((t) => fnsFor(t).forEach((f) => fns.add(f)));
    fns.forEach((f) => run(f));
  } else if (ev === 'resync') refreshAll();
  else if (ev === 'session_ended') { stop(); goLogin(); }
  else if (ev === 'server_restarting') { conn.markRestarting(); setStatus('reconnecting'); }
}

async function connect() {
  if (running || stopped || document.hidden) return;
  running = true; ctl = new AbortController();
  let gotAny = false;
  try {
    const qs = page ? `?page=${encodeURIComponent(page)}` : '';
    const res = await fetch('/api/events' + qs, { headers: { 'X-Auth-Key': getKey(), Accept: 'text/event-stream' }, signal: ctl.signal, cache: 'no-store' });
    if (res.status === 401) { stop(); goLogin(); return; }
    if (res.status === 429) { backoff = 30000; throw new Error('too many streams'); }
    if (res.status === 400 || res.status === 403) { stop(); setStatus('off'); console.warn('live updates unavailable for this page', res.status); return; }
    if (!res.ok || !res.body) throw new Error('stream http ' + res.status);
    gotAny = true; setStatus('live');
    // after every (re)connect events may have been missed
    if (everConnected) refreshAll();
    everConnected = true;
    const reader = res.body.getReader(); const dec = new TextDecoder(); let buf = '';
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true }).replace(/\r\n/g, '\n');
      let i;
      while ((i = buf.indexOf('\n\n')) >= 0) {
        const block = buf.slice(0, i); buf = buf.slice(i + 2);
        let ev = 'message', data = '';
        for (const line of block.split('\n')) {
          if (line.startsWith(':')) continue;
          if (line.startsWith('event:')) ev = line.slice(6).trim();
          else if (line.startsWith('data:')) data += line.slice(5).trimStart();
        }
        onEvent(ev, data);
      }
    }
  } catch (e) { if (!(e && e.name === 'AbortError')) console.debug('live stream interrupted', e && e.message); }
  finally {
    running = false;
    if (!stopped && !document.hidden) {
      setStatus('reconnecting');
      const wait = Math.min(30000, backoff) * (0.75 + Math.random() * 0.5);
      if (!gotAny) backoff = Math.min(30000, backoff * 2); else backoff = 1000;
      clearTimeout(retryTimer); retryTimer = setTimeout(connect, wait);
    }
  }
}
function pause() { clearTimeout(retryTimer); if (ctl) ctl.abort(); running = false; if (!stopped) setStatus('paused'); }
function resume() { if (stopped) return; backoff = 1000; clearTimeout(retryTimer); connect(); }
export function stop() { stopped = true; clearTimeout(retryTimer); if (ctl) ctl.abort(); running = false; setStatus('off'); }

let wired = false;
function wire() {
  if (wired) return; wired = true;
  document.addEventListener('visibilitychange', () => { if (document.hidden) pause(); else resume(); });
  window.addEventListener('online', resume);
  window.addEventListener('pagehide', () => { if (ctl) ctl.abort(); });     // free the server-side stream slot (max 8 per user)
  window.addEventListener('pageshow', (e) => { if (e.persisted) resume(); });
  window.addEventListener('erp:reconnected', resume);
}

export const live = {
  /** Register shell-level handlers (alerts -> bell, users -> nav). Call before start(). */
  internal(map) { for (const [t, fn] of Object.entries(map)) (internal[t] ||= []).push(fn); },
  /** start(page, {topic: handler, '*': handler}) - page key per API_REFERENCE section 15 (e.g. 'warehouse', 'profile'). */
  start(pageKey, map = {}) {
    if (!stopped && page === pageKey) { handlers = { ...map }; return; }   // same stream: just swap handlers
    page = pageKey; handlers = { ...map }; stopped = false; everConnected = false; wire(); backoff = 1000;
    // handlers given as arrays are not supported - use one function per topic
    connect();
  },
  stop,
  /** Re-run every handler now (e.g. after the user pressed "refresh"). */
  refresh: refreshAll,
  get status() { return status; },
  onStatus(fn) { statusSubs.add(fn); fn(status); return () => statusSubs.delete(fn); },
};
export default live;
