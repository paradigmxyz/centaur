// Service worker for the installed Centaur Console PWA.
//
// Deliberately conservative: console pages are session-authenticated and
// server-rendered, so HTML is never cached. Navigations go straight to the
// network and only fall back to the offline page when the network itself is
// unreachable. Digested assets under /assets/ (propshaft) and the static PWA
// icons are safe to cache forever.

const CACHE_VERSION = "centaur-console-v1";
const OFFLINE_URL = "/offline.html";
const PRECACHE_URLS = [OFFLINE_URL, "/pwa-icon.svg", "/pwa-icon-192.png", "/pwa-icon-512.png"];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE_VERSION).then((cache) => cache.addAll(PRECACHE_URLS)).then(() => self.skipWaiting())
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((key) => key !== CACHE_VERSION).map((key) => caches.delete(key))))
      .then(() => self.clients.claim())
  );
});

// Web Push: show whatever the server sent ({ title, options }); options.data.path
// tells notificationclick where to focus. The backend send path (VAPID keys,
// subscription storage) ships separately -- until then these never fire.
self.addEventListener("push", (event) => {
  if (!event.data) return
  const { title, options } = event.data.json()
  event.waitUntil(self.registration.showNotification(title || "Centaur Console", options))
})

self.addEventListener("notificationclick", (event) => {
  event.notification.close()
  const path = event.notification.data?.path || "/"
  event.waitUntil(
    clients.matchAll({ type: "window", includeUncontrolled: true }).then((clientList) => {
      const existing = clientList.find((client) => new URL(client.url).pathname === path && "focus" in client)
      if (existing) return existing.focus()
      return clients.openWindow ? clients.openWindow(path) : undefined
    })
  )
})

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  // Navigations: network first, offline page as the last resort. Never served
  // from cache, so login state and Turbo behavior are unchanged when online.
  if (request.mode === "navigate") {
    event.respondWith(
      fetch(request).catch(() => caches.match(OFFLINE_URL, { cacheName: CACHE_VERSION }))
    );
    return;
  }

  // Digested assets and static icons: cache first, populate on miss.
  const cacheable = url.pathname.startsWith("/assets/") || PRECACHE_URLS.includes(url.pathname);
  if (!cacheable) return;

  event.respondWith(
    caches.open(CACHE_VERSION).then(async (cache) => {
      const cached = await cache.match(request);
      if (cached) return cached;

      const response = await fetch(request);
      if (response.ok) cache.put(request, response.clone());
      return response;
    })
  );
});
