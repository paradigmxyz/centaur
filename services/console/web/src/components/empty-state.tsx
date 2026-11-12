import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

export function EmptyState({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={cn(
        "rounded-xl border border-dashed bg-muted/20 px-5 py-8 text-sm text-muted-foreground",
        className,
      )}
    >
      {children}
    </div>
  );
}
