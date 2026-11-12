import { Menu } from "lucide-react";
import { Suspense, useEffect, useMemo, useRef } from "react";
import { Link, Outlet, useLocation } from "react-router";
import { toast } from "sonner";

import { features } from "@/app/registry";
import { useSession } from "@/app/session";
import { ShellContext } from "@/app/shell-context";
import { AccountMenu } from "@/app/shell/AccountMenu";
import { visibleControlTabs, visibleNav } from "@/app/shell/nav";
import { ConsoleSidebar } from "@/app/shell/Sidebar";
import { isSpaPath } from "@/app/spa-paths";
import { Button } from "@/components/ui/button";
import { SidebarInset, SidebarProvider, useSidebar } from "@/components/ui/sidebar";
import { Skeleton } from "@/components/ui/skeleton";
import { navigation } from "@/lib/api";
import lockup from "@/assets/centaur-lockup-white.svg";

export function ShellLoading() {
  return (
    <div role="status" className="grid min-h-svh place-items-center text-sm text-muted-foreground">
      Loading…
    </div>
  );
}

function PageLoading() {
  return (
    <div role="status" className="space-y-6">
      <span className="sr-only">Loading…</span>
      <div className="space-y-2" aria-hidden="true">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-4 w-72" />
      </div>
      <Skeleton className="h-64 w-full rounded-xl" aria-hidden="true" />
    </div>
  );
}

export function AppShell() {
  return (
    <Suspense fallback={<ShellLoading />}>
      <ShellFrame />
    </Suspense>
  );
}

function ShellFrame() {
  const me = useSession();
  const location = useLocation();

  const nav = useMemo(() => visibleNav(features, me), [me]);
  const controlTabs = useMemo(() => visibleControlTabs(features, me), [me]);
  const shell = useMemo(() => ({ controlTabs, isSpaPath }), [controlTabs]);

  useFlashToasts(me.flash);

  // A pending account may only see the server-rendered /pending page.
  if (me.user.status === "pending") return <FullPageRedirect to="/pending" />;

  return (
    <ShellContext.Provider value={shell}>
      <SidebarProvider>
        <ConsoleSidebar nav={nav} pathname={location.pathname} footer={<AccountMenu me={me} />} />
        <CloseDrawerOnNavigate pathname={location.pathname} />
        <SidebarInset className="min-w-0">
          <MobileTopBar />
          <div className="mx-auto w-full max-w-[80rem] px-4 pt-5 pb-10 md:px-8 md:pt-8">
            <Suspense fallback={<PageLoading />}>
              <Outlet />
            </Suspense>
          </div>
        </SidebarInset>
      </SidebarProvider>
    </ShellContext.Provider>
  );
}

/** Leaves the SPA for a Rails-rendered page (login, pending). */
function FullPageRedirect({ to }: { to: string }) {
  useEffect(() => navigation.assign(to), [to]);
  return <ShellLoading />;
}

function CloseDrawerOnNavigate({ pathname }: { pathname: string }) {
  const { setOpenMobile } = useSidebar();
  const last = useRef(pathname);
  useEffect(() => {
    if (last.current === pathname) return;
    last.current = pathname;
    setOpenMobile(false);
  }, [pathname, setOpenMobile]);
  return null;
}

function MobileTopBar() {
  const { isMobile, openMobile, setOpenMobile } = useSidebar();
  if (!isMobile) return null;
  return (
    <div className="sticky top-0 z-30 flex h-14 flex-none items-center gap-2 border-b bg-background/85 px-3 backdrop-blur">
      <Button
        type="button"
        variant="ghost"
        size="icon"
        aria-label="Open navigation"
        aria-expanded={openMobile}
        onClick={() => setOpenMobile(!openMobile)}
      >
        <Menu className="size-5" />
      </Button>
      <Link
        to="/"
        aria-label="Centaur Console"
        className="flex min-w-0 items-center rounded-md p-1 outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
      >
        <img src={lockup} alt="Centaur" width={497} height={127} className="logo-themed h-5 w-auto" />
      </Link>
    </div>
  );
}

/** Shows a redirect flash handed off through GET /api/v1/console/me exactly once. */
function useFlashToasts(flash: { notice?: string; alert?: string }) {
  const shown = useRef<object | null>(null);
  useEffect(() => {
    if (shown.current === flash) return;
    shown.current = flash;
    if (flash.notice) toast.success(flash.notice);
    if (flash.alert) toast.error(flash.alert);
  }, [flash]);
}
