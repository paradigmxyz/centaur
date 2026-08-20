import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  static targets = ["preset", "cron", "expression"]

  connect() {
    this.update()
  }

  update() {
    const cronSelected = this.presetTarget.value === "cron"
    this.cronTarget.hidden = !cronSelected
    this.expressionTarget.disabled = !cronSelected
    this.expressionTarget.required = cronSelected
  }
}
