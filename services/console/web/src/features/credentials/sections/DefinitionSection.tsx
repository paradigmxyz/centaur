import type { ReactNode } from "react";

import { SectionHeading } from "@/components/page-header";

export type DefinitionRow = [label: string, value: ReactNode];

function isBlank(value: ReactNode) {
  return value === null || value === undefined || value === false || value === "";
}

/**
 * A labeled definition list (the credential detail page's dl sections). Pass
 * `false`/`null` rows to skip them; blank values render an em dash.
 */
export function DefinitionSection({
  title,
  rows,
  note,
}: {
  title: string;
  rows: (DefinitionRow | false | null | undefined)[];
  note?: ReactNode;
}) {
  return (
    <section className="mb-8" aria-label={title}>
      <SectionHeading className="mb-3">{title}</SectionHeading>
      <dl className="divide-y divide-border/70 rounded-sm border border-border bg-card">
        {rows
          .filter((row): row is DefinitionRow => Boolean(row))
          .map(([label, value]) => (
            <div key={label} className="flex flex-col gap-1 px-5 py-3 sm:flex-row sm:gap-4">
              <dt className="w-48 shrink-0 text-xs tracking-wider text-muted-foreground uppercase">{label}</dt>
              <dd className="font-mono text-xs break-all whitespace-pre-line text-heading">
                {isBlank(value) ? <span className="text-subtle-foreground">—</span> : value}
              </dd>
            </div>
          ))}
      </dl>
      {note && <p className="mt-2 text-xs text-subtle-foreground">{note}</p>}
    </section>
  );
}
