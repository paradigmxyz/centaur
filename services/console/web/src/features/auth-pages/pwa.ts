// Progressive web app bootstrap, run once from main.tsx (port of the legacy
// app/javascript/application.js):
// - registers the service worker (/service-worker.js, scope "/"): offline
//   fallback page and a cache for content-hashed assets; it never caches the
//   SPA shell or the API. Registration needs a secure context (https or
//   localhost), so it silently no-ops on plain-http dev setups.
// - asks the browser to keep this origin's storage under disk pressure.
// - exposes window.ConsoleBadge for the installed app's dock badge.
// The manifest is linked from index.html (/manifest.json).

export interface ConsoleBadge {
  set(count: number): void;
  clear(): void;
}

declare global {
  interface Window {
    ConsoleBadge?: ConsoleBadge;
  }
}

type BadgingNavigator = Navigator & {
  setAppBadge?: (count?: number) => Promise<void>;
  clearAppBadge?: () => Promise<void>;
};

export const SERVICE_WORKER_URL = "/service-worker.js";

export function registerPwa(win: Window = window) {
  const nav = win.navigator as BadgingNavigator;

  if ("serviceWorker" in nav) {
    const register = () => {
      nav.serviceWorker.register(SERVICE_WORKER_URL, { scope: "/" }).catch(() => {});
    };
    if (win.document.readyState === "complete") register();
    else win.addEventListener("load", register, { once: true });
  }

  nav.storage?.persist?.().catch(() => {});

  win.ConsoleBadge = {
    set(count: number) {
      if (!nav.setAppBadge || !nav.clearAppBadge) return;
      (count > 0 ? nav.setAppBadge(count) : nav.clearAppBadge()).catch(() => {});
    },
    clear() {
      nav.clearAppBadge?.().catch(() => {});
    },
  };
}
