import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  static targets = ["preset", "custom", "customTime"]

  connect() {
    this.update()
  }

  update() {
    const customSelected = this.presetTarget.value === "custom"
    this.customTarget.hidden = !customSelected
    this.customTimeTarget.required = customSelected
  }
}
