import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/** Green resource badge (kind labels, "STATIC", "SLACK_DM"). */
export function ResourceBadge({ children, className, title }: { children: ReactNode; className?: string; title?: string }) {
  return (
    <span
      title={title}
      className={cn(
        "inline-block max-w-full truncate rounded-sm bg-primary/10 px-2 py-0.5 align-bottom text-xs font-medium text-centaur-300 ring-1 ring-primary/25 light:text-primary",
        className,
      )}
    >
      {children}
    </span>
  );
}

/** key=value label chip. */
export function LabelChip({ name, value }: { name: string; value: unknown }) {
  return (
    <span className="rounded-sm bg-primary/10 px-1.5 py-0.5 text-xs text-centaur-300/90 ring-1 ring-primary/20 light:text-primary">
      {name}={String(value)}
    </span>
  );
}

export function LabelChips({ labels }: { labels: Record<string, unknown> | null | undefined }) {
  const entries = Object.entries(labels ?? {});
  if (entries.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-1">
      {entries.map(([key, value]) => (
        <LabelChip key={key} name={key} value={value} />
      ))}
    </div>
  );
}

/** Sky-blue pill for role names. */
export function RolePill({ children, title }: { children: ReactNode; title?: string }) {
  return (
    <span
      title={title}
      className="inline-flex max-w-full truncate rounded-full bg-role-soft px-2 py-0.5 text-xs font-medium text-role ring-1 ring-sky-500/25"
    >
      {children}
    </span>
  );
}

const STATUS_TONES = {
  success: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25 light:text-emerald-700",
  danger: "bg-red-500/10 text-red-300 ring-red-500/25 light:text-red-700",
  warning: "bg-amber-500/10 text-amber-300 ring-amber-500/25 light:text-amber-700",
  info: "bg-sky-500/10 text-sky-300 ring-sky-500/25 light:text-sky-700",
  neutral: "bg-muted text-zinc-400 ring-border light:text-zinc-600",
  brand: "bg-primary/10 text-centaur-300 ring-primary/30 light:text-primary",
} as const;

export type StatusTone = keyof typeof STATUS_TONES;

/** Status pill. Map domain statuses to tones with the helpers below. */
export function StatusBadge({ tone, children, title }: { tone: StatusTone; children: ReactNode; title?: string }) {
  return (
    <span
      title={title}
      className={cn("inline-flex items-center rounded-sm px-2 py-0.5 text-[11px] font-medium ring-1", STATUS_TONES[tone])}
    >
      {children}
    </span>
  );
}

/** credential_status_classes: live / dead / bootstrapping. */
export function credentialStatusTone(status: string): StatusTone {
  if (status === "live") return "success";
  if (status === "dead") return "danger";
  return "warning";
}

/** workflow_status_classes. */
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

/** User account status (active / pending / disabled). */
export function userStatusTone(status: string): StatusTone {
  if (status === "active") return "success";
  if (status === "disabled") return "danger";
  return "warning";
}
