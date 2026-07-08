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
