import type { ReactNode } from "react";

/** A plain chip (legacy .label-chip): scopes, the "default" principal marker. */
export function Chip({ children }: { children: ReactNode }) {
  return (
    <span className="rounded-sm bg-primary/10 px-1.5 py-0.5 text-xs text-centaur-300/90 ring-1 ring-primary/20 light:text-primary">
      {children}
    </span>
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
