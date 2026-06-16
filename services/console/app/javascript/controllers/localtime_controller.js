import { Controller } from "@hotwired/stimulus"

// Localizes a server-rendered UTC timestamp to the viewer's time zone. The
// element ships with an ISO-8601 fallback in its text so it is still readable
// before JS connects (and when JS is disabled).
//
//   data-localtime-relative-value="true"  -> "5 minutes ago", with the absolute
//                                            local time as a hover tooltip.
export default class extends Controller {
  static values = { datetime: String, relative: Boolean }

  connect() {
    const date = new Date(this.datetimeValue)
    if (isNaN(date.getTime())) return

    const absolute = this.formatAbsolute(date)

    if (this.relativeValue) {
      this.element.textContent = this.relativeFrom(date)
      this.element.title = absolute
    } else {
      this.element.textContent = absolute
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
    const seconds = Math.round((date.getTime() - Date.now()) / 1000)
    const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" })
    const units = [
      ["year", 31536000], ["month", 2592000], ["day", 86400],
      ["hour", 3600], ["minute", 60]
    ]
    for (const [unit, secs] of units) {
      if (Math.abs(seconds) >= secs) return rtf.format(Math.round(seconds / secs), unit)
    }
    return rtf.format(seconds, "second")
  }
}
