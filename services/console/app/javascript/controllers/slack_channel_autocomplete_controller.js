import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  static targets = ["input", "value", "list", "status", "submit", "deliveryMode", "channelFields"]
  static values = { url: String, dmUserId: String }

  connect() {
    this.options = []
    this.activeIndex = -1
    this.opened = false
    this.selectedDisplay = null
    this.channelValue = /^[CDG][A-Z0-9]{8,}$/.test(this.valueTarget.value) ? this.valueTarget.value : ""
    this.deliveryModeChanged()
    this.updateSubmitState()
  }

  disconnect() {
    clearTimeout(this.searchTimer)
    clearTimeout(this.blurTimer)
    this.abortController?.abort()
  }

  open() {
    clearTimeout(this.blurTimer)
    this.opened = true
    if (this.inputTarget.value === this.selectedDisplay) this.inputTarget.select()
    this.search()
  }

  input() {
    this.selectedDisplay = null
    this.opened = true
    this.syncManualChannelId()
    clearTimeout(this.searchTimer)
    this.searchTimer = setTimeout(() => this.search(), 200)
  }

  keydown(event) {
    if (event.key === "ArrowDown") {
      event.preventDefault()
      this.moveActive(1)
    } else if (event.key === "ArrowUp") {
      event.preventDefault()
      this.moveActive(-1)
    } else if (event.key === "Enter" && this.activeIndex >= 0) {
      event.preventDefault()
      this.select(this.options[this.activeIndex])
    } else if (event.key === "Escape") {
      this.hide()
    }
  }

  blur() {
    this.blurTimer = setTimeout(() => {
      this.opened = false
      this.hide()
    }, 150)
  }

  async search() {
    this.abortController?.abort()
    this.abortController = new AbortController()

    const preservingSelection = this.inputTarget.value === this.selectedDisplay
    if (!preservingSelection) this.setStatus("Loading channels…")

    const query = preservingSelection ? "" : this.inputTarget.value.trim()
    this.currentQuery = query
    const url = new URL(this.urlValue, window.location.origin)
    url.searchParams.set("q", query)

    try {
      const response = await fetch(url, {
        credentials: "same-origin",
        headers: { "Accept": "application/json" },
        signal: this.abortController.signal,
      })
      const body = await response.json().catch(() => ({}))
      if (!response.ok) throw new Error(body.error || `Request failed with HTTP ${response.status}`)

      this.options = body.options || []
      this.renderOptions()
      if (body.error) {
        this.setStatus(body.error)
      } else if (!preservingSelection) {
        this.setStatus(this.resultStatus())
      }
    } catch (error) {
      if (error.name === "AbortError") return
      this.options = []
      this.renderOptions()
      this.setStatus(error.message || "Could not load Slack channels.", true)
    }
  }

  renderOptions() {
    this.listTarget.replaceChildren()
    this.activeIndex = -1

    this.options.forEach((option, index) => {
      const button = document.createElement("button")
      button.type = "button"
      button.id = `${this.listTarget.id}_option_${index}`
      button.className = "block min-h-11 w-full px-3 py-2 text-left transition-colors hover:bg-centaur-500/[0.08] focus:bg-centaur-500/[0.08] focus:outline-none"
      button.setAttribute("role", "option")
      button.setAttribute("aria-selected", "false")
      button.addEventListener("pointerdown", (event) => event.preventDefault())
      button.addEventListener("click", () => this.select(option))

      const label = document.createElement("div")
      label.className = "text-sm text-zinc-100"
      label.textContent = option.label
      const description = document.createElement("div")
      description.className = "mt-0.5 text-xs text-zinc-500"
      description.textContent = option.description
      button.append(label, description)
      this.listTarget.append(button)
    })

    const visible = this.opened && this.options.length > 0
    this.listTarget.hidden = !visible
    this.inputTarget.setAttribute("aria-expanded", String(visible))
  }

  moveActive(delta) {
    if (this.options.length === 0) return
    this.activeIndex = (this.activeIndex + delta + this.options.length) % this.options.length

    Array.from(this.listTarget.children).forEach((element, index) => {
      const active = index === this.activeIndex
      element.setAttribute("aria-selected", String(active))
      element.classList.toggle("bg-centaur-500/[0.08]", active)
      if (active) {
        this.inputTarget.setAttribute("aria-activedescendant", element.id)
        element.scrollIntoView({ block: "nearest" })
      }
    })
  }

  select(option) {
    this.inputTarget.value = `${option.label} (${option.value})`
    this.valueTarget.value = option.value
    this.channelValue = option.value
    this.selectedDisplay = this.inputTarget.value
    this.updateSubmitState()
    this.setStatus(`Selected ${option.label}.`)
    this.opened = false
    this.hide()
  }

  syncManualChannelId() {
    const value = this.inputTarget.value.trim().toUpperCase()
    const destinationPattern = this.hasDeliveryModeTarget ? /^[CDG][A-Z0-9]{8,}$/ : /^[CDGUW][A-Z0-9]{8,}$/
    this.valueTarget.value = destinationPattern.test(value) ? value : ""
    if (this.hasDeliveryModeTarget) this.channelValue = this.valueTarget.value
    this.updateSubmitState()
  }

  deliveryModeChanged() {
    if (!this.hasDeliveryModeTarget) return

    const deliveryMode = this.deliveryModeTargets.find((input) => input.checked)?.value
    const deliverToDm = deliveryMode === "dm"
    if (deliverToDm) {
      if (/^[CDG][A-Z0-9]{8,}$/.test(this.valueTarget.value)) this.channelValue = this.valueTarget.value
      this.valueTarget.value = this.dmUserIdValue
    } else if (/^[UW][A-Z0-9]{8,}$/.test(this.valueTarget.value)) {
      this.valueTarget.value = this.channelValue
    }

    this.channelFieldsTarget.hidden = deliverToDm
    this.inputTarget.disabled = deliverToDm
    this.updateSubmitState()
  }

  updateSubmitState() {
    const deliverToDm = this.hasDeliveryModeTarget &&
      this.deliveryModeTargets.some((input) => input.checked && input.value === "dm")
    if (deliverToDm) {
      this.submitTarget.disabled = this.dmUserIdValue === ""
      return
    }

    const hasInput = this.inputTarget.value.trim() !== ""
    const hasChannelId = this.valueTarget.value.trim() !== ""
    this.submitTarget.disabled = hasInput && !hasChannelId
  }

  resultStatus() {
    if (this.options.length === 0) return "No matching Slack destinations."
    if (this.options.length === 20) {
      return this.currentQuery === ""
        ? "Showing the first 20 channels. Type to search all channels."
        : "Showing the first 20 matching channels. Keep typing to narrow the results."
    }
    return `${this.options.length} matching channel${this.options.length === 1 ? "" : "s"}.`
  }

  setStatus(message, error = false) {
    this.statusTarget.textContent = message
    this.statusTarget.classList.toggle("text-red-300", error)
    this.statusTarget.classList.toggle("text-zinc-500", !error)
  }

  hide() {
    this.listTarget.hidden = true
    this.inputTarget.setAttribute("aria-expanded", "false")
    this.inputTarget.removeAttribute("aria-activedescendant")
    this.activeIndex = -1
  }
}
