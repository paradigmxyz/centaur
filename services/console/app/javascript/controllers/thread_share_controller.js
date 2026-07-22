import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  static targets = ["dialog", "form", "copyButton", "error"]
  static values = { url: String }

  open(event) {
    event.preventDefault()
    this.reset()
    this.dialogTarget.showModal()
  }

  cancel(event) {
    event.preventDefault()
    this.dialogTarget.close()
  }

  closeFromBackdrop(event) {
    if (event.target !== this.dialogTarget) return

    const bounds = this.dialogTarget.getBoundingClientRect()
    const inside = event.clientX >= bounds.left && event.clientX <= bounds.right &&
      event.clientY >= bounds.top && event.clientY <= bounds.bottom
    if (!inside) this.dialogTarget.close()
  }

  async copyLink(event) {
    event.preventDefault()
    if (this.copyButtonTarget.disabled) return

    this.copyButtonTarget.disabled = true
    this.copyButtonTarget.textContent = "Copying…"
    this.errorTarget.hidden = true

    try {
      // Invoke clipboard access directly from the click gesture. Only publish
      // after copying succeeds so a denied clipboard permission does not make
      // the chat public as a side effect.
      try {
        await this.writeToClipboard(this.urlValue)
      } catch {
        throw new Error("Could not copy the link.")
      }
      const response = await fetch(this.formTarget.action, {
        method: "POST",
        body: new FormData(this.formTarget),
        credentials: "same-origin",
        headers: {
          "Accept": "application/json",
          "X-CSRF-Token": document.querySelector("meta[name='csrf-token']")?.content ?? ""
        }
      })
      if (!response.ok) {
        const payload = await response.json().catch(() => ({}))
        throw new Error(payload.error || "Could not share the chat.")
      }

      this.copyButtonTarget.textContent = "Copied"
      window.setTimeout(() => this.dialogTarget.close(), 600)
    } catch (error) {
      this.errorTarget.textContent = error.message || "Could not copy the link."
      this.errorTarget.hidden = false
      this.copyButtonTarget.disabled = false
      this.copyButtonTarget.textContent = "Copy link"
    }
  }

  writeToClipboard(text) {
    if (navigator.clipboard?.writeText) return navigator.clipboard.writeText(text)

    const input = document.createElement("textarea")
    input.value = text
    input.setAttribute("readonly", "")
    input.style.position = "fixed"
    input.style.opacity = "0"
    document.body.appendChild(input)
    input.select()
    const copied = document.execCommand("copy")
    input.remove()
    return copied ? Promise.resolve() : Promise.reject(new Error("Could not copy the link."))
  }

  reset() {
    this.copyButtonTarget.disabled = false
    this.copyButtonTarget.textContent = "Copy link"
    this.errorTarget.hidden = true
    this.errorTarget.textContent = ""
  }
}
