import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ChevronsUpDown, LogOut, Monitor, Moon, Sun, UserCircle } from "lucide-react";
import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";

import { defaultLandingPath, type Me } from "@/app/session";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { SidebarMenu, SidebarMenuItem } from "@/components/ui/sidebar";
import { api, errorMessage, navigation } from "@/lib/api";
import { sharedKeys } from "@/lib/query-keys";
import { nextPreference, useTheme, type ThemePreference } from "@/lib/theme";

const THEME_LABELS: Record<ThemePreference, string> = {
  system: "System mode",
  light: "Light mode",
  dark: "Dark mode",
};

interface InstallPromptEvent extends Event {
  prompt: () => Promise<void>;
}

export function AccountMenu({ me }: { me: Me }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const { preference, setPreference } = useTheme();
  const installPrompt = useInstallPrompt();

  const descope = useMutation({
    mutationFn: () => api.post("/api/v1/descope"),
    onSuccess: async () => {
      // Drop admin data cached under the old permissions.
      queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== sharedKeys.me[0] });
      const fresh = await queryClient.fetchQuery({ queryKey: sharedKeys.me, queryFn: () => api.get<{ data: Me }>("/api/v1/me").then((b) => b.data) });
      navigate(defaultLandingPath(fresh));
    },
    onError: (error) => toast.error(errorMessage(error)),
  });

  const signOut = useMutation({
    mutationFn: () => api.delete("/api/v1/session"),
    onSuccess: () => {
      queryClient.clear();
      navigation.assign("/login");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });

  const next = nextPreference(preference);
  const ThemeIcon = preference === "light" ? Sun : preference === "dark" ? Moon : Monitor;
  const initial = me.user.email.charAt(0).toUpperCase() || "?";

  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              className="flex h-12 w-full min-w-0 cursor-pointer items-center gap-2.5 rounded-md px-2 text-left outline-none transition-colors hover:bg-sidebar-accent focus-visible:ring-[3px] focus-visible:ring-sidebar-ring/50 data-[state=open]:bg-sidebar-accent"
            >
              <span className="grid size-8 flex-none place-items-center rounded-lg bg-primary/15 text-xs font-semibold text-primary">
                {initial}
              </span>
              <span className="min-w-0 flex-1 truncate text-sm text-sidebar-foreground" title={me.user.email}>
                {me.user.email}
              </span>
              <ChevronsUpDown className="ml-auto size-4 text-muted-foreground" aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent side="top" align="start" className="w-[var(--radix-dropdown-menu-trigger-width)] min-w-56">
            <DropdownMenuLabel className="flex flex-col gap-0.5 font-normal">
              <span className="truncate text-sm font-medium" title={me.user.email}>
                {me.user.email}
              </span>
              <span className="text-xs text-muted-foreground">Centaur Console</span>
            </DropdownMenuLabel>
            <DropdownMenuSeparator />
            {installPrompt && (
              <DropdownMenuItem
                title="Install Centaur Console as a standalone app"
                onSelect={() => void installPrompt.prompt()}
              >
                <Monitor /> Install app
              </DropdownMenuItem>
            )}
            <DropdownMenuItem
              aria-label={`Current theme: ${preference}. Switch to ${next} mode`}
              onSelect={(event) => {
                event.preventDefault();
                setPreference(next);
              }}
            >
              <ThemeIcon /> {THEME_LABELS[preference]}
            </DropdownMenuItem>
            {me.acting_admin && me.auth_method === "session" && (
              <DropdownMenuItem title="Temporarily pause your admin permissions" onSelect={() => descope.mutate()}>
                <UserCircle /> View as operator
              </DropdownMenuItem>
            )}
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={() => signOut.mutate()}>
              <LogOut /> Sign out
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </SidebarMenuItem>
    </SidebarMenu>
  );
}

/** The deferred PWA install prompt, once the browser reports the app installable. */
function useInstallPrompt(): InstallPromptEvent | null {
  const [event, setEvent] = useState<InstallPromptEvent | null>(null);
  useEffect(() => {
    const onPrompt = (e: Event) => {
      e.preventDefault();
      setEvent(e as InstallPromptEvent);
    };
    const onInstalled = () => setEvent(null);
    window.addEventListener("beforeinstallprompt", onPrompt);
    window.addEventListener("appinstalled", onInstalled);
    return () => {
      window.removeEventListener("beforeinstallprompt", onPrompt);
      window.removeEventListener("appinstalled", onInstalled);
    };
  }, []);
  return event;
}
