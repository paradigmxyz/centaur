import { Controller } from "@hotwired/stimulus"

// Reveals one of several panels based on the selected radio within this element.
// Each panel carries data-toggle-key matching a radio value; the panel whose key
// equals the checked radio is shown. Used for the static secret's inject/replace
// mode switch.
export default class extends Controller {
  static targets = ["panel"]

  connect() {
    this.update()
  }

  update() {
    const checked = this.element.querySelector("input[type=radio]:checked")
    const selected = checked ? checked.value : null
    this.panelTargets.forEach((panel) => {
      panel.hidden = panel.dataset.toggleKey !== selected
    })
  }
}
