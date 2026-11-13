import { StatusBadge } from "@/components/badges";
import { Badge } from "@/components/ui/badge";

export function ProviderPill({ provider }: { provider: string }) {
  return (
    <Badge variant="outline" className="rounded-md bg-muted/50 font-mono text-[11px] font-normal text-muted-foreground">
      {provider}
    </Badge>
  );
}

export function EnabledBadge({ enabled }: { enabled: boolean }) {
  return <StatusBadge tone={enabled ? "success" : "neutral"}>{enabled ? "enabled" : "disabled"}</StatusBadge>;
}
