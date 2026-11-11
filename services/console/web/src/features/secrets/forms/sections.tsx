// Form sections shared by the secret kinds: identity, labels, source, request
// rules, and pg_dsn session settings. All read the form from FormProvider.
import { Trash2 } from "lucide-react";
import type { ReactNode } from "react";
import { Controller, useFieldArray, useFormContext, useWatch } from "react-hook-form";

import { FieldShell, FormSection, SelectField, TextField } from "@/components/form/fields";
import { KeyValueRowsField } from "@/components/form/key-value-rows";
import { SecretField } from "@/components/form/secret-field";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

import {
  AWS_SOURCE_TYPES,
  REFERENCE_LABELS,
  SETTING_KINDS,
  SOURCE_TYPE_OPTIONS,
  type SecretFormValues,
} from "../form-model";
import { MethodChips, PathChips } from "./chips";

/** Inline field error under a section (legacy .field-error). */
export function SectionError({ messages }: { messages: string[] | undefined }) {
  if (!messages || messages.length === 0) return null;
  return <p className="mb-3 text-xs text-red-400 light:text-red-700">{messages.join(", ")}</p>;
}

export function IdentitySection({ foreignIdRequired = false }: { foreignIdRequired?: boolean }) {
  const { control } = useFormContext<SecretFormValues>();
  return (
    <FormSection title="Identity">
      <div className="grid gap-5 sm:grid-cols-2">
        <TextField
          control={control}
          name="foreign_id"
          label={foreignIdRequired ? "Foreign ID *" : "Foreign ID"}
          placeholder="stable-handle"
          hint={foreignIdRequired ? undefined : "Optional. A stable, URL-safe handle operators can reference."}
        />
        <TextField control={control} name="name" label="Name" />
        <TextField control={control} name="description" label="Description" />
      </div>
    </FormSection>
  );
}

export function LabelsSection() {
  const { control } = useFormContext<SecretFormValues>();
  return <KeyValueRowsField control={control} name="labels" title="Labels" keyPlaceholder="team" valuePlaceholder="platform" />;
}

interface SourceFieldsProps {
  heading?: string;
  /** Render inside another section (gcp keyfile) instead of its own card. */
  bare?: boolean;
  /** The stored source is inline: a blank value keeps it. */
  hasStoredInline: boolean;
  errors?: string[];
}

/**
 * Source backend + reference. An inline (control_plane) source edits a
 * write-only value that is never prefilled; every other backend edits the
 * reference stored under its config key.
 */
export function SourceFields({ heading = "Source", bare = false, hasStoredInline, errors }: SourceFieldsProps) {
  const { control } = useFormContext<SecretFormValues>();
  const type = useWatch({ control, name: "source.source_type" });
  const inline = type === "control_plane";
  const aws = AWS_SOURCE_TYPES.includes(type);

  const body = (
    <>
      <SectionError messages={errors} />
      <div className="grid gap-5 sm:grid-cols-2">
        <SelectField
          control={control}
          name="source.source_type"
          label="Backend"
          options={SOURCE_TYPE_OPTIONS.map((option) => ({ ...option }))}
        />
        {inline ? (
          <SecretField
            control={control}
            name="source.secret"
            label="Inline value"
            multiline
            hasStoredValue={hasStoredInline}
            placeholder="stored encrypted at rest"
          />
        ) : (
          <TextField control={control} name="source.reference" label={REFERENCE_LABELS[type] ?? "Reference"} />
        )}
        {aws && <TextField control={control} name="source.region" label="Region" placeholder="us-west-2" />}
        <TextField
          control={control}
          name="source.json_key"
          label="JSON key (optional)"
          hint="Extract this key from the resolved value if it is JSON."
        />
      </div>
    </>
  );

  if (bare) {
    return (
      <div className="space-y-4">
        <h3 className="text-xs font-semibold tracking-wider text-foreground uppercase">{heading}</h3>
        {body}
      </div>
    );
  }
  return <FormSection title={heading}>{body}</FormSection>;
}

const EMPTY_RULE = { host: "", cidr: "", http_methods: [], paths: [] };

