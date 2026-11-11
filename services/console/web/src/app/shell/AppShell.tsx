import { Suspense, useEffect, useMemo, useRef, useState } from "react";
import { Navigate, Outlet, useLocation } from "react-router";
import { toast } from "sonner";

import { features } from "@/app/registry";
import { useSession } from "@/app/session";
import { ShellContext } from "@/app/shell-context";
import { AccountMenu } from "@/app/shell/AccountMenu";
import { DescopeBanner } from "@/app/shell/DescopeBanner";
import { visibleControlTabs, visibleNav } from "@/app/shell/nav";
import { Sidebar } from "@/app/shell/Sidebar";
import { isSpaPath } from "@/app/spa-paths";
import { useMediaQuery } from "@/lib/use-media-query";
import lockup from "@/assets/centaur-lockup-white.svg";
import { Menu } from "lucide-react";
import { Link } from "react-router";

function ShellLoading() {
  return <div className="grid h-full place-items-center text-sm text-muted-foreground">Loading…</div>;
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
  const mobile = useMediaQuery("(max-width: 768px)");
  const [navOpen, setNavOpen] = useState(false);

  const nav = useMemo(() => visibleNav(features, me), [me]);
  const controlTabs = useMemo(() => visibleControlTabs(features, me), [me]);
  const shell = useMemo(() => ({ controlTabs, isSpaPath }), [controlTabs]);

  useFlashToasts(me.flash);

  // Close the drawer after every navigation (adjusting state during render)
  // and on Escape.
  const [lastPath, setLastPath] = useState(location.pathname);
  if (lastPath !== location.pathname) {
    setLastPath(location.pathname);
    setNavOpen(false);
  }
  useEffect(() => {
    if (!navOpen) return;
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && setNavOpen(false);
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [navOpen]);

  // An account awaiting approval may only see the holding page; send it there
  // instead of rendering pages whose API calls would all be refused.
  if (me.user.status === "pending") return <Navigate to="/pending" replace />;

  return (
    <ShellContext.Provider value={shell}>
      <div className="flex h-full flex-col overflow-hidden bg-background">
        {me.descoped && <DescopeBanner />}
        <div className="flex min-h-0 flex-1 overflow-hidden" data-nav-open={navOpen || undefined}>
          <Sidebar
            nav={nav}
            pathname={location.pathname}
            mobile={mobile}
            open={navOpen}
            footer={<AccountMenu me={me} />}
          />
          {mobile && (
            <div
              aria-hidden="true"
              onClick={() => setNavOpen(false)}
              className={`fixed inset-0 z-40 bg-black/55 transition-opacity duration-200 ${
                navOpen ? "opacity-100" : "pointer-events-none opacity-0"
              }`}
            />
          )}
          <main className="flex h-full min-h-0 min-w-0 flex-1 flex-col overflow-y-auto">
            {mobile && (
              <div className="sticky top-0 z-30 flex h-13 flex-none items-center gap-3 border-b border-sidebar-border bg-sidebar/90 px-3 backdrop-blur">
                <button
                  type="button"
                  aria-label="Open navigation"
                  aria-expanded={navOpen}
                  onClick={() => setNavOpen((open) => !open)}
                  className="grid size-9 cursor-pointer place-items-center rounded-[0.625rem] text-foreground hover:bg-sidebar-accent"
                >
                  <Menu className="size-5" />
                </button>
                <Link to="/" aria-label="Centaur Console" className="flex min-w-0 items-center">
                  <img src={lockup} alt="Centaur" width={497} height={127} className="glow logo-themed h-[1.4rem] w-auto" />
                </Link>
              </div>
            )}
            <div className="mx-auto w-full max-w-[80rem] px-4 pt-4 pb-6 md:px-8 md:pt-[1.35rem] md:pb-10">
              <Suspense fallback={<p className="text-sm text-muted-foreground">Loading…</p>}>
                <Outlet />
              </Suspense>
            </div>
          </main>
        </div>
      </div>
    </ShellContext.Provider>
  );
}

/** Shows a redirect flash handed off through GET /api/v1/me exactly once. */
function useFlashToasts(flash: { notice?: string; alert?: string }) {
  const shown = useRef<object | null>(null);
  useEffect(() => {
    if (shown.current === flash) return;
    shown.current = flash;
    if (flash.notice) toast.success(flash.notice);
    if (flash.alert) toast.error(flash.alert);
  }, [flash]);
}
