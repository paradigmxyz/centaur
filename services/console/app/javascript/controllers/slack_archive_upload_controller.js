import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  static targets = ["form", "status", "submitButton"]
  static values = {
    createUrl: String,
    startUrlTemplate: String,
  }

  async submit(event) {
    event.preventDefault()
    const formData = new FormData(this.formTarget)
    const file = formData.get("archive")

    if (!file || file.size === 0) {
      this.setStatus("Choose a Slack export ZIP first.", true)
      return
    }
    if (!file.name.toLowerCase().endsWith(".zip")) {
      this.setStatus("Slack archive imports must be ZIP files.", true)
      return
    }

    this.setBusy(true)
    try {
      const contentType = file.type || "application/zip"
      this.setStatus("Creating archive import...")
      const createResponse = await this.fetchJson(this.createUrlValue, {
        method: "POST",
        body: JSON.stringify({
          filename: file.name,
          content_type: contentType,
        }),
      })

      this.setStatus("Uploading archive...")
      const uploadResponse = await fetch(createResponse.upload.upload_url, {
        method: "PUT",
        headers: { "Content-Type": contentType },
        body: file,
      })
      if (!uploadResponse.ok) {
        throw new Error(`Archive upload failed with HTTP ${uploadResponse.status}`)
      }

      if (formData.get("auto_start") === "1") {
        this.setStatus("Starting ingest workflow...")
        await this.fetchJson(
          this.startUrlTemplateValue.replace("__IMPORT_ID__", createResponse.import.import_id),
          { method: "POST" },
        )
      }

      this.setStatus("Archive accepted. Refreshing...")
      window.location.reload()
    } catch (error) {
      this.setStatus(error.message || "Archive upload failed.", true)
      this.setBusy(false)
    }
  }

  async fetchJson(url, options = {}) {
    const response = await fetch(url, {
      credentials: "same-origin",
      headers: {
        "Accept": "application/json",
        "Content-Type": "application/json",
        "X-CSRF-Token": this.csrfToken(),
        ...(options.headers || {}),
      },
      ...options,
    })
    const body = await response.json().catch(() => ({}))
    if (!response.ok) {
      throw new Error(body.error || `Request failed with HTTP ${response.status}`)
    }
    return body
  }

  csrfToken() {
    return document.querySelector("meta[name='csrf-token']")?.content || ""
  }

  setBusy(busy) {
    this.submitButtonTarget.disabled = busy
    this.submitButtonTarget.classList.toggle("opacity-50", busy)
    this.submitButtonTarget.classList.toggle("cursor-wait", busy)
  }

  setStatus(message, error = false) {
    this.statusTarget.textContent = message
    this.statusTarget.classList.toggle("text-red-300", error)
    this.statusTarget.classList.toggle("text-zinc-500", !error)
  }
}
