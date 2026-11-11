import { useLocation } from "react-router";

import { useShell } from "@/app/shell-context";
import { AppLink } from "@/components/app-link";
import { cn } from "@/lib/utils";

/** The Control section tab strip. Render it at the top of Control index pages. */
export function ControlTabs() {
  const { controlTabs } = useShell();
  const { pathname } = useLocation();
  return (
    <nav
      aria-label="Control sections"
      className="mb-6 flex gap-1 overflow-x-auto border-b border-border [scrollbar-width:none]"
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
              "-mb-px flex-none border-b border-transparent px-3 py-2.5 text-sm font-medium whitespace-nowrap text-zinc-400 transition-colors hover:text-heading light:text-[#6b7078]",
              active && "border-primary text-centaur-400 light:text-primary",
            )}
          >
            {tab.label}
          </AppLink>
        );
      })}
    </nav>
  );
}
