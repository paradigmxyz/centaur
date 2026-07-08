import { Controller } from "@hotwired/stimulus"

// Local file workbench built on the File System Access API. All reads and
// writes happen in the browser against the operator's own disk; no file
// content ever reaches the server. Browsers without the API (Firefox, Safari)
// degrade to a read-only <input type="file"> picker.
//
// Also consumes window.launchQueue: when a file is opened with the installed
// PWA via the OS "Open with" menu (see file_handlers in the manifest), the
// launched handles are delivered here and opened directly.

const HANDLE_DB = "centaur-console-local-files"
const HANDLE_STORE = "handles"
const LAST_DIRECTORY_KEY = "last-directory"
const MAX_EDITABLE_BYTES = 2 * 1024 * 1024

export default class extends Controller {
  static targets = [
    "unsupported", "dirButton", "fileButton", "restoreButton", "fallbackLabel", "fallbackInput",
    "breadcrumbs", "entries", "empty",
    "filename", "meta", "saveButton", "editor", "preview", "placeholder", "status"
  ]

  connect() {
    this.supported = "showDirectoryPicker" in window
    this.crumbs = []       // [{ name, handle }] from the picked root to the current dir
    this.fileHandle = null // writable handle for the open file, when we have one
    this.objectUrl = null

    if (!this.supported) {
      this.unsupportedTarget.hidden = false
      this.dirButtonTarget.hidden = true
      this.fileButtonTarget.hidden = true
      this.fallbackLabelTarget.hidden = false
    }

    this.consumeLaunchQueue()
    if (this.supported) this.offerRestore()
  }

  disconnect() {
    this.revokeObjectUrl()
  }

  // --- opening folders and files ---

  async openDirectory() {
    try {
      const handle = await window.showDirectoryPicker({ mode: "readwrite" })
      await this.storeHandle(LAST_DIRECTORY_KEY, handle)
      this.restoreButtonTarget.hidden = true
      await this.enterDirectory([{ name: handle.name, handle }])
    } catch (error) {
      this.reportUnlessAborted(error)
    }
  }

  async openFile() {
    try {
      const [handle] = await window.showOpenFilePicker()
      await this.openFileHandle(handle)
    } catch (error) {
      this.reportUnlessAborted(error)
    }
  }

  // Reopen the directory persisted in IndexedDB from a previous visit. The
  // permission re-request needs a user gesture, hence a button instead of
  // restoring automatically on connect.
  async restore() {
    try {
      const handle = await this.loadHandle(LAST_DIRECTORY_KEY)
      if (!handle) return
      if (!(await this.ensurePermission(handle, "readwrite")) && !(await this.ensurePermission(handle, "read"))) {
        this.setStatus("Permission to reopen the folder was denied.")
        return
      }
      this.restoreButtonTarget.hidden = true
      await this.enterDirectory([{ name: handle.name, handle }])
    } catch (error) {
      this.reportUnlessAborted(error)
    }
  }

  dragover(event) {
    event.preventDefault()
  }

  async drop(event) {
    event.preventDefault()
    const item = [...event.dataTransfer.items].find((candidate) => candidate.kind === "file")
    if (!item) return

    // getAsFileSystemHandle gives a real handle (so drops are editable);
    // getAsFile is the read-only fallback. Both must be called before the
    // dataTransfer goes inert, so no awaits above this line.
    if (item.getAsFileSystemHandle) {
      const handle = await item.getAsFileSystemHandle()
      if (handle.kind === "directory") {
        await this.storeHandle(LAST_DIRECTORY_KEY, handle)
        await this.enterDirectory([{ name: handle.name, handle }])
      } else {
        await this.openFileHandle(handle)
      }
      return
    }

    const file = item.getAsFile()
    if (file) await this.showFile(file)
  }

  fallbackChange() {
    const files = [...this.fallbackInputTarget.files]
    if (!files.length) return
    this.crumbs = []
    this.renderBreadcrumbs()
    this.renderEntries(files.map((file) => ({
      name: file.name,
      kind: "file",
      activate: () => this.showFile(file)
    })))
    this.showFile(files[0])
  }

