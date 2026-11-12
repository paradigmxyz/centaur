import { ArrowLeft } from "lucide-react";

import { AppLink } from "@/components/app-link";

export function BackLink({ to, children }: { to: string; children: string }) {
  return (
    <AppLink
      to={to}
      className="group inline-flex items-center gap-1.5 rounded-sm text-sm text-muted-foreground transition-colors hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
    >
      <ArrowLeft className="size-3.5 transition-transform group-hover:-translate-x-0.5" aria-hidden="true" />
      <span>
        <span className="sr-only">←</span> {children}
      </span>
    </AppLink>
  );
}
