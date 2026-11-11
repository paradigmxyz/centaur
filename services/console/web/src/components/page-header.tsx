import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { useEffect } from "react";

import { cn } from "@/lib/utils";

interface PageHeaderProps {
  title: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
  icon?: LucideIcon;
  /** Document title; defaults to `title` when it is a string. */
  documentTitle?: string;
  className?: string;
}

/** Ported from console/_page_header: title + subtitle with actions on the right. */
export function PageHeader({ title, subtitle, actions, icon: Icon, documentTitle, className }: PageHeaderProps) {
  useDocumentTitle(documentTitle ?? (typeof title === "string" ? title : undefined));
  return (
    <header
      className={cn(
        "mb-6 flex min-h-11 flex-col gap-3 sm:flex-row sm:items-start sm:justify-between sm:gap-4",
        className,
      )}
    >
      <div className="flex min-w-0 items-start gap-3">
        {Icon && (
          <span className="mt-0.5 grid size-7 shrink-0 place-items-center rounded-sm bg-muted text-muted-foreground">
            <Icon className="size-4" aria-hidden="true" />
          </span>
        )}
        <div className="min-w-0">
          <h1 className="text-2xl font-bold text-heading">{title}</h1>
          {subtitle && <div className="mt-1 text-sm text-muted-foreground">{subtitle}</div>}
        </div>
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </header>
  );
}

/** Sets "<page> · Centaur Console" as the document title. */
export function useDocumentTitle(title: string | undefined) {
  useEffect(() => {
    document.title = title ? `${title} · Centaur Console` : "Centaur Console";
  }, [title]);
}

/** Section heading used inside pages ("ROLES", "DIRECT GRANTS", ...). */
export function SectionHeading({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <h2 className={cn("mb-1 text-xs font-semibold tracking-wider text-muted-foreground uppercase", className)}>
      {children}
    </h2>
  );
}
