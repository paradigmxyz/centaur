// The service worker never caches the SPA shell or the API. Registration needs
// a secure context, so it no-ops on plain-http dev setups.

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
