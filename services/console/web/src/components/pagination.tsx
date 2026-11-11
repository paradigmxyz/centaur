import { useSearchParams } from "react-router";

import { Button } from "@/components/ui/button";
import type { PageMeta } from "@/lib/api";
import { pluralize } from "@/lib/format";

/**
 * The page number in the URL query (`?page=2`, or a custom name such as
 * `roles_page` so several paginators on one page stay independent). Setting a
 * page keeps every other query param.
 */
export function usePageParam(name = "page"): [number, (page: number) => void] {
  const [params, setParams] = useSearchParams();
  const raw = Number.parseInt(params.get(name) ?? "1", 10);
  const page = Number.isFinite(raw) && raw > 0 ? raw : 1;
  const setPage = (next: number) =>
    setParams(
      (prev) => {
        const copy = new URLSearchParams(prev);
        if (next <= 1) copy.delete(name);
        else copy.set(name, String(next));
        return copy;
      },
      { preventScrollReset: true },
    );
  return [page, setPage];
}

interface PaginationProps {
  meta: Pick<PageMeta, "page" | "total" | "total_pages">;
  unit: string;
  onPageChange: (page: number) => void;
}

/** Footer: "51 principals · page 1 of 2" with Previous/Next. */
export function Pagination({ meta, unit, onPageChange }: PaginationProps) {
  const totalPages = Math.max(meta.total_pages, 1);
  return (
    <div className="flex items-center justify-between gap-4 border-t px-5 py-3 text-xs text-muted-foreground">
      <div>
        {pluralize(meta.total, unit)}
        {totalPages > 1 && (
          <span>
            {" "}
            · page {meta.page} of {totalPages.toLocaleString("en-US")}
          </span>
        )}
      </div>
      {totalPages > 1 && (
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={meta.page <= 1}
            onClick={() => onPageChange(meta.page - 1)}
          >
            Previous
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={meta.page >= totalPages}
            onClick={() => onPageChange(meta.page + 1)}
          >
            Next
          </Button>
        </div>
      )}
    </div>
  );
}
