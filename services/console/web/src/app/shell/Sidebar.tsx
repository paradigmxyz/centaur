import type { ReactNode } from "react";
import { Link } from "react-router";

import { isActivePath } from "@/app/shell/nav";
import type { NavEntry } from "@/app/types";
import { AppLink } from "@/components/app-link";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from "@/components/ui/sidebar";
import lockup from "@/assets/centaur-lockup-white.svg";

interface ConsoleSidebarProps {
  nav: NavEntry[];
  pathname: string;
  footer: ReactNode;
}

/** The console sidebar: logo, section links, and the account menu. Off-canvas (a Sheet) on mobile. */
export function ConsoleSidebar({ nav, pathname, footer }: ConsoleSidebarProps) {
  return (
    <Sidebar collapsible="offcanvas">
      <SidebarHeader className="h-14 justify-center px-4">
        <Link
          to="/"
          aria-label="Centaur Console"
          className="flex w-fit items-center rounded-md p-1 outline-none focus-visible:ring-[3px] focus-visible:ring-sidebar-ring/50"
        >
          <img src={lockup} alt="Centaur" width={497} height={127} className="logo-themed h-6 w-auto" />
        </Link>
      </SidebarHeader>

      <SidebarContent>
        <SidebarGroup className="px-3 py-2">
          <nav aria-label="Console sections">
            <SidebarMenu>
              {nav.map((entry) => (
                <NavItem key={entry.id} entry={entry} pathname={pathname} />
              ))}
            </SidebarMenu>
          </nav>
        </SidebarGroup>
      </SidebarContent>

      <SidebarFooter className="border-t border-sidebar-border p-3">{footer}</SidebarFooter>
    </Sidebar>
  );
}

function NavItem({ entry, pathname }: { entry: NavEntry; pathname: string }) {
  const active = isActivePath(pathname, entry.match ?? [entry.to]) || (entry.id === "control" && pathname === "/");
  const Icon = entry.icon;
  return (
    <SidebarMenuItem>
      <SidebarMenuButton
        asChild
        isActive={active}
        className="h-9 gap-2.5 px-3 font-medium text-sidebar-foreground focus-visible:ring-[3px] focus-visible:ring-sidebar-ring/50 data-[active=true]:bg-primary/10 data-[active=true]:text-primary data-[active=true]:hover:bg-primary/15 data-[active=true]:hover:text-primary"
      >
        <AppLink to={entry.to} title={entry.label} aria-current={active ? "page" : undefined}>
          <Icon aria-hidden="true" />
          <span>{entry.label}</span>
        </AppLink>
      </SidebarMenuButton>
    </SidebarMenuItem>
  );
}
