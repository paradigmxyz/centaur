import { CircleAlert, TriangleAlert } from "lucide-react";
import type { ReactNode } from "react";

import { Alert } from "@/components/ui/alert";
import { cn } from "@/lib/utils";

// The icon sits in the left gutter so `className` can lay out the children
// directly (e.g. a message with a Retry button on the right).
const gutter = "block pl-10 [&>svg]:absolute [&>svg]:top-3.5 [&>svg]:left-4 [&>svg]:size-4";

/** Red error box. */
export function ErrorAlert({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <Alert
      variant="destructive"
      className={cn(gutter, "mb-6 border-destructive/30 bg-destructive/10 text-destructive", className)}
    >
      <CircleAlert aria-hidden="true" />
      {children}
    </Alert>
  );
}

/** Amber warning box (catalog loading, configuration hints). */
export function WarningAlert({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <Alert role="status" className={cn(gutter, "border-warning/30 bg-warning/10 text-warning", className)}>
      <TriangleAlert aria-hidden="true" />
      {children}
    </Alert>
  );
}
