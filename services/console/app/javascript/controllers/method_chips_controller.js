import { Controller } from "@hotwired/stimulus"

// Multi-select HTTP-method chips backing a single comma-joined hidden input.
// The "*" (ALL) chip is mutually exclusive with the specific methods: selecting
// it clears the others, and selecting any specific method clears it.
export default class extends Controller {
  static targets = ["button", "input"]
  static WILDCARD = "*"

  toggle(event) {
    const btn = event.currentTarget
    if (this.isOn(btn)) {
      this.setOn(btn, false)
    } else {
      this.setOn(btn, true)
      if (btn.dataset.method === this.constructor.WILDCARD) {
        this.buttonTargets.forEach((b) => { if (b !== btn) this.setOn(b, false) })
      } else {
        this.buttonTargets.forEach((b) => {
          if (b.dataset.method === this.constructor.WILDCARD) this.setOn(b, false)
        })
      }
    }
    this.sync()
  }

  isOn(btn) {
    return btn.classList.contains("chip-on")
  }

  setOn(btn, on) {
    btn.classList.toggle("chip-on", on)
    btn.setAttribute("aria-pressed", on ? "true" : "false")
  }

  sync() {
    const selected = this.buttonTargets.filter((b) => this.isOn(b)).map((b) => b.dataset.method)
    this.inputTarget.value = selected.join(",")
  }
}