  consumeLaunchQueue() {
    if (!("launchQueue" in window)) return
    window.launchQueue.setConsumer(async (params) => {
      const handle = (params.files || []).find((candidate) => candidate.kind === "file")
      if (handle) await this.openFileHandle(handle)
    })
  }

  // --- directory listing ---

  async enterDirectory(crumbs) {
    try {
      const dir = crumbs[crumbs.length - 1].handle
      const dirs = []
      const files = []
      for await (const entry of dir.values()) {
        (entry.kind === "directory" ? dirs : files).push(entry)
      }
      const byName = (a, b) => a.name.localeCompare(b.name)
      dirs.sort(byName)
      files.sort(byName)

      this.crumbs = crumbs
      this.renderBreadcrumbs()
      this.renderEntries([...dirs, ...files].map((entry) => ({
        name: entry.kind === "directory" ? `${entry.name}/` : entry.name,
        kind: entry.kind,
        activate: () => entry.kind === "directory"
          ? this.enterDirectory([...this.crumbs, { name: entry.name, handle: entry }])
          : this.openFileHandle(entry)
      })))
    } catch (error) {
      this.reportUnlessAborted(error)
    }
  }

  renderBreadcrumbs() {
    this.breadcrumbsTarget.hidden = this.crumbs.length === 0
    this.breadcrumbsTarget.replaceChildren(...this.crumbs.flatMap((crumb, index) => {
      const last = index === this.crumbs.length - 1
      const button = document.createElement("button")
      button.type = "button"
      button.textContent = crumb.name
      button.className = last ? "text-zinc-300" : "cursor-pointer hover:text-centaur-300"
      button.disabled = last
      button.addEventListener("click", () => this.enterDirectory(this.crumbs.slice(0, index + 1)))
      if (last) return [button]
      const separator = document.createElement("span")
      separator.textContent = "/"
      return [button, separator]
    }))
  }

  renderEntries(items) {
    this.emptyTarget.hidden = items.length > 0
    this.entriesTarget.replaceChildren(...items.map((item) => {
      const row = document.createElement("li")
      const button = document.createElement("button")
      button.type = "button"
      button.className = "flex w-full cursor-pointer items-center gap-2 rounded px-2 py-1.5 text-left font-mono text-xs text-zinc-300 transition-colors hover:bg-centaur-500/[0.06] hover:text-centaur-300"
      const marker = document.createElement("span")
      marker.textContent = item.kind === "directory" ? "▸" : "·"
      marker.className = "w-3 shrink-0 text-zinc-600"
      const name = document.createElement("span")
      name.textContent = item.name
      name.className = "truncate"
      button.append(marker, name)
      button.addEventListener("click", item.activate)
      row.append(button)
      return row
    }))
  }

  // --- preview and editing ---

  async openFileHandle(handle) {
    try {
      const file = await handle.getFile()
      await this.showFile(file, handle)
    } catch (error) {
      this.setStatus(`Could not open ${handle.name}: ${error.message}`)
    }
  }

  async showFile(file, handle = null) {
    this.revokeObjectUrl()
    this.fileHandle = handle
    this.filenameTarget.textContent = file.name
    this.metaTarget.textContent = [
      this.formatBytes(file.size),
      file.type || "unknown type",
      `modified ${new Date(file.lastModified).toLocaleString()}`
    ].join(" · ")
    this.placeholderTarget.hidden = true
    this.setStatus("")

    if (file.type.startsWith("image/")) {
      this.objectUrl = URL.createObjectURL(file)
      const image = document.createElement("img")
      image.src = this.objectUrl
      image.alt = file.name
      image.className = "max-h-full max-w-full object-contain"
      this.showPreview(image)
      return
    }

    const probe = new Uint8Array(await file.slice(0, 8192).arrayBuffer())
    if (probe.includes(0)) {
      const notice = document.createElement("p")
      notice.textContent = "Binary file — no preview."
      notice.className = "text-xs text-zinc-500"
      this.showPreview(notice)
      return
    }

    const truncated = file.size > MAX_EDITABLE_BYTES
    const text = truncated ? await file.slice(0, MAX_EDITABLE_BYTES).text() : await file.text()
    const writable = !truncated && typeof handle?.createWritable === "function"

    this.editorTarget.value = text
    this.editorTarget.readOnly = !writable
    this.editorTarget.hidden = false
    this.previewTarget.hidden = true
    this.saveButtonTarget.hidden = !writable

    if (truncated) {
      this.setStatus(`Showing the first ${this.formatBytes(MAX_EDITABLE_BYTES)} of ${this.formatBytes(file.size)} (read-only).`)
    } else if (!writable) {
      this.setStatus("Read-only: opened without a writable file handle.")
    }
  }

