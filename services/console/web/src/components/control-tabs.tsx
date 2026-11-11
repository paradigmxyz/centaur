import { useLocation } from "react-router";

import { useShell } from "@/app/shell-context";
import { AppLink } from "@/components/app-link";
import { cn } from "@/lib/utils";

/** The Control section tab strip. Render it at the top of Control index pages. */
export function ControlTabs() {
  const { controlTabs } = useShell();
  const { pathname } = useLocation();
  return (
    <div className="-mx-4 mb-6 overflow-x-auto px-4 [scrollbar-width:none] md:mx-0 md:px-0">
      <nav
        aria-label="Control sections"
        className="inline-flex h-9 w-max items-center rounded-lg bg-muted p-[3px] text-muted-foreground"
      >
        {controlTabs.map((tab, index) => {
          const match = tab.match ?? tab.to;
          const active = pathname === match || pathname.startsWith(`${match}/`) || (index === 0 && pathname === "/");
          return (
            <AppLink
              key={tab.id}
              to={tab.to}
              aria-current={active ? "page" : undefined}
              className={cn(
                "inline-flex h-full flex-none items-center rounded-md border border-transparent px-3 text-sm font-medium whitespace-nowrap transition-[color,box-shadow] outline-none hover:text-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50",
                active && "bg-background text-foreground shadow-sm dark:border-input dark:bg-input/30",
              )}
            >
              {tab.label}
            </AppLink>
          );
        })}
      </nav>
    </div>
  );
}
