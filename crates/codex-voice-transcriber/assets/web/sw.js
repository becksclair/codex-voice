const SHELL_ASSETS = [
  '/web',
  `/web/manifest.webmanifest?v=${WEB_BUILD_REVISION}`,
  `/web/manifest-light.webmanifest?v=${WEB_BUILD_REVISION}`,
  `/web/icon-192.png?v=${WEB_BUILD_REVISION}`,
  `/web/icon-512.png?v=${WEB_BUILD_REVISION}`,
  `/web/icon-maskable-512.png?v=${WEB_BUILD_REVISION}`,
  `/web/apple-touch-icon.png?v=${WEB_BUILD_REVISION}`
];
const NETWORK_FIRST_ASSETS = new Set([
  '/web',
  '/web/manifest.webmanifest',
  '/web/manifest-light.webmanifest'
]);
const CACHE_FIRST_ASSETS = new Set([
  '/web/icon-192.png',
  '/web/icon-512.png',
  '/web/icon-maskable-512.png',
  '/web/apple-touch-icon.png'
]);

async function networkFirst(request, cacheKey) {
  const cache = await caches.open(CACHE_NAME);
  try {
    const response = await fetch(request);
    if (response.ok) {
      await cache.put(cacheKey, response.clone());
      return response;
    }
    const cached = await cache.match(cacheKey);
    if (cached) return cached;
    return response;
  } catch (_) {
    const cached = await cache.match(cacheKey);
    if (cached) return cached;
    throw _;
  }
}

async function cacheFirst(request) {
  const cached = await caches.match(request);
  if (cached) return cached;
  const response = await fetch(request);
  if (response.ok) {
    const cache = await caches.open(CACHE_NAME);
    await cache.put(request, response.clone());
  }
  return response;
}

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE_NAME)
      .then((cache) => cache.addAll(SHELL_ASSETS))
      .then(() => self.skipWaiting())
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches.keys()
      .then((names) => Promise.all(
        names.filter((name) => name !== CACHE_NAME).map((name) => caches.delete(name))
      ))
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  if (request.method !== 'GET') return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  if (request.mode === 'navigate' && url.pathname === '/web') {
    event.respondWith(networkFirst(request, '/web'));
    return;
  }

  if (NETWORK_FIRST_ASSETS.has(url.pathname)) {
    event.respondWith(networkFirst(request, url.pathname));
    return;
  }

  if (CACHE_FIRST_ASSETS.has(url.pathname)) {
    event.respondWith(cacheFirst(request));
  }
});
