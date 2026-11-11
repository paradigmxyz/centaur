import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/** Red error box (legacy .alert-error). */
export function ErrorAlert({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      role="alert"
      className={cn(
        "mb-6 rounded-sm border border-red-500/40 bg-red-500/[0.08] px-4 py-3 text-sm text-red-300 light:text-red-700",
        className,
      )}
    >
      {children}
    </div>
  );
}

/** Amber warning box (catalog loading, configuration hints). */
export function WarningAlert({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      role="status"
      className={cn(
        "rounded-sm border border-amber-500/30 bg-amber-500/[0.08] px-3 py-2 text-sm text-amber-200 light:border-amber-700 light:bg-amber-500 light:font-semibold light:text-amber-950",
        className,
      )}
    >
      {children}
    </div>
  );
}