export function RulesSection({
  hint = "Each rule matches by host or CIDR. With no rules, the secret applies with no request matching.",
  errors,
  ruleErrors,
}: {
  hint?: string;
  errors?: string[];
  /** Messages per rule index. */
  ruleErrors?: Record<number, string[]>;
}) {
  const { control, register } = useFormContext<SecretFormValues>();
  const { fields, append, remove } = useFieldArray({ control, name: "rules" });
  return (
    <FormSection
      title="Request Rules"
      actions={
        <Button type="button" variant="secondary" size="sm" onClick={() => append({ ...EMPTY_RULE })}>
          Add rule
        </Button>
      }
    >
      <p className="mb-3 text-xs text-muted-foreground">{hint}</p>
      <SectionError messages={errors} />
      <div className="space-y-3">
        {fields.map((field, index) => {
          const n = index + 1;
          return (
            <div key={field.id} className="rounded-sm border border-border bg-muted/20 p-4" data-testid={`rule-${n}`}>
              <SectionError messages={ruleErrors?.[index]} />
              <div className="grid gap-4 sm:grid-cols-2">
                <FieldShell id={`rule-${n}-host`} label="Host">
                  <Input id={`rule-${n}-host`} placeholder="api.example.com" aria-label={`Rule ${n} host`} {...register(`rules.${index}.host`)} />
                </FieldShell>
                <FieldShell id={`rule-${n}-cidr`} label="CIDR">
                  <Input id={`rule-${n}-cidr`} placeholder="10.0.0.0/8" aria-label={`Rule ${n} CIDR`} {...register(`rules.${index}.cidr`)} />
                </FieldShell>
                <div className="sm:col-span-2">
                  <p className="mb-1 text-xs font-semibold tracking-wider text-foreground uppercase">HTTP methods</p>
                  <Controller
                    control={control}
                    name={`rules.${index}.http_methods`}
                    render={({ field: methods }) => (
                      <MethodChips label={`Rule ${n} HTTP methods`} value={methods.value} onChange={methods.onChange} />
                    )}
                  />
                </div>
                <div className="sm:col-span-2">
                  <p className="mb-1 text-xs font-semibold tracking-wider text-foreground uppercase">Paths</p>
                  <Controller
                    control={control}
                    name={`rules.${index}.paths`}
                    render={({ field: paths }) => (
                      <PathChips label={`Rule ${n} paths`} value={paths.value} onChange={paths.onChange} />
                    )}
                  />
                </div>
              </div>
              <div className="mt-3 text-right">
                <button
                  type="button"
                  className="cursor-pointer text-xs text-muted-foreground transition-colors hover:text-red-400"
                  onClick={() => remove(index)}
                >
                  Remove rule {n}
                </button>
              </div>
            </div>
          );
        })}
      </div>
    </FormSection>
  );
}

export function SettingsSection({ errors }: { errors?: string[] }) {
  const { control, register } = useFormContext<SecretFormValues>();
  const { fields, append, remove } = useFieldArray({ control, name: "settings" });
  return (
    <FormSection
      title="Session Settings"
      actions={
        <Button type="button" variant="secondary" size="sm" onClick={() => append({ name: "", kind: "literal", value: "" })}>
          Add setting
        </Button>
      }
    >
      <p className="mb-3 text-xs text-muted-foreground">
        Pinned session variables (GUCs) the proxy SETs at session start, in order, before SET ROLE. Clients can&apos;t
        override them. Names must be a bare or dotted identifier (e.g. <code>app.tenant</code>); <code>role</code> and{" "}
        <code>session_authorization</code> are reserved. A value is a literal, or resolved at sync time from an
        assigned principal label, an assigned principal field (<code>id</code>, <code>foreign_id</code>,{" "}
        <code>name</code>), or a proxy label.
      </p>
      <div className="space-y-2">
        {fields.map((field, index) => {
          const n = index + 1;
          return (
            <div key={field.id} className="flex items-center gap-2">
              <Input aria-label={`Setting ${n} name`} placeholder="app.tenant" {...register(`settings.${index}.name`)} />
              <span className="text-subtle-foreground">=</span>
              <Controller
                control={control}
                name={`settings.${index}.kind`}
                render={({ field: kind }) => (
                  <Select value={kind.value} onValueChange={kind.onChange}>
                    <SelectTrigger className="w-44 shrink-0" aria-label={`Setting ${n} kind`}>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {SETTING_KINDS.map((option) => (
                        <SelectItem key={option.value} value={option.value}>
                          {option.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                )}
              />
              <Input aria-label={`Setting ${n} value`} placeholder="centaur" {...register(`settings.${index}.value`)} />
              <Button type="button" variant="ghost" size="icon-sm" aria-label={`Remove setting ${n}`} onClick={() => remove(index)}>
                <Trash2 />
              </Button>
            </div>
          );
        })}
      </div>
      <SectionError messages={errors} />
    </FormSection>
  );
}

/** A pair of radio panels (legacy toggle controller). */
export function RadioChoice<V extends string>({
  name,
  value,
  onChange,
  options,
}: {
  name: string;
  value: V;
  onChange: (value: V) => void;
  options: { value: V; label: ReactNode }[];
}) {
  return (
    <div className="mb-4 flex flex-wrap gap-6 text-sm text-foreground" role="radiogroup">
      {options.map((option) => (
        <label key={option.value} className="flex cursor-pointer items-center gap-2">
          <input
            type="radio"
            name={name}
            value={option.value}
            checked={value === option.value}
            onChange={() => onChange(option.value)}
            className="accent-primary"
          />
          <span>{option.label}</span>
        </label>
      ))}
    </div>
  );
}
