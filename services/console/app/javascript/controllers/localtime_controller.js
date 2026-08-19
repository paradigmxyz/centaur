import { Controller } from "@hotwired/stimulus"

// Localizes a server-rendered UTC timestamp to the viewer's time zone. The
// element ships with an ISO-8601 fallback in its text so it is still readable
// before JS connects (and when JS is disabled).
//
//   data-localtime-relative-value="true"  -> "5 minutes ago", with the absolute
//                                            local time as a hover tooltip.
//   data-localtime-format-value="compact" -> "4d", with the absolute local time
//                                            as a hover tooltip.
//
// Relative displays re-render every 30s so "now" ages into "1m" without a
// page visit, and truncate toward zero so 90s reads as 1m, not 2m.
export default class extends Controller {
  static values = { datetime: String, format: String, relative: Boolean }

  connect() {
    this.date = new Date(this.datetimeValue)
    if (isNaN(this.date.getTime())) return

    this.render()
    if (this.formatValue === "compact" || this.relativeValue) {
      this.timer = setInterval(() => this.render(), 30000)
    }
  }

  disconnect() {
    if (this.timer) clearInterval(this.timer)
  }

  render() {
    const absolute = this.formatAbsolute(this.date)

    if (this.formatValue === "compact") {
      this.element.textContent = this.compactRelativeFrom(this.date)
      this.element.title = absolute
    } else if (this.relativeValue) {
      this.element.textContent = this.relativeFrom(this.date)
      this.element.title = absolute
    } else {
      this.element.textContent = absolute
      this.element.title = absolute
    }
  }

  // MM/DD/YYYY HH:MM:SS in the viewer's local time zone (24-hour, zero-padded).
  formatAbsolute(date) {
    const pad = (n) => String(n).padStart(2, "0")
    const d = `${pad(date.getMonth() + 1)}/${pad(date.getDate())}/${date.getFullYear()}`
    const t = `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
    return `${d} ${t}`
  }

  relativeFrom(date) {
    const seconds = Math.trunc((date.getTime() - Date.now()) / 1000)
    const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" })
    const units = [
      ["year", 31536000], ["month", 2592000], ["day", 86400],
      ["hour", 3600], ["minute", 60]
    ]
    for (const [unit, secs] of units) {
      if (Math.abs(seconds) >= secs) return rtf.format(Math.trunc(seconds / secs), unit)
    }
    return rtf.format(seconds, "second")
  }

  compactRelativeFrom(date) {
    const seconds = Math.abs(Math.trunc((Date.now() - date.getTime()) / 1000))
    const units = [
      ["y", 31536000], ["mo", 2592000], ["w", 604800],
      ["d", 86400], ["h", 3600], ["m", 60]
    ]
    for (const [unit, secs] of units) {
      if (seconds >= secs) return `${Math.floor(seconds / secs)}${unit}`
    }
    return "now"
  }
}
