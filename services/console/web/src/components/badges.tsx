import type { ReactNode } from "react";

import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

export function ResourceBadge({ children, className, title }: { children: ReactNode; className?: string; title?: string }) {
  return (
    <Badge
      variant="outline"
      title={title}
      className={cn(
        "max-w-full justify-start rounded-md border-primary/25 bg-primary/10 font-mono text-[11px] text-primary",
        "[&>span]:truncate",
        className,
      )}
    >
      <span>{children}</span>
    </Badge>
  );
}

export function LabelChip({ name, value }: { name: string; value: unknown }) {
  return (
    <Badge variant="outline" className="rounded-md bg-muted/50 font-mono text-[11px] font-normal text-muted-foreground">
      {name}={String(value)}
    </Badge>
  );
}

export function LabelChips({ labels }: { labels: Record<string, unknown> }) {
  const entries = Object.entries(labels);
  if (entries.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-1">
      {entries.map(([key, value]) => (
        <LabelChip key={key} name={key} value={value} />
      ))}
    </div>
  );
}

export function RolePill({ children, title }: { children: ReactNode; title?: string }) {
  return (
    <Badge
      variant="outline"
      title={title}
      className="max-w-full justify-start border-info/25 bg-info/10 text-info [&>span]:truncate"
    >
      <span>{children}</span>
    </Badge>
  );
}

const STATUS_TONES = {
  success: "border-success/25 bg-success/10 text-success",
  danger: "border-destructive/25 bg-destructive/10 text-destructive",
  warning: "border-warning/25 bg-warning/10 text-warning",
  info: "border-info/25 bg-info/10 text-info",
  neutral: "border-border bg-muted text-muted-foreground",
  brand: "border-primary/25 bg-primary/10 text-primary",
} as const;

const STATUS_DOTS = {
  success: "bg-success",
  danger: "bg-destructive",
  warning: "bg-warning",
  info: "bg-info",
  neutral: "bg-muted-foreground",
  brand: "bg-primary",
} as const;

export type StatusTone = keyof typeof STATUS_TONES;

/** Status pill. Map domain statuses to tones with the helpers below. */
export function StatusBadge({ tone, children, title }: { tone: StatusTone; children: ReactNode; title?: string }) {
  return (
    <Badge variant="outline" title={title} className={cn("gap-1.5 text-[11px]", STATUS_TONES[tone])}>
      <span aria-hidden="true" className={cn("size-1.5 rounded-full", STATUS_DOTS[tone])} />
      {children}
    </Badge>
  );
}

export function credentialStatusTone(status: string): StatusTone {
  if (status === "live") return "success";
  if (status === "dead") return "danger";
  return "warning";
}

export function workflowStatusTone(status: string): StatusTone {
  switch (status) {
    case "completed":
      return "brand";
    case "running":
      return "info";
    case "failed":
      return "danger";
    case "pending":
    case "sleeping":
      return "warning";
    default:
      return "neutral";
  }
}

export function userStatusTone(status: string): StatusTone {
  if (status === "active") return "success";
  if (status === "disabled") return "danger";
  return "warning";
}
