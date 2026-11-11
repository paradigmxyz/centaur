import { afterEach, describe, expect, it, vi } from "vitest";

import { registerPwa, SERVICE_WORKER_URL } from "./pwa";

afterEach(() => {
  vi.restoreAllMocks();
  delete window.ConsoleBadge;
});

function fakeWindow(navigatorOverrides: Record<string, unknown>, readyState = "complete") {
  const listeners: Record<string, () => void> = {};
  return {
    win: {
      navigator: navigatorOverrides,
      document: { readyState },
      addEventListener: (name: string, fn: () => void) => {
        listeners[name] = fn;
      },
    } as unknown as Window,
    listeners,
  };
}

describe("registerPwa", () => {
  it("registers the service worker at the root scope once the page has loaded", () => {
    const register = vi.fn().mockResolvedValue(undefined);
    const { win, listeners } = fakeWindow({ serviceWorker: { register } }, "loading");
    registerPwa(win);
    expect(register).not.toHaveBeenCalled();
    listeners.load?.();
    expect(register).toHaveBeenCalledWith(SERVICE_WORKER_URL, { scope: "/" });
  });

  it("asks for persistent storage and tolerates browsers without a service worker", () => {
    const persist = vi.fn().mockResolvedValue(true);
    const { win } = fakeWindow({ storage: { persist } });
    expect(() => registerPwa(win)).not.toThrow();
    expect(persist).toHaveBeenCalled();
  });

  it("exposes the dock badge helpers", () => {
    const setAppBadge = vi.fn().mockResolvedValue(undefined);
    const clearAppBadge = vi.fn().mockResolvedValue(undefined);
    const { win } = fakeWindow({ setAppBadge, clearAppBadge });
    registerPwa(win);

    win.ConsoleBadge?.set(3);
    expect(setAppBadge).toHaveBeenCalledWith(3);
    win.ConsoleBadge?.set(0);
    win.ConsoleBadge?.clear();
    expect(clearAppBadge).toHaveBeenCalledTimes(2);
  });

  it("badge helpers are no-ops without the Badging API", () => {
    const { win } = fakeWindow({});
    registerPwa(win);
    expect(() => {
      win.ConsoleBadge?.set(2);
      win.ConsoleBadge?.clear();
    }).not.toThrow();
  });
});
