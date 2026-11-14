// Service worker: precache the static shell, never touch /api/*, network-first HTML with offline fallback,
// stale-while-revalidate for css/js/img. Bump VERSION when the precache list or caching rules change.
const VERSION = 'v2';
const SHELL = `erp-shell-${VERSION}`;
const RUNTIME = `erp-rt-${VERSION}`;
const PRECACHE = [
  '/offline.html', '/login.html', '/index.html', '/manifest.json',
  '/css/styles.css', '/img/icons.svg', '/img/icon-192.png',
  '/js/api.js', '/js/ui.js', '/js/crud.js', '/js/drafts.js', '/js/connectivity.js', '/js/events.js', '/js/nav.js', '/js/shell.js', '/js/pwa.js',
];

self.addEventListener('install', (e) => {
  e.waitUntil(
    caches.open(SHELL)
      // add one by one: a single 404 must not break the install
      .then((c) => Promise.all(PRECACHE.map((u) => c.add(new Request(u, { cache: 'reload' })).catch(() => null))))
      .then(() => self.skipWaiting()));
});

self.addEventListener('activate', (e) => {
  e.waitUntil(
    caches.keys().then((keys) => Promise.all(keys.filter((k) => k.startsWith('erp-') && k !== SHELL && k !== RUNTIME).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()));
});

const okToCache = (res) => res && res.ok && res.type === 'basic' && !/no-store/i.test(res.headers.get('cache-control') || '');

self.addEventListener('fetch', (e) => {
  const req = e.request;
  if (req.method !== 'GET') return;                 // writes are never intercepted
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  if (url.pathname.startsWith('/api/')) return;     // NEVER cache or intercept the API (auth, live data, events)
  if (req.mode === 'navigate' || (req.headers.get('accept') || '').includes('text/html')) {
    e.respondWith(networkFirst(req));
    return;
  }
  if (/\.(?:css|js|svg|png|jpg|jpeg|webp|ico|json|woff2?)$/.test(url.pathname)) { e.respondWith(staleWhileRevalidate(req)); }
});

async function networkFirst(req) {
  const cache = await caches.open(RUNTIME);
  try {
    const res = await fetchWithTimeout(req, 6000);
    if (okToCache(res)) cache.put(req, res.clone());
    return res;
  } catch {
    const hit = (await cache.match(req)) || (await caches.match(req, { ignoreSearch: true }));
    return hit || (await caches.match('/offline.html')) || new Response('Offline', { status: 503, headers: { 'Content-Type': 'text/plain' } });
  }
}
function fetchWithTimeout(req, ms) {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error('timeout')), ms);
    fetch(req).then((r) => { clearTimeout(t); resolve(r); }, (e) => { clearTimeout(t); reject(e); });
  });
}
async function staleWhileRevalidate(req) {
  const cache = await caches.open(RUNTIME);
  const hit = (await cache.match(req)) || (await caches.match(req));
  const net = fetch(req).then((res) => { if (okToCache(res)) cache.put(req, res.clone()); return res; }).catch(() => null);
  return hit || (await net) || new Response('', { status: 504 });
}

/* ---- Web Push ---- */
self.addEventListener('push', (e) => {
  let d = {};
  try { d = e.data ? e.data.json() : {}; } catch { d = { title: 'Notification', body: e.data ? e.data.text() : '' }; }
  const title = d.title || 'Neersagar ERP';
  e.waitUntil(self.registration.showNotification(title, {
    body: d.body || '', tag: d.tag || (d.alert_id ? `alert-${d.alert_id}` : undefined), renotify: !!d.tag,
    icon: '/img/icon-192.png', badge: '/img/icon-192.png', data: { url: d.url || '/dashboard.html' },
  }));
});

self.addEventListener('notificationclick', (e) => {
  e.notification.close();
  let target = (e.notification.data && e.notification.data.url) || '/dashboard.html';
  try { const u = new URL(target, self.location.origin); target = u.origin === self.location.origin ? u.pathname + u.search + u.hash : '/dashboard.html'; } catch { target = '/dashboard.html'; }
  e.waitUntil((async () => {
    const all = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
    for (const c of all) { if ('focus' in c) { try { await c.navigate(target); } catch { /* ignore */ } return c.focus(); } }
    return self.clients.openWindow(target);
  })());
});
