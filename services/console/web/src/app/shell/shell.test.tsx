import { QueryClient, QueryClientProvider, useSuspenseQuery } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import { act } from "react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { buildRoutes } from "@/app/router";
import type { Me } from "@/app/session";
import type { FeatureModule } from "@/app/types";
import { api, navigation } from "@/lib/api";
import { sharedKeys } from "@/lib/query-keys";
import { adminMe, apiResponse, mockApi, operatorMe } from "@/test/utils";

function FlakyPage() {
  const { data } = useSuspenseQuery({
    queryKey: ["flaky"],
    queryFn: () => api.get<{ data: string }>("/api/v1/console/flaky"),
  });
  return <p>{data.data}</p>;
}

const testFeature: FeatureModule = {
  id: "shell-test",
  routes: [
    { path: "/console/flaky", element: <FlakyPage /> },
    { path: "/console/other", element: <p>other page</p> },
    { path: "/console/lazy", lazy: async () => ({ Component: () => <p>lazy page</p> }) },
  ],
};

function renderShell(url: string, me: Me) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  queryClient.setQueryData(sharedKeys.me, me);
  const router = createMemoryRouter(buildRoutes([testFeature]), { initialEntries: [url] });
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { router, queryClient };
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("app shell", () => {
  it("refetches a page's failed suspense query when the page is visited again", async () => {
    let calls = 0;
    mockApi({
      "GET /api/v1/console/flaky": () => {
        calls += 1;
        return calls === 1 ? apiResponse(500, { error: { message: "upstream exploded" } }) : { data: "flaky ok" };
      },
    });
    const { router } = renderShell("/console/flaky", adminMe());

    expect(await screen.findByText("upstream exploded")).toBeInTheDocument();

    await act(() => router.navigate("/console/other"));
    expect(await screen.findByText("other page")).toBeInTheDocument();

    await act(() => router.navigate("/console/flaky"));
    expect(await screen.findByText("flaky ok")).toBeInTheDocument();
    expect(calls).toBe(2);
  });

  it("keeps the session query when a page fails", async () => {
    mockApi({ "GET /api/v1/console/flaky": apiResponse(500, { error: { message: "upstream exploded" } }) });
    const { queryClient } = renderShell("/console/flaky", adminMe());

    expect(await screen.findByText("upstream exploded")).toBeInTheDocument();
    expect(queryClient.getQueryData(sharedKeys.me)).toBeDefined();
  });

  it("loads a lazy route on a full page load without a HydrateFallback warning", async () => {
    const warn = vi.spyOn(console, "warn");
    renderShell("/console/lazy", adminMe());

    expect(await screen.findByText("lazy page")).toBeInTheDocument();
    expect(warn.mock.calls.flat().join(" ")).not.toMatch(/HydrateFallback/);
  });

  it("sends a pending account to the server-rendered holding page instead of rendering the shell", async () => {
    const assign = vi.spyOn(navigation, "assign").mockImplementation(() => {});
    const fetchMock = mockApi({});
    const pending = operatorMe({
      user: { id: "usr_p", email: "waiting@example.test", name: null, status: "pending", admin: false },
    });
    renderShell("/console/other", pending);

    await waitFor(() => expect(assign).toHaveBeenCalledWith("/pending"));
    expect(screen.queryByText("other page")).not.toBeInTheDocument();
    expect(fetchMock.calls()).toHaveLength(0);
  });
});
