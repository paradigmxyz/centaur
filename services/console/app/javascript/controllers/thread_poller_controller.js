import { Controller } from "@hotwired/stimulus"

// Polls one thread panel's transcript while its turn is running and swaps only
// that panel's transcript stream in place (ThreadsController#panel). Replaces
// the old whole-page Turbo refresh, so a running thread never re-renders the
// other panes, their composers, or in-progress drafts.
export default class extends Controller {
  static targets = ["transcript"]
  static values = {
    url: String,
    active: Boolean,
    interval: { type: Number, default: 4000 }
  }

  connect() {
    // A composer submit hands control to the server-side redirect; polling in
    // that window could paint a pre-submit transcript over the optimistic
    // user bubble (see _composer.html.erb).
    this.section = this.element.closest("section")
    this.onSubmit = () => this.cancel()
    this.section?.addEventListener("submit", this.onSubmit)
    if (this.activeValue) this.schedule()
  }

  disconnect() {
    this.section?.removeEventListener("submit", this.onSubmit)
    this.cancel()
  }

  schedule() {
    this.cancel()
    this.timer = window.setTimeout(() => this.refresh(), this.intervalValue)
  }

  cancel() {
    if (this.timer) window.clearTimeout(this.timer)
    this.timer = null
  }

  async refresh() {
    try {
      const response = await fetch(this.urlValue, {
        credentials: "same-origin",
        headers: { "Accept": "text/html" }
      })
      if (response.ok) {
        const active = response.headers.get("X-Console-Execution-Active") === "true"
        this.swap(await response.text())
        // The final transcript (turn result included) just rendered; a
        // composer submit re-renders the page and restarts the poller.
        if (!active) return
      }
    } catch {
      // Transient network error; retry on the next tick.
    }
    this.schedule()
  }

  swap(html) {
    if (html === this.lastHtml) return
    this.lastHtml = html

    // Keep the reader's place: reopen the disclosures they had expanded and
    // stay pinned to the bottom when they were already there. The transcript
    // is append-mostly, so positional indexes are stable across swaps.
    const openIndexes = new Set()
    this.transcriptTarget.querySelectorAll("details").forEach((details, index) => {
      if (details.open) openIndexes.add(index)
    })
    const pinned = this.element.scrollHeight - this.element.scrollTop - this.element.clientHeight < 48

    this.transcriptTarget.innerHTML = html
    this.transcriptTarget.querySelectorAll("details").forEach((details, index) => {
      if (openIndexes.has(index)) details.open = true
    })
    if (pinned) this.element.scrollTop = this.element.scrollHeight
  }
}
