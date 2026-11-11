import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/** Dashed placeholder box for empty sections. */
export function EmptyState({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={cn(
        "rounded-sm border border-dashed border-border bg-muted/40 px-5 py-6 text-sm text-subtle-foreground",
        className,
      )}
    >
      {children}
    </div>
  );
}
