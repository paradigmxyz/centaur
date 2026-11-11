import { useMutation, useQueryClient } from "@tanstack/react-query";
import { LogOut, Monitor, Moon, Sun, UserCircle } from "lucide-react";
import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";

import { defaultLandingPath, type Me } from "@/app/session";
import { useShell } from "@/app/shell-context";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
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
  const { isSpaPath } = useShell();
  const { preference, setPreference } = useTheme();
  const installPrompt = useInstallPrompt();

  const descope = useMutation({
    mutationFn: () => api.post("/api/v1/descope"),
    onSuccess: async () => {
      // Drop admin data cached under the old permissions.
      queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== sharedKeys.me[0] });
      const fresh = await queryClient.fetchQuery({ queryKey: sharedKeys.me, queryFn: () => api.get<{ data: Me }>("/api/v1/me").then((b) => b.data) });
      const target = defaultLandingPath(fresh);
      if (isSpaPath(target)) navigate(target);
      else navigation.assign(target);
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
    <div className="relative border-t border-sidebar-border">
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            className="flex h-[3.625rem] w-full min-w-0 cursor-pointer items-center gap-2.5 px-6 text-left transition-colors hover:bg-sidebar-accent data-[state=open]:bg-sidebar-accent"
          >
            <span className="grid size-7 flex-none place-items-center rounded-full bg-centaur-500/15 text-xs font-bold text-centaur-200 light:bg-[#dff2e7] light:text-[#12733f]">
              {initial}
            </span>
            <span className="min-w-0 truncate text-xs text-muted-foreground" title={me.user.email}>
              {me.user.email}
            </span>
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent side="top" align="start" className="w-[var(--radix-dropdown-menu-trigger-width)] min-w-56 p-2">
          <DropdownMenuLabel className="truncate text-xs font-normal text-muted-foreground" title={me.user.email}>
            {me.user.email}
          </DropdownMenuLabel>
          <DropdownMenuLabel className="text-xs font-normal text-subtle-foreground">Centaur Console</DropdownMenuLabel>
          <DropdownMenuSeparator />
          {installPrompt && (
            <DropdownMenuItem
              title="Install Centaur Console as a standalone app"
              onSelect={() => void installPrompt.prompt()}
            >
              <Monitor className="size-4" /> Install app
            </DropdownMenuItem>
          )}
          <DropdownMenuItem
            aria-label={`Current theme: ${preference}. Switch to ${next} mode`}
            onSelect={(event) => {
              event.preventDefault();
              setPreference(next);
            }}
          >
            <ThemeIcon className="size-4" /> {THEME_LABELS[preference]}
          </DropdownMenuItem>
          {me.acting_admin && me.auth_method === "session" && (
            <DropdownMenuItem title="Temporarily pause your admin permissions" onSelect={() => descope.mutate()}>
              <UserCircle className="size-4" /> View as operator
            </DropdownMenuItem>
          )}
          <DropdownMenuItem onSelect={() => signOut.mutate()}>
            <LogOut className="size-4" /> Sign out
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
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
