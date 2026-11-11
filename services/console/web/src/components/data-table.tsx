import type { ReactNode } from "react";

import { Pagination } from "@/components/pagination";
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
  /** Pagination footer (server meta + unit noun). */
  pagination?: { meta: PageMeta; unit: string; onPageChange: (page: number) => void };
  /** Fixed layout with colgroup widths from column classNames. */
  fixed?: boolean;
  className?: string;
  caption?: string;
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
  className,
  caption,
}: DataTableProps<Row>) {
  return (
    <div className={cn("overflow-hidden rounded-sm border border-border bg-card", className)}>
      <div className="overflow-x-auto">
        <table className={cn("min-w-full text-sm", fixed && "w-full table-fixed")}>
          {caption && <caption className="sr-only">{caption}</caption>}
          <thead className="border-b border-border bg-table-head text-left text-xs tracking-wider text-muted-foreground uppercase">
            <tr>
              {columns.map((column) => (
                <th key={column.id} scope="col" className={cn("px-5 py-2.5 font-medium", column.className, column.headerClassName)}>
                  {column.header}
                </th>
              ))}
            </tr>
          </thead>
          <tbody className="divide-y divide-border/70">
            {loading && !rows ? (
              <tr>
                <td colSpan={columns.length} className="px-5 py-10 text-center text-subtle-foreground">
                  Loading…
                </td>
              </tr>
            ) : rows && rows.length > 0 ? (
              rows.map((row) => (
                <tr key={rowKey(row)} className="transition-colors hover:bg-primary/[0.06]">
                  {columns.map((column) => (
                    <td key={column.id} className={cn("min-w-0 px-5 py-2", column.className)}>
                      {column.cell(row)}
                    </td>
                  ))}
                </tr>
              ))
            ) : (
              <tr>
                <td colSpan={columns.length} className="px-5 py-10 text-center text-subtle-foreground">
                  {empty}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
      {pagination && <Pagination {...pagination} />}
    </div>
  );
}
