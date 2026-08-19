import { Controller } from "@hotwired/stimulus"

// Shows an "Install app" banner when the browser reports the console is
// installable, and drives the native install prompt. beforeinstallprompt fires
// once per page load — usually before any Stimulus controller connects, and
// never again across Turbo visits — so the deferred event is captured at
// module scope and controllers sync with it on connect.

let deferredPrompt = null

window.addEventListener("beforeinstallprompt", (event) => {
  event.preventDefault()
  deferredPrompt = event
  window.dispatchEvent(new CustomEvent("pwa:installable"))
})

window.addEventListener("appinstalled", () => {
  deferredPrompt = null
  window.dispatchEvent(new CustomEvent("pwa:installed"))
})

export default class extends Controller {
  static targets = ["banner"]

  connect() {
    this.sync = () => { this.bannerTarget.hidden = !deferredPrompt }
    window.addEventListener("pwa:installable", this.sync)
    window.addEventListener("pwa:installed", this.sync)
    this.sync()
  }

  disconnect() {
    window.removeEventListener("pwa:installable", this.sync)
    window.removeEventListener("pwa:installed", this.sync)
  }

  async install() {
    if (!deferredPrompt) return
    deferredPrompt.prompt()
    await deferredPrompt.userChoice
    deferredPrompt = null
    this.sync()
  }
}
