// Formatting helpers ported from app/helpers/application_helper.rb and the
// localtime Stimulus controller.

/** "salesforce…rest-api": keep head and tail of long opaque ids. */
export function truncateMiddle(value: string | null | undefined, max = 40, omission = "…"): string {
  const text = value ?? "";
  if (text.length <= max) return text;
  const keep = max - omission.length;
  const head = Math.ceil(keep / 2);
  const tail = Math.floor(keep / 2);
  return `${text.slice(0, head)}${omission}${tail > 0 ? text.slice(-tail) : ""}`;
}

export function pluralize(count: number, singular: string, plural = `${singular}s`): string {
  return `${count.toLocaleString("en-US")} ${count === 1 ? singular : plural}`;
}

const pad = (n: number) => String(n).padStart(2, "0");

/** MM/DD/YYYY HH:MM:SS in the viewer's time zone (24-hour). */
export function formatAbsolute(date: Date): string {
  return `${pad(date.getMonth() + 1)}/${pad(date.getDate())}/${date.getFullYear()} ${pad(date.getHours())}:${pad(
    date.getMinutes(),
  )}:${pad(date.getSeconds())}`;
}

/** "5 minutes ago", truncating toward zero so 90s reads as 1 minute. */
export function formatRelative(date: Date, now: number = Date.now()): string {
  const seconds = Math.trunc((date.getTime() - now) / 1000);
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ["year", 31536000],
    ["month", 2592000],
    ["day", 86400],
    ["hour", 3600],
    ["minute", 60],
  ];
  for (const [unit, secs] of units) {
    if (Math.abs(seconds) >= secs) return rtf.format(Math.trunc(seconds / secs), unit);
  }
  return rtf.format(seconds, "second");
}

/** "4d", "1mo", "now". */
export function formatCompact(date: Date, now: number = Date.now()): string {
  const seconds = Math.abs(Math.trunc((now - date.getTime()) / 1000));
  const units: [string, number][] = [
    ["y", 31536000],
    ["mo", 2592000],
    ["w", 604800],
    ["d", 86400],
    ["h", 3600],
    ["m", 60],
  ];
  for (const [unit, secs] of units) {
    if (seconds >= secs) return `${Math.floor(seconds / secs)}${unit}`;
  }
  return "now";
}

/** name || foreign_id || id, the label every console table uses. */
export function displayLabel(record: { name?: string | null; foreign_id?: string | null; id: string }): string {
  return record.name || record.foreign_id || record.id;
}
