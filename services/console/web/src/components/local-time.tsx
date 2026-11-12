import { useEffect, useState } from "react";

import { cn } from "@/lib/utils";
import { formatAbsolute, formatCompact, formatRelative } from "@/lib/format";

interface LocalTimeProps {
  value: string | null | undefined;
  /** "absolute" (default): MM/DD/YYYY HH:MM:SS; "relative": "5 minutes ago"; "compact": "4d". */
  format?: "absolute" | "relative" | "compact";
  className?: string;
}

export function LocalTime({ value, format = "absolute", className }: LocalTimeProps) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (format === "absolute") return;
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, [format]);

  if (!value) return <span className="text-muted-foreground">—</span>;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return <span className={className}>{value}</span>;

  const absolute = formatAbsolute(date);
  const text =
    format === "compact" ? formatCompact(date, now) : format === "relative" ? formatRelative(date, now) : absolute;
  return (
    <time dateTime={date.toISOString()} title={absolute} className={cn("tabular-nums", className)}>
      {text}
    </time>
  );
}
