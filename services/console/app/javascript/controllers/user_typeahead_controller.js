import { Controller } from "@hotwired/stimulus"

// Multi-user picker with a filterable listbox. Selected users are submitted as
// opaque IDs, while all filtering and chip management stays client-side.
export default class extends Controller {
  static targets = ["input", "listbox", "option", "selections", "empty"]

  connect() {
    this.syncOptions()
  }

  open() {
    this.listboxTarget.hidden = false
    this.inputTarget.setAttribute("aria-expanded", "true")
    this.filter()
  }

  close(event) {
    if (event && this.element.contains(event.target)) return

    this.listboxTarget.hidden = true
    this.inputTarget.setAttribute("aria-expanded", "false")
    this.activeOption = null
  }

  filter() {
    const query = this.inputTarget.value.trim().toLowerCase()
    const selectedOids = this.selectedOids()

    this.optionTargets.forEach((option) => {
      const selected = selectedOids.has(option.dataset.userOid)
      option.closest("li").hidden = selected || !option.dataset.searchText.includes(query)
    })
  }

  select(event) {
    event.preventDefault()
    this.addOption(event.currentTarget)
  }

  remove(event) {
    event.preventDefault()
    event.currentTarget.closest("[data-user-typeahead-target='selection']").remove()
    this.syncOptions()
    this.inputTarget.focus()
    this.open()
  }

  navigate(event) {
    if (event.key === "Escape") {
      this.close()
      return
    }

    if (!["ArrowDown", "ArrowUp", "Enter"].includes(event.key)) return

    const options = this.visibleOptions()
    if (options.length === 0) return

    event.preventDefault()
    if (event.key === "Enter") {
      this.addOption(this.activeOption || options[0])
      return
    }

    const currentIndex = options.indexOf(this.activeOption)
    const offset = event.key === "ArrowDown" ? 1 : -1
    const nextIndex = currentIndex === -1
      ? (offset === 1 ? 0 : options.length - 1)
      : (currentIndex + offset + options.length) % options.length
    this.focusOption(options[nextIndex])
  }

  addOption(option) {
    if (this.selectedOids().has(option.dataset.userOid)) return

    const row = document.createElement("div")
    row.className = "flex items-center justify-between gap-3 rounded border border-ink-600 bg-ink-850 px-3 py-2"
    row.dataset.userTypeaheadTarget = "selection"
    row.dataset.userOid = option.dataset.userOid

    const label = document.createElement("span")
    label.className = "min-w-0 truncate text-sm text-zinc-300"
    label.textContent = option.dataset.label

    const hidden = document.createElement("input")
    hidden.type = "hidden"
    hidden.name = "skill[editor_oids][]"
    hidden.value = option.dataset.userOid

    const remove = document.createElement("button")
    remove.type = "button"
    remove.className = "shrink-0 text-xs text-zinc-500 transition-colors hover:text-red-400"
    remove.dataset.action = "user-typeahead#remove"
    remove.textContent = "Remove"

    row.append(label, hidden, remove)
    this.selectionsTarget.append(row)
    this.inputTarget.value = ""
    this.syncOptions()
    this.inputTarget.focus()
  }

  syncOptions() {
    this.filter()
    this.emptyTarget.hidden = this.hasSelectionsTarget && this.selectionsTarget.children.length > 0
  }

  selectedOids() {
    return new Set(
      Array.from(
        this.selectionsTarget.querySelectorAll("input[name='skill[editor_oids][]']"),
        (input) => input.value
      )
    )
  }

  visibleOptions() {
    return this.optionTargets.filter((option) => !option.closest("li").hidden)
  }

  focusOption(option) {
    this.optionTargets.forEach((candidate) => {
      candidate.setAttribute("aria-selected", "false")
      candidate.classList.remove("bg-ink-700")
    })
    option.setAttribute("aria-selected", "true")
    option.classList.add("bg-ink-700")
    this.activeOption = option
  }
}
