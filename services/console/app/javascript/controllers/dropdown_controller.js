import { Controller } from "@hotwired/stimulus"

// A click-to-open menu. Powers the "Add Secret" button on the secrets index.
// Closes on an outside click or the Escape key.
export default class extends Controller {
  static targets = ["menu"]

  connect() {
    this.menuTarget.hidden = true
    this._onOutside = (e) => { if (!this.element.contains(e.target)) this.hide() }
    this._onEsc = (e) => { if (e.key === "Escape") this.hide() }
  }

  disconnect() {
    this.unbind()
  }

  toggle(event) {
    event.stopPropagation()
    this.menuTarget.hidden ? this.show() : this.hide()
  }

  show() {
    this.menuTarget.hidden = false
    document.addEventListener("click", this._onOutside)
    document.addEventListener("keydown", this._onEsc)
  }

  hide() {
    this.menuTarget.hidden = true
    this.unbind()
  }

  unbind() {
    document.removeEventListener("click", this._onOutside)
    document.removeEventListener("keydown", this._onEsc)
  }
}
