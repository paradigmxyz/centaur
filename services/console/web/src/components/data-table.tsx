import type { ReactNode } from "react";

import { isPastLastPage, Pagination, type PaginationProps } from "@/components/pagination";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCaption, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import type { PageMeta } from "@/lib/api";
import { cn } from "@/lib/utils";

export interface Column<Row> {
  id: string;
  header: ReactNode;
  cell: (row: Row) => ReactNode;
  /** Classes for both th and td (widths, alignment). */
  className?: string;
  headerClassName?: string;
}

interface DataTableProps<Row> {
  columns: Column<Row>[];
  rows: Row[] | undefined;
  rowKey: (row: Row) => string;
  /** Shown in a full-width row when `rows` is empty. */
  empty: ReactNode;
  loading?: boolean;
  /**
   * Pagination footer (server meta + unit noun). A response past the last
   * page shows the loading rows while the footer moves to the last page.
   */
  pagination?: PaginationProps & { meta: PageMeta };
  /**
   * Fixed layout with colgroup widths from column classNames. Fixed tables keep
   * a minimum width and scroll horizontally inside the panel on narrow screens
   * instead of squeezing their columns.
   */
  fixed?: boolean;
  /**
   * Minimum table width class. Defaults to `min-w-[44rem]` for fixed tables and
   * `min-w-[40rem]` for tables with four or more columns, so narrow screens
   * scroll the table instead of squeezing it.
   */
  minWidth?: string;
  className?: string;
  caption?: string;
}

function defaultMinWidth(fixed: boolean | undefined, columnCount: number) {
  if (fixed) return "min-w-[44rem]";
  return columnCount >= 4 ? "min-w-[40rem]" : undefined;
}

/** The console table panel: header row, hover rows, empty state, pager. */
export function DataTable<Row>({
  columns,
  rows,
  rowKey,
  empty,
  loading,
  pagination,
  fixed,
  minWidth,
  className,
  caption,
}: DataTableProps<Row>) {
  const pastLastPage = pagination !== undefined && isPastLastPage(pagination.meta);
  return (
    <div className={cn("overflow-hidden rounded-xl border bg-card shadow-xs", className)}>
      <Table className={cn("min-w-full", fixed && "table-fixed", minWidth ?? defaultMinWidth(fixed, columns.length))}>
        {caption && <TableCaption className="sr-only">{caption}</TableCaption>}
        <TableHeader className="bg-muted/40">
          <TableRow className="hover:bg-transparent">
            {columns.map((column) => (
              <TableHead
                key={column.id}
                scope="col"
                className={cn(
                  "h-10 px-4 text-xs font-medium first:pl-5 last:pr-5",
                  column.className,
                  "font-sans text-xs font-medium text-muted-foreground",
                  column.headerClassName,
                )}
              >
                {column.header}
              </TableHead>
            ))}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(loading && !rows) || pastLastPage ? (
            <>
              <TableRow className="hover:bg-transparent">
                <TableCell colSpan={columns.length} className="sr-only">
                  Loading…
                </TableCell>
              </TableRow>
              {[0, 1, 2].map((index) => (
                <TableRow key={index} aria-hidden="true" className="hover:bg-transparent">
                  {columns.map((column) => (
                    <TableCell key={column.id} className="px-4 py-3 first:pl-5 last:pr-5">
                      <Skeleton className="h-4 w-full max-w-32" />
                    </TableCell>
                  ))}
                </TableRow>
              ))}
            </>
          ) : rows && rows.length > 0 ? (
            rows.map((row) => (
              <TableRow key={rowKey(row)}>
                {columns.map((column) => (
                  <TableCell
                    key={column.id}
                    className={cn("min-w-0 px-4 py-2.5 whitespace-normal first:pl-5 last:pr-5", column.className)}
                  >
                    {column.cell(row)}
                  </TableCell>
                ))}
              </TableRow>
            ))
          ) : (
            <TableRow className="hover:bg-transparent">
              <TableCell colSpan={columns.length} className="px-5 py-12 text-center whitespace-normal text-muted-foreground">
                {empty}
              </TableCell>
            </TableRow>
          )}
        </TableBody>
      </Table>
      {pagination && <Pagination {...pagination} />}
    </div>
  );
}
