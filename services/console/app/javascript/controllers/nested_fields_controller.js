import { Controller } from "@hotwired/stimulus"

// Adds and removes repeatable form rows (request rules, labels). The markup ships
// a hidden <template> whose field names use the literal NEW_RECORD as their index
// placeholder; each insertion swaps in a fresh, monotonically increasing index so
// Rails groups the row's fields together. Removing a row just deletes it -- the
// server reindexes by position on save, so gaps left behind do not matter.
export default class extends Controller {
  static targets = ["container", "template"]

  add(event) {
    event.preventDefault()
    const html = this.templateTarget.innerHTML.replace(/NEW_RECORD/g, this.nextIndex())
    this.containerTarget.insertAdjacentHTML("beforeend", html)
  }

  remove(event) {
    event.preventDefault()
    event.target.closest("[data-nested-fields-row]").remove()
  }

  nextIndex() {
    if (this.counter === undefined) {
      this.counter = this.containerTarget.querySelectorAll("[data-nested-fields-row]").length
    } else {
      this.counter += 1
    }
    return this.counter
  }
}
