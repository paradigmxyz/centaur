import type { ReactNode } from "react";
import { Link } from "react-router";

import { isActivePath } from "@/app/shell/nav";
import type { NavEntry } from "@/app/types";
import { AppLink } from "@/components/app-link";
import { cn } from "@/lib/utils";
import lockup from "@/assets/centaur-lockup-white.svg";

interface SidebarProps {
  nav: NavEntry[];
  pathname: string;
  mobile: boolean;
  open: boolean;
  footer: ReactNode;
}

export function Sidebar({ nav, pathname, mobile, open, footer }: SidebarProps) {
  const primary = nav.filter((entry) => entry.placement === "primary");
  const groups = nav.filter((entry) => entry.placement === "group");

  return (
    <aside
      aria-label="Console navigation"
      // Off-canvas and closed: keep the drawer out of the tab order.
      inert={mobile && !open ? true : undefined}
      className={cn(
        "flex min-h-0 w-72 flex-none flex-col border-r border-sidebar-border bg-sidebar",
        mobile &&
          "fixed inset-y-0 left-0 z-50 w-68 max-w-[82vw] -translate-x-full transition-transform duration-200",
        mobile && open && "translate-x-0 shadow-[0_24px_60px_rgba(0,0,0,0.5)]",
      )}
    >
      <div className="flex h-14 items-center gap-3 px-6">
        <Link to="/" aria-label="Centaur Console" className="flex min-w-0 flex-1 items-center text-heading">
          <img src={lockup} alt="Centaur" width={497} height={127} className="glow logo-themed h-7 w-auto" />
        </Link>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-3 pt-1 pb-4">
        <nav aria-label="Console sections" className="mb-0.5 grid gap-0.5">
          {primary.map((entry) => (
            <NavItem key={entry.id} entry={entry} pathname={pathname} />
          ))}
        </nav>
        {groups.map((entry) => (
          <div key={entry.id} className="mt-0.5">
            <NavItem entry={entry} pathname={pathname} />
          </div>
        ))}
      </div>

      {footer}
    </aside>
  );
}

function NavItem({ entry, pathname }: { entry: NavEntry; pathname: string }) {
  const active = isActivePath(pathname, entry.match ?? [entry.to]) || (entry.id === "control" && pathname === "/");
  const Icon = entry.icon;
  return (
    <AppLink
      to={entry.to}
      title={entry.label}
      aria-current={active ? "page" : undefined}
      className={cn(
        "flex h-[2.35rem] min-w-0 items-center gap-2.5 rounded-[0.875rem] px-3.5 text-sidebar-foreground no-underline transition-colors hover:bg-sidebar-accent hover:text-heading",
        active && "bg-sidebar-active text-sidebar-active-foreground hover:bg-sidebar-active hover:text-sidebar-active-foreground",
      )}
    >
      <span className="grid w-4 flex-none place-items-center">
        <Icon className="size-4" aria-hidden="true" />
      </span>
      <span className="min-w-0 truncate text-sm font-medium">{entry.label}</span>
    </AppLink>
  );
}
