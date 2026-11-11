// Read-only sections of the secret detail page, rendered from the server's
// console view (GET /api/v1/secrets/:kind/:id).
import { DataTable, type Column } from "@/components/data-table";
import { EmptyState } from "@/components/empty-state";
import { SectionHeading } from "@/components/page-header";

import type { RequestRule, SecretDetail, SourceSegment } from "../api";

export function SourcesSection({ segments }: { segments: SourceSegment[] }) {
  const hasRoles = segments.some((segment) => segment.role);
  const columns: Column<SourceSegment>[] = [
    ...(hasRoles
      ? [
          {
            id: "field",
            header: "Field",
            className: "whitespace-nowrap font-mono text-xs text-muted-foreground",
            cell: (segment: SourceSegment) => segment.role || "—",
          },
        ]
      : []),
    {
      id: "backend",
      header: "Backend",
      className: "whitespace-nowrap",
      cell: (segment) => (
        <span className="rounded-md bg-muted px-2 py-0.5 text-xs text-foreground">{segment.type_label}</span>
      ),
    },
    {
      id: "reference",
      header: "Reference",
      cell: (segment) =>
        segment.ref ? (
          <span className="block font-mono text-xs break-all text-foreground">{segment.ref}</span>
        ) : (
          <span className="text-muted-foreground">—</span>
        ),
    },
  ];
  return (
    <section className="mb-8">
      <SectionHeading className="mb-3">Sources</SectionHeading>
      {segments.length === 0 ? (
        <EmptyState>No source configured.</EmptyState>
      ) : (
        <DataTable
          caption="Sources"
          columns={columns}
          rows={segments}
          rowKey={(segment) => `${segment.role ?? ""}:${segment.type}:${segment.ref ?? ""}`}
          empty="No source configured."
        />
      )}
    </section>
  );
}

export function ConfigurationSection({ rows }: { rows: SecretDetail["configuration"] }) {
  if (rows.length === 0) return null;
  return (
    <section className="mb-8">
      <SectionHeading className="mb-3">Configuration</SectionHeading>
      <dl className="divide-y divide-border/70 rounded-xl border bg-card shadow-xs">
        {rows.map((row) => (
          <div key={row.label} className="flex flex-col gap-1 px-5 py-3 sm:flex-row sm:gap-4">
            <dt className="w-40 shrink-0 text-xs text-muted-foreground">{row.label}</dt>
            <dd className="font-mono text-xs break-all whitespace-pre-line text-foreground">{row.value}</dd>
          </div>
        ))}
      </dl>
    </section>
  );
}

export function SignedHeadersSection({ headers }: { headers: SecretDetail["signed_headers"] }) {
  if (!headers || headers.length === 0) return null;
  return (
    <section className="mb-8">
      <SectionHeading className="mb-3">Signed Headers</SectionHeading>
      <DataTable
        caption="Signed headers"
        columns={[
          { id: "name", header: "Name", className: "whitespace-nowrap font-mono text-xs", cell: (h) => h.name },
          { id: "value", header: "Value", className: "font-mono text-xs break-all text-muted-foreground", cell: (h) => h.value },
        ]}
        rows={headers}
        rowKey={(header) => header.name}
        empty=""
      />
    </section>
  );
}

export function RequestRulesSection({ rules }: { rules: RequestRule[] | null }) {
  if (!rules) return null;
  return (
    <section className="mb-8">
      <SectionHeading className="mb-3">Request Rules</SectionHeading>
      {rules.length === 0 ? (
        <p className="text-sm text-muted-foreground">No request rules.</p>
      ) : (
        <DataTable
          caption="Request rules"
          columns={[
            {
              id: "match",
              header: "Match",
              className: "font-mono text-xs",
              cell: (rule) => (rule.host ? `host: ${rule.host}` : `cidr: ${rule.cidr ?? ""}`),
            },
            {
              id: "methods",
              header: "Methods",
              className: "font-mono text-xs text-muted-foreground",
              cell: (rule) => (rule.http_methods.length > 0 ? rule.http_methods.join(", ") : "any"),
            },
            {
              id: "paths",
              header: "Paths",
              className: "font-mono text-xs break-all text-muted-foreground",
              cell: (rule) => (rule.paths.length > 0 ? rule.paths.join(", ") : "any"),
            },
          ]}
          rows={rules}
          rowKey={(rule) => String(rule.position)}
          empty="No request rules."
        />
      )}
    </section>
  );
}
