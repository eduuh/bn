/* bn-web service worker — makes the dashboard installable and resilient on flaky mobile data.
 *
 * Strategy: the API and SSE stream are NEVER cached (live fleet/cost must be fresh — a cached
 * `/api` frame would show a stale fleet); navigations are network-first with the cached shell as
 * the offline fallback; hashed static assets are cache-first (immutable, safe to serve fast). */

const CACHE = "bn-web-v1";
const SHELL = ["/", "/index.html", "/manifest.webmanifest", "/icon.svg"];

self.addEventListener("install", (event) => {
  self.skipWaiting();
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL).catch(() => undefined)));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const { request } = event;
  if (request.method !== "GET") return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  // Live data — always hit the network, never the cache.
  if (url.pathname.startsWith("/api/")) return;

  // App navigations — network-first, fall back to the cached shell when offline.
  if (request.mode === "navigate") {
    event.respondWith(fetch(request).catch(() => caches.match("/index.html")));
    return;
  }

  // Static (hashed) assets — cache-first, then populate the cache on first miss.
  event.respondWith(
    caches.match(request).then((cached) => {
      if (cached) return cached;
      return fetch(request)
        .then((res) => {
          if (res.ok) {
            const copy = res.clone();
            caches
              .open(CACHE)
              .then((c) => c.put(request, copy))
              .catch(() => undefined);
          }
          return res;
        })
        .catch(() => cached);
    }),
  );
});
