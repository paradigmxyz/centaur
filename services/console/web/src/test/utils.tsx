// Test helpers for console pages and components.
//
//   const fetchMock = mockApi({ "GET /api/v1/principals": { data: [], meta } });
//   renderRoute(<PrincipalsListPage />, { path: "/console/principals", me: adminMe() });
//   await screen.findByText("No principals yet.");
//   expect(fetchMock.calls("POST /api/v1/grants")[0]?.body).toEqual({...});
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render } from "@testing-library/react";
import type { ReactElement } from "react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { vi } from "vitest";

import type { Me } from "@/app/session";
import { ShellContext } from "@/app/shell-context";
import type { ControlTab } from "@/app/types";
import { setCsrfToken } from "@/lib/api";
import { sharedKeys } from "@/lib/query-keys";

export function adminMe(overrides: Partial<Me> = {}): Me {
  return {
    user: { id: "usr_admin", email: "admin@example.test", name: null, status: "active", admin: true },
    auth_method: "session",
    acting_admin: true,
    descoped: false,
    csrf_token: "csrf-test-token",
    features: { api_clients: false, password_login: true },
    nav: {
      control: true,
      principals: true,
      roles: true,
      secrets: true,
      credentials: true,
      oauth_apps: true,
      users: true,
      api_clients: false,
      settings: true,
      workflows: true,
      integrations: true,
      skills: true,
      scheduled_tasks: true,
    },
    flash: {},
    ...overrides,
  };
}

export function operatorMe(overrides: Partial<Me> = {}): Me {
  const base = adminMe();
  return {
    ...base,
    user: { ...base.user, email: "operator@example.test", admin: false },
    acting_admin: false,
    nav: Object.fromEntries(Object.keys(base.nav).map((key) => [key, false])) as Me["nav"],
    ...overrides,
  } satisfies Me;
}

type Handler = unknown | ((request: { url: URL; body: unknown }) => unknown);

interface MockResponse {
  status: number;
  body: unknown;
}

/** A canned non-2xx response for mockApi handlers. */
export function apiResponse(status: number, body: unknown): MockResponse & { __mock: true } {
  return { status, body, __mock: true };
}

/**
 * Stubs global fetch. Keys are "METHOD /path" (query ignored). Handlers return
 * a JSON body (200), apiResponse(status, body), or undefined (204).
 */
export function mockApi(handlers: Record<string, Handler>) {
  const calls: { key: string; url: URL; body: unknown; headers: Record<string, string> }[] = [];
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    const method = (init?.method ?? "GET").toUpperCase();
    const key = `${method} ${url.pathname}`;
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : undefined;
    calls.push({ key, url, body, headers: (init?.headers ?? {}) as Record<string, string> });
    if (!(key in handlers)) return new Response(JSON.stringify({ error: { message: `unmocked ${key}` } }), { status: 404 });
    const handler = handlers[key];
    const result = typeof handler === "function" ? (handler as (r: { url: URL; body: unknown }) => unknown)({ url, body }) : handler;
    if (result === undefined) return new Response(null, { status: 204 });
    if (typeof result === "object" && result !== null && "__mock" in result) {
      const mock = result as unknown as MockResponse;
      return new Response(JSON.stringify(mock.body), { status: mock.status });
    }
    return new Response(JSON.stringify(result), { status: 200, headers: { "Content-Type": "application/json" } });
  });
  vi.stubGlobal("fetch", fetchMock);
  return {
    fetch: fetchMock,
    calls: (key?: string) => (key ? calls.filter((call) => call.key === key) : calls),
  };
}

interface RenderRouteOptions {
  /** Route pattern for the element, e.g. "/console/principals/:id". */
  path?: string;
  /** Initial URL; defaults to `path`. */
  url?: string;
  me?: Me;
  controlTabs?: ControlTab[];
  /** Extra routes (e.g. navigation targets). */
  routes?: { path: string; element: ReactElement }[];
  isSpaPath?: (path: string) => boolean;
}

/** Renders `element` inside a memory router, a fresh QueryClient, and a seeded session. */
export function renderRoute(element: ReactElement, options: RenderRouteOptions = {}) {
  const path = options.path ?? "/";
  const me = options.me ?? adminMe();
  setCsrfToken(me.csrf_token);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  queryClient.setQueryData(sharedKeys.me, me);
  const router = createMemoryRouter(
    [{ path, element }, ...(options.routes ?? []), { path: "*", element: <p>other page</p> }],
    { initialEntries: [options.url ?? path] },
  );
  const result = render(
    <QueryClientProvider client={queryClient}>
      <ShellContext.Provider value={{ controlTabs: options.controlTabs ?? [], isSpaPath: options.isSpaPath ?? (() => true) }}>
        <RouterProvider router={router} />
      </ShellContext.Provider>
    </QueryClientProvider>,
  );
  return { ...result, router, queryClient };
}
