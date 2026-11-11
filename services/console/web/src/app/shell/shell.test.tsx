import { QueryClient, QueryClientProvider, useSuspenseQuery } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { act } from "react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { buildRoutes } from "@/app/router";
import type { Me } from "@/app/session";
import type { FeatureModule } from "@/app/types";
import { api } from "@/lib/api";
import { sharedKeys } from "@/lib/query-keys";
import { adminMe, apiResponse, mockApi, operatorMe } from "@/test/utils";

function FlakyPage() {
  const { data } = useSuspenseQuery({
    queryKey: ["flaky"],
    queryFn: () => api.get<{ data: string }>("/api/v1/flaky"),
  });
  return <p>{data.data}</p>;
}

const testFeature: FeatureModule = {
  id: "shell-test",
  routes: [
    { path: "/console/flaky", element: <FlakyPage /> },
    { path: "/console/other", element: <p>other page</p> },
  ],
  publicRoutes: [{ path: "/pending", element: <p>holding page</p> }],
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

afterEach(() => vi.unstubAllGlobals());

describe("app shell", () => {
  it("refetches a page's failed suspense query when the page is visited again", async () => {
    let calls = 0;
    mockApi({
      "GET /api/v1/flaky": () => {
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
    mockApi({ "GET /api/v1/flaky": apiResponse(500, { error: { message: "upstream exploded" } }) });
    const { queryClient } = renderShell("/console/flaky", adminMe());

    expect(await screen.findByText("upstream exploded")).toBeInTheDocument();
    expect(queryClient.getQueryData(sharedKeys.me)).toBeDefined();
  });

  it("sends a pending account to the holding page instead of rendering the shell", async () => {
    const fetchMock = mockApi({});
    const pending = operatorMe({
      user: { id: "usr_p", email: "waiting@example.test", name: null, status: "pending", admin: false },
    });
    const { router } = renderShell("/console/other", pending);

    expect(await screen.findByText("holding page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/pending");
    expect(screen.queryByText("other page")).not.toBeInTheDocument();
    expect(fetchMock.calls()).toHaveLength(0);
  });
});
