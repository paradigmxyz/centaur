import { useEffect, useState } from "react";

import { formatAbsolute, formatCompact, formatRelative } from "@/lib/format";

interface LocalTimeProps {
  value: string | null | undefined;
  /** "absolute" (default): MM/DD/YYYY HH:MM:SS; "relative": "5 minutes ago"; "compact": "4d". */
  format?: "absolute" | "relative" | "compact";
  className?: string;
}

/**
 * A timestamp in the viewer's time zone (port of the localtime Stimulus
 * controller). Relative formats re-render every 30s and show the absolute time
 * on hover. Renders an em dash for null.
 */
export function LocalTime({ value, format = "absolute", className }: LocalTimeProps) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (format === "absolute") return;
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, [format]);

  if (!value) return <span className="text-subtle-foreground">—</span>;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return <span className={className}>{value}</span>;

  const absolute = formatAbsolute(date);
  const text =
    format === "compact" ? formatCompact(date, now) : format === "relative" ? formatRelative(date, now) : absolute;
  return (
    <time dateTime={date.toISOString()} title={absolute} className={className}>
      {text}
    </time>
  );
}
