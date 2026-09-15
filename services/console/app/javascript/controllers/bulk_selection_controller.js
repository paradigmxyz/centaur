import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  static targets = ["item", "all", "submit", "toolbar", "count", "operation"]

  connect() {
    this.update()
  }

  toggleAll() {
    this.itemTargets.forEach((item) => { item.checked = this.allTarget.checked })
    this.update()
  }

  update() {
    const selected = this.itemTargets.filter((item) => item.checked).length
    const operationSelected = this.hasOperationTarget && this.operationTarget.value !== ""
    this.submitTargets.forEach((button) => { button.disabled = selected === 0 || !operationSelected })
    if (this.hasToolbarTarget) this.toolbarTarget.hidden = selected === 0
    if (this.hasCountTarget) this.countTarget.textContent = `${selected} selected`

    if (this.hasAllTarget) {
      this.allTarget.checked = selected > 0 && selected === this.itemTargets.length
      this.allTarget.indeterminate = selected > 0 && selected < this.itemTargets.length
    }
  }
}
