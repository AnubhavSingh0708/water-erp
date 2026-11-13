// pwa.js - service worker registration, "install app" prompt, Web Push helpers (used by profile.html).
import { api } from '/js/api.js';

export function registerSW() {
  if (!('serviceWorker' in navigator)) return;
  const go = () => navigator.serviceWorker.register('/sw.js', { scope: '/' }).catch((e) => console.debug('sw register failed', e && e.message));
  if (document.readyState === 'complete') go(); else window.addEventListener('load', go, { once: true });
}

let deferred = null;
window.addEventListener('beforeinstallprompt', (e) => { e.preventDefault(); deferred = e; window.dispatchEvent(new CustomEvent('erp:installable')); });
window.addEventListener('appinstalled', () => { deferred = null; window.dispatchEvent(new CustomEvent('erp:installed')); });
export const canInstall = () => !!deferred;
export async function promptInstall() { if (!deferred) return false; deferred.prompt(); const r = await deferred.userChoice; deferred = null; return r.outcome === 'accepted'; }
export const isStandalone = () => window.matchMedia('(display-mode: standalone)').matches || navigator.standalone === true;
export const isIOS = () => /iphone|ipad|ipod/i.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);

/* ----- Web Push ----- */
export function urlBase64ToUint8Array(b64) {
  const pad = '='.repeat((4 - (b64.length % 4)) % 4);
  const raw = atob((b64 + pad).replace(/-/g, '+').replace(/_/g, '/'));
  const out = new Uint8Array(raw.length); for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}
export const pushSupported = () => 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window;

/** -> {supported, permission, subscribed, serverEnabled, publicKey, endpoint, reason} */
export async function pushStatus() {
  const st = { supported: pushSupported(), permission: 'Notification' in window ? Notification.permission : 'denied', subscribed: false, serverEnabled: false, publicKey: null, endpoint: null, reason: '' };
  try { const k = await api.get('/api/notifications/push/key', null, { retries: 1 }); st.serverEnabled = !!(k && k.enabled && k.public_key); st.publicKey = k && k.public_key; } catch (e) { st.reason = e.message || ''; }
  if (st.supported) {
    try { const reg = await navigator.serviceWorker.getRegistration('/'); const sub = reg && await reg.pushManager.getSubscription(); if (sub) { st.subscribed = true; st.endpoint = sub.endpoint; } } catch { /* ignore */ }
  }
  return st;
}
export async function pushEnable(publicKey) {
  const perm = await Notification.requestPermission();
  if (perm !== 'granted') throw Object.assign(new Error('Notifications are blocked for this site.'), { code: 'PERMISSION' });
  const reg = await navigator.serviceWorker.ready;
  let sub = await reg.pushManager.getSubscription();
  if (!sub) sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: urlBase64ToUint8Array(publicKey) });
  const j = sub.toJSON();
  await api.post('/api/notifications/push/subscribe', { endpoint: j.endpoint, keys: { p256dh: j.keys.p256dh, auth: j.keys.auth } });
  return sub.endpoint;
}
export async function pushDisable() {
  const reg = await navigator.serviceWorker.getRegistration('/');
  const sub = reg && await reg.pushManager.getSubscription();
  if (!sub) return;
  const endpoint = sub.endpoint;
  try { await api.post('/api/notifications/push/unsubscribe', { endpoint }); } finally { await sub.unsubscribe().catch(() => {}); }
}
export const pushTest = () => api.post('/api/notifications/push/test', {});
