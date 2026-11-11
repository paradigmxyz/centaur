import { StatusBadge } from "@/components/badges";

/** Uppercase provider pill (legacy rounded bg-ink-700 chip). */
export function ProviderPill({ provider }: { provider: string }) {
  return (
    <span className="rounded-sm bg-muted px-2 py-0.5 text-[11px] tracking-wide text-foreground uppercase">{provider}</span>
  );
}

export function EnabledBadge({ enabled }: { enabled: boolean }) {
  return <StatusBadge tone={enabled ? "success" : "neutral"}>{enabled ? "enabled" : "disabled"}</StatusBadge>;
}