  showPreview(node) {
    this.previewTarget.replaceChildren(node)
    this.previewTarget.hidden = false
    this.editorTarget.hidden = true
    this.saveButtonTarget.hidden = true
  }

  async save() {
    if (!this.fileHandle) return
    try {
      if (!(await this.ensurePermission(this.fileHandle, "readwrite"))) {
        this.setStatus("Write permission denied.")
        return
      }
      const stream = await this.fileHandle.createWritable()
      await stream.write(this.editorTarget.value)
      await stream.close()
      this.setStatus(`Saved ${this.fileHandle.name} to disk at ${new Date().toLocaleTimeString()}.`)
    } catch (error) {
      this.setStatus(`Save failed: ${error.message}`)
    }
  }

  markDirty() {
    if (!this.saveButtonTarget.hidden) this.setStatus("Unsaved changes.")
  }

  // --- helpers ---

  async offerRestore() {
    try {
      const handle = await this.loadHandle(LAST_DIRECTORY_KEY)
      if (!handle) return
      this.restoreButtonTarget.textContent = `Reopen “${handle.name}”`
      this.restoreButtonTarget.hidden = false
    } catch {
      // IndexedDB unavailable; restore is best-effort.
    }
  }

  async ensurePermission(handle, mode) {
    const descriptor = { mode }
    if ((await handle.queryPermission(descriptor)) === "granted") return true
    return (await handle.requestPermission(descriptor)) === "granted"
  }

  openDb() {
    return new Promise((resolve, reject) => {
      const request = indexedDB.open(HANDLE_DB, 1)
      request.onupgradeneeded = () => request.result.createObjectStore(HANDLE_STORE)
      request.onsuccess = () => resolve(request.result)
      request.onerror = () => reject(request.error)
    })
  }

  async storeHandle(key, handle) {
    try {
      const db = await this.openDb()
      await new Promise((resolve, reject) => {
        const tx = db.transaction(HANDLE_STORE, "readwrite")
        tx.objectStore(HANDLE_STORE).put(handle, key)
        tx.oncomplete = resolve
        tx.onerror = () => reject(tx.error)
      })
      db.close()
    } catch {
      // Persistence is best-effort; the picker still works without it.
    }
  }

  async loadHandle(key) {
    const db = await this.openDb()
    const handle = await new Promise((resolve, reject) => {
      const request = db.transaction(HANDLE_STORE).objectStore(HANDLE_STORE).get(key)
      request.onsuccess = () => resolve(request.result)
      request.onerror = () => reject(request.error)
    })
    db.close()
    return handle
  }

  reportUnlessAborted(error) {
    if (error?.name === "AbortError") return
    this.setStatus(error?.message || String(error))
  }

  setStatus(message) {
    this.statusTarget.textContent = message
  }

  formatBytes(bytes) {
    if (bytes < 1024) return `${bytes} B`
    const units = ["KB", "MB", "GB"]
    let value = bytes
    let unit = "B"
    for (const next of units) {
      if (value < 1024) break
      value /= 1024
      unit = next
    }
    return `${value.toFixed(value >= 10 ? 0 : 1)} ${unit}`
  }

  revokeObjectUrl() {
    if (this.objectUrl) {
      URL.revokeObjectURL(this.objectUrl)
      this.objectUrl = null
    }
  }
}
