import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  static targets = ["input", "feedback", "submit"]
  static values = { url: String, saved: Boolean }

  disconnect() {
    clearTimeout(this.timer)
    this.request?.abort()
  }

  validate() {
    clearTimeout(this.timer)
    this.request?.abort()
    this.version = (this.version || 0) + 1
    const version = this.version
    const key = this.inputTarget.value
    this.inputTarget.removeAttribute("aria-invalid")
    if (!key.trim()) {
      this.feedbackTarget.textContent = this.savedValue
        ? "Leave blank to keep your saved key."
        : "Paste the complete private key, including the BEGIN and END lines."
      return
    }
    this.feedbackTarget.textContent = "Checking key…"
    this.timer = setTimeout(async () => {
      this.request = new AbortController()
      try {
        const response = await fetch(this.urlValue, {
          method: "POST",
          credentials: "same-origin",
          cache: "no-store",
          headers: {
            "Content-Type": "application/json",
            "Accept": "application/json",
            "X-CSRF-Token": document.querySelector('meta[name="csrf-token"]')?.content || ""
          },
          body: JSON.stringify({ private_key: key }),
          signal: this.request.signal
        })
        if (response.status !== 200 && response.status !== 422) throw new Error("Validation unavailable")
        const result = await response.json()
        if (version !== this.version) return
        this.inputTarget.setAttribute("aria-invalid", String(!result.valid))
        this.feedbackTarget.textContent = result.valid
          ? `${result.message} Fingerprint: ${result.fingerprint}. Not saved yet—click Save SSH key.`
          : result.message
      } catch (error) {
        if (error.name !== "AbortError" && version === this.version) {
          this.feedbackTarget.textContent = "Could not check the key. Save to validate it, or try again."
        }
      }
    }, 500)
  }

  saving() {
    clearTimeout(this.timer)
    this.request?.abort()
    this.submitTarget.value = "Saving…"
    this.submitTarget.disabled = true
  }
}
