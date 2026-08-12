import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  static targets = [
    "panel", "query", "results", "selected", "inputs", "noProject",
    "label", "previous", "next", "status"
  ]
  static values = { url: String }

  connect() {
    this.repositories = new Map()
    this.cursor = null
    this.nextCursor = null
    this.history = []
    this.loaded = false
  }

  toggle() {
    this.panelTarget.hidden = !this.panelTarget.hidden
    if (!this.panelTarget.hidden) {
      this.positionPanel()
      if (!this.loaded) this.load()
    }
  }

  close() {
    this.panelTarget.hidden = true
  }

  search(event) {
    event?.preventDefault()
    this.cursor = null
    this.history = []
    this.load()
  }

  keydown(event) {
    if (event.key === "Enter") this.search(event)
    if (event.key === "Escape") this.close()
  }

  next() {
    if (!this.nextCursor) return
    this.history.push(this.cursor)
    this.cursor = this.nextCursor
    this.load()
  }

  previous() {
    if (this.history.length === 0) return
    this.cursor = this.history.pop()
    this.load()
  }

  choose(event) {
    const input = event.currentTarget
    const repository = JSON.parse(input.dataset.repository)
    if (input.checked) {
      this.repositories.set(repository.repository_id, repository)
      if (this.hasNoProjectTarget) this.noProjectTarget.checked = false
    } else {
      this.repositories.delete(repository.repository_id)
    }
    this.renderSelection()
  }

  remove(event) {
    this.repositories.delete(event.currentTarget.dataset.repositoryId)
    this.renderSelection()
    this.renderChecks()
  }

  noProject() {
    if (this.noProjectTarget.checked) this.repositories.clear()
    this.renderSelection()
    this.renderChecks()
  }

  async load() {
    this.statusTarget.textContent = "Loading projects"
    const url = new URL(this.urlValue, window.location.origin)
    const query = this.queryTarget.value.trim()
    if (query) url.searchParams.set("query", query)
    if (this.cursor) url.searchParams.set("cursor", this.cursor)
    try {
      const response = await fetch(url, { headers: { Accept: "application/json" } })
      if (!response.ok) throw new Error("request failed")
      const page = await response.json()
      this.loaded = true
      this.nextCursor = page.next_cursor || null
      this.renderResults(page.items || page.repositories || [])
      this.statusTarget.textContent = ""
      this.previousTarget.disabled = this.history.length === 0
      this.nextTarget.disabled = !this.nextCursor
      this.positionPanel()
    } catch (_error) {
      this.resultsTarget.replaceChildren()
      this.statusTarget.textContent = "Projects are temporarily unavailable"
    }
  }

  positionPanel() {
    if (this.panelTarget.hidden) return
    const margin = 8
    this.panelTarget.classList.remove("repository-picker-panel--above")
    this.panelTarget.style.maxHeight = ""
    const picker = this.element.getBoundingClientRect()
    const panel = this.panelTarget.getBoundingClientRect()
    const below = window.innerHeight - picker.bottom - margin
    const above = picker.top - margin
    if (panel.height > below && above > below) {
      this.panelTarget.classList.add("repository-picker-panel--above")
      if (panel.height > above) this.panelTarget.style.maxHeight = `${Math.max(above, 0)}px`
    } else if (panel.height > below) {
      this.panelTarget.style.maxHeight = `${Math.max(below, 0)}px`
    }
  }

  renderResults(repositories) {
    this.resultsTarget.replaceChildren(...repositories.map((repository) => {
      const label = document.createElement("label")
      label.className = "repository-picker-result"
      const checkbox = document.createElement("input")
      checkbox.type = "checkbox"
      checkbox.checked = this.repositories.has(repository.repository_id)
      checkbox.dataset.repository = JSON.stringify(repository)
      checkbox.dataset.action = "repository-picker#choose"
      const text = document.createElement("span")
      text.className = "min-w-0"
      const name = document.createElement("span")
      name.className = "repository-picker-result-name"
      name.textContent = repository.display_name
      const path = document.createElement("span")
      path.className = "repository-picker-result-path"
      path.textContent = repository.path_with_namespace
      text.append(name, path)
      label.append(checkbox, text)
      return label
    }))
  }

  renderChecks() {
    this.resultsTarget.querySelectorAll("input[type=checkbox]").forEach((input) => {
      const repository = JSON.parse(input.dataset.repository)
      input.checked = this.repositories.has(repository.repository_id)
    })
  }

  renderSelection() {
    this.inputsTarget.replaceChildren()
    this.selectedTarget.replaceChildren()
    for (const repository of this.repositories.values()) {
      const hidden = document.createElement("input")
      hidden.type = "hidden"
      hidden.name = "repository_ids[]"
      hidden.value = repository.repository_id
      this.inputsTarget.append(hidden)

      const chip = document.createElement("span")
      chip.className = "repository-picker-chip"
      const name = document.createElement("span")
      name.textContent = repository.display_name
      const remove = document.createElement("button")
      remove.type = "button"
      remove.dataset.action = "repository-picker#remove"
      remove.dataset.repositoryId = repository.repository_id
      remove.setAttribute("aria-label", `Remove ${repository.display_name}`)
      remove.textContent = "x"
      chip.append(name, remove)
      this.selectedTarget.append(chip)
    }
    const count = this.repositories.size
    this.labelTarget.textContent = this.hasNoProjectTarget && this.noProjectTarget.checked
      ? "No project"
      : count > 0 ? `${count} project${count === 1 ? "" : "s"}` : "Projects"
  }
}
