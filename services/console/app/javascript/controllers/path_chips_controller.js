import { Controller } from "@hotwired/stimulus"

// A token input for request paths. Typing a value and pressing comma (or Enter)
// turns it into a removable chip; Backspace on an empty entry removes the last
// chip. All chips are mirrored into a single comma-joined hidden input that the
// server parses. Blur also commits, so a half-typed path is not lost on submit.
export default class extends Controller {
  static targets = ["input", "list", "entry"]

  keydown(event) {
    if (event.key === "," || event.key === "Enter") {
      event.preventDefault()
      this.commit()
    } else if (event.key === "Backspace" && this.entryTarget.value === "") {
      this.removeLast()
    }
  }

  commit() {
    this.entryTarget.value
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean)
      .forEach((value) => this.addToken(value))
    this.entryTarget.value = ""
    this.sync()
  }

  addToken(value) {
    const token = document.createElement("span")
    token.className = "chip-token"
    token.dataset.value = value
    token.dataset.pathChipsTarget = "token"

    const label = document.createElement("span")
    label.textContent = value

    const remove = document.createElement("button")
    remove.type = "button"
    remove.className = "chip-remove"
    remove.textContent = "×"
    remove.dataset.action = "path-chips#remove"

    token.append(label, remove)
    this.listTarget.insertBefore(token, this.entryTarget)
  }

  remove(event) {
    event.target.closest(".chip-token").remove()
    this.sync()
  }

  removeLast() {
    const tokens = this.tokens()
    if (tokens.length === 0) return
    tokens[tokens.length - 1].remove()
    this.sync()
  }

  focus() {
    this.entryTarget.focus()
  }

  tokens() {
    return Array.from(this.listTarget.querySelectorAll(".chip-token"))
  }

  sync() {
    this.inputTarget.value = this.tokens().map((t) => t.dataset.value).join(",")
  }
}
