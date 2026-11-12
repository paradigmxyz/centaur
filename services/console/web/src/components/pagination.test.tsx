import { render, screen } from "@testing-library/react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { describe, expect, it } from "vitest";

import { DataTable } from "@/components/data-table";
import { usePageParam } from "@/components/pagination";

// A list backed by an unclamped server page, like the /api/v1 list endpoints:
// a page past the end answers `data: []` with the real total.
function List({ rows, limit, hideSinglePage }: { rows: string[]; limit: number; hideSinglePage?: boolean }) {
  const [page, setPage] = usePageParam();
  const total_pages = Math.ceil(rows.length / limit);
  const meta = { page, limit, total: rows.length, total_pages };
  return (
    <DataTable
      columns={[{ id: "name", header: "Name", cell: (row: string) => row }]}
      rows={rows.slice((page - 1) * limit, page * limit)}
      rowKey={(row) => row}
      empty="Nothing here yet."
      pagination={{ meta, unit: "row", onPageChange: setPage, hideSinglePage }}
    />
  );
}

function renderList(url: string, props: Parameters<typeof List>[0]) {
  const router = createMemoryRouter([{ path: "/list", element: <List {...props} /> }], { initialEntries: [url] });
  render(<RouterProvider router={router} />);
  return router;
}

describe("stale pagination", () => {
  it("replaces a page past the end with the last page and keeps other params", async () => {
    const router = renderList("/list?q=foo&page=9", { rows: ["a", "b", "c"], limit: 1 });

    expect(await screen.findByText("c")).toBeInTheDocument();
    expect(screen.queryByText("Nothing here yet.")).not.toBeInTheDocument();
    expect(router.state.location.search).toBe("?q=foo&page=3");
    expect(router.state.historyAction).toBe("REPLACE");
  });

  it("clamps a hidden single-page footer to page one", async () => {
    const router = renderList("/list?q=foo&page=4", { rows: ["a"], limit: 50, hideSinglePage: true });

    expect(await screen.findByText("a")).toBeInTheDocument();
    expect(router.state.location.search).toBe("?q=foo");
  });

  it("shows the empty state without navigating when there are no rows", () => {
    const router = renderList("/list?q=foo&page=4", { rows: [], limit: 50 });

    expect(screen.getByText("Nothing here yet.")).toBeInTheDocument();
    expect(router.state.location.search).toBe("?q=foo&page=4");
    expect(router.state.historyAction).toBe("POP");
  });
});
