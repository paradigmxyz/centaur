// Configure your import map in config/importmap.rb. Read more: https://github.com/rails/importmap-rails
import "@hotwired/turbo-rails"
import "controllers"

// PWA service worker: offline fallback page + static asset cache. Requires a
// secure context (https or localhost), so registration silently no-ops in
// plain-http dev setups.
if ("serviceWorker" in navigator) {
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("/service-worker.js", { scope: "/" }).catch(() => {})
  })
}

// Ask the browser to exempt this origin's storage (IndexedDB, caches) from
// eviction under disk pressure. Granted silently for installed PWAs; a plain
// tab may ignore it. Best-effort either way.
if (navigator.storage?.persist) {
  navigator.storage.persist().catch(() => {})
}

// Dock-icon badge for the installed app (running agents, pending approvals,
// ...). No-op in browsers without the Badging API or in a plain tab.
window.ConsoleBadge = {
  set(count) {
    if (!("setAppBadge" in navigator)) return
    const update = count > 0 ? navigator.setAppBadge(count) : navigator.clearAppBadge()
    update.catch(() => {})
  },
  clear() {
    if ("clearAppBadge" in navigator) navigator.clearAppBadge().catch(() => {})
  }
}
