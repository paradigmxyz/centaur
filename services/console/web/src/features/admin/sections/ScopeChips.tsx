import type { ReactNode } from "react";

import { Badge } from "@/components/ui/badge";

/** A plain chip (legacy .label-chip): scopes, the "default" principal marker. */
export function Chip({ children }: { children: ReactNode }) {
  return (
    <Badge variant="outline" className="rounded-md border-primary/25 bg-primary/10 font-mono text-[11px] text-primary">
      {children}
    </Badge>
  );
}

/** Scope names as chips. */
export function ScopeChips({ scopes }: { scopes: string[] }) {
  return (
    <div className="flex flex-wrap gap-1">
      {scopes.map((scope) => (
        <Chip key={scope}>{scope}</Chip>
      ))}
    </div>
  );
}
