// Theme preference, compatible with the legacy console's localStorage keys and
// the inline bootstrap script in index.html (which applies the theme before
// first paint). Preference cycles system -> light -> dark.
import { useSyncExternalStore } from "react";

export type ThemePreference = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

const STORAGE_KEY = "centaur-console-theme";
const SOURCE_KEY = "centaur-console-theme-source";
const listeners = new Set<() => void>();

function systemTheme(): ResolvedTheme {
  return window.matchMedia?.("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

export function getPreference(): ThemePreference {
  try {
    const theme = localStorage.getItem(STORAGE_KEY);
    const source = localStorage.getItem(SOURCE_KEY);
    if (source === "manual" && (theme === "light" || theme === "dark")) return theme;
  } catch {
    // Storage unavailable: follow the system.
  }
  return "system";
}

export function resolveTheme(preference = getPreference()): ResolvedTheme {
  return preference === "system" ? systemTheme() : preference;
}

export function applyTheme(preference = getPreference()) {
  const theme = resolveTheme(preference);
  document.documentElement.dataset.consoleTheme = theme;
  document.documentElement.classList.toggle("dark", theme === "dark");
  const favicon = document.querySelector<HTMLLinkElement>("link[data-console-favicon]");
  favicon?.setAttribute("href", theme === "light" ? "/icon-light.svg" : "/icon-dark.svg");
  listeners.forEach((listener) => listener());
}

export function setPreference(preference: ThemePreference) {
  try {
    if (preference === "system") {
      localStorage.removeItem(STORAGE_KEY);
      localStorage.removeItem(SOURCE_KEY);
    } else {
      localStorage.setItem(STORAGE_KEY, preference);
      localStorage.setItem(SOURCE_KEY, "manual");
    }
  } catch {
    // Still applies for this page load.
  }
  applyTheme(preference);
}

export function nextPreference(preference: ThemePreference): ThemePreference {
  const order: ThemePreference[] = ["system", "light", "dark"];
  return order[(order.indexOf(preference) + 1) % order.length] ?? "system";
}

export function watchSystemTheme() {
  const query = window.matchMedia?.("(prefers-color-scheme: light)");
  const sync = () => {
    if (getPreference() === "system") applyTheme("system");
  };
  query?.addEventListener?.("change", sync);
  return () => query?.removeEventListener?.("change", sync);
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Current preference and resolved theme; re-renders on change. */
export function useTheme() {
  const preference = useSyncExternalStore(subscribe, getPreference, () => "system" as ThemePreference);
  const resolved = useSyncExternalStore(subscribe, () => resolveTheme(), () => "dark" as ResolvedTheme);
  return { preference, resolved, setPreference };
}
