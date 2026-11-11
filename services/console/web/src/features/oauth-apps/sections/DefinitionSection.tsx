import type { ReactNode } from "react";

import { SectionHeading } from "@/components/page-header";

/** Legacy def_section: a heading over a panel of label/value rows. */
export function DefinitionSection({
  title,
  rows,
  footer,
}: {
  title: string;
  rows: [label: string, value: ReactNode][];
  footer?: ReactNode;
}) {
  return (
    <section className="mb-8" aria-label={title}>
      <SectionHeading className="mb-3">{title}</SectionHeading>
      <dl className="divide-y divide-border rounded-xl border bg-card shadow-xs">
        {rows.map(([label, value]) => (
          <div key={label} className="flex flex-col gap-1 px-5 py-3 sm:flex-row sm:gap-4">
            <dt className="w-48 shrink-0 text-xs text-muted-foreground">{label}</dt>
            <dd className="font-mono text-xs break-all whitespace-pre-line text-foreground">
              {value === "" || value === null || value === undefined ? (
                <span className="text-muted-foreground">—</span>
              ) : (
                value
              )}
            </dd>
          </div>
        ))}
      </dl>
      {footer && <p className="mt-2 text-xs text-muted-foreground">{footer}</p>}
    </section>
  );
}
