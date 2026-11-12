import { useEffect, useRef } from "react";
import { useSearchParams } from "react-router";

import { Button } from "@/components/ui/button";
import type { PageMeta } from "@/lib/api";
import { pluralize } from "@/lib/format";

export interface PageChangeOptions {
  /** Replace the current history entry instead of pushing a new one. */
  replace?: boolean;
}

export type PageChangeHandler = (page: number, options?: PageChangeOptions) => void;

/**
 * The page number in the URL query (`?page=2`, or a custom name such as
 * `roles_page` so several paginators on one page stay independent). Setting a
 * page keeps every other query param.
 */
export function usePageParam(name = "page"): [number, PageChangeHandler] {
  const [params, setParams] = useSearchParams();
  const raw = Number.parseInt(params.get(name) ?? "1", 10);
  const page = Number.isFinite(raw) && raw > 0 ? raw : 1;
  const setPage: PageChangeHandler = (next, options) =>
    setParams(
      (prev) => {
        const copy = new URLSearchParams(prev);
        if (next <= 1) copy.delete(name);
        else copy.set(name, String(next));
        return copy;
      },
      { preventScrollReset: true, replace: options?.replace },
    );
  return [page, setPage];
}

/**
 * True when the server answered a page past the last one: a stale link, or
 * rows deleted since the page was opened. The API returns an empty page with
 * the real total rather than clamping, so the client moves to the last page.
 */
export function isPastLastPage(meta: Pick<PageMeta, "page" | "total_pages">) {
  return meta.total_pages > 0 && meta.page > meta.total_pages;
}

/**
 * Replaces the URL's page with the last page (keeping every other param) when
 * the response is past it, once per stale response.
 */
function useClampToLastPage(meta: Pick<PageMeta, "page" | "total_pages">, onPageChange: PageChangeHandler) {
  const clamped = useRef<string | null>(null);
  const stale = isPastLastPage(meta);
  useEffect(() => {
    if (!stale) return;
    const key = `${meta.page}/${meta.total_pages}`;
    if (clamped.current === key) return;
    clamped.current = key;
    onPageChange(meta.total_pages, { replace: true });
  }, [stale, meta.page, meta.total_pages, onPageChange]);
}

export interface PaginationProps {
  meta: Pick<PageMeta, "page" | "total" | "total_pages">;
  unit: string;
  onPageChange: PageChangeHandler;
  /** Render nothing when everything fits on one page (stale pages are still clamped). */
  hideSinglePage?: boolean;
}

/**
 * Footer: "51 principals · page 1 of 2" with Previous/Next. A response past
 * the last page sends the URL to the last page instead of showing a false
 * empty list.
 */
export function Pagination({ meta, unit, onPageChange, hideSinglePage }: PaginationProps) {
  useClampToLastPage(meta, onPageChange);
  const totalPages = Math.max(meta.total_pages, 1);
  if (hideSinglePage && totalPages <= 1) return null;
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
