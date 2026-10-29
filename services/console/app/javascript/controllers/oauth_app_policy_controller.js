import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  static targets = ["provider", "options", "personal", "shared", "alwaysAvailable"]

  connect() {
    this.update()
  }

  update() {
    const personal = this.providerTarget.value === "runlayer"
    this.optionsTarget.hidden = personal
    this.personalTarget.hidden = !personal
    this.sharedTarget.disabled = personal || this.sharedTarget.dataset.persisted === "true"
    this.alwaysAvailableTarget.disabled = personal
    if (personal) {
      this.sharedTarget.checked = false
      this.alwaysAvailableTarget.checked = true
    }
  }
}
