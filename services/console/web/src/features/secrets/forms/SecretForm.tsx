// The create/edit form for the four console-editable secret kinds. One form
// model (form-model.ts) with per-kind sections; the payload goes to the
// kind's own /api/v1 resource (POST to create, PATCH to update).
import { useState } from "react";
import { Controller, FormProvider, useForm, useFormContext, useWatch, type FieldPath } from "react-hook-form";
import { Link, useNavigate } from "react-router";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { CheckboxField, FormSection, SelectField, TextareaField, TextField } from "@/components/form/fields";
import { Button } from "@/components/ui/button";
import { ApiError, errorMessage } from "@/lib/api";
import { pluralize } from "@/lib/format";

import { useSaveSecret, type FormKind, type KindRecord } from "../api";
import {
  errorMessages,
  GCP_ID_TOKEN_HEADERS,
  STATIC_PROFILES,
  storedSource,
  toFormValues,
  toPayload,
  validateForm,
  type SecretFormValues,
} from "../form-model";
import {
  IdentitySection,
  LabelsSection,
  RadioChoice,
  RulesSection,
  SectionError,
  SettingsSection,
  SourceFields,
} from "./sections";

type Details = Record<string, string[]>;

/** Server keys that map onto a form field's inline error. */
const FIELD_KEYS: Record<string, keyof SecretFormValues> = {
  foreign_id: "foreign_id",
  name: "name",
  description: "description",
  database: "database",
  role: "role",
  subject: "subject",
  scopes: "scopes",
  audience: "audience",
};

const pick = (details: Details, ...keys: string[]) => keys.flatMap((key) => details[key] ?? []);
const notGeneric = (messages: string[]) => messages.filter((message) => message !== "is invalid");

function sourceErrors(details: Details): string[] {
  const nested = Object.fromEntries(Object.entries(details).filter(([key]) => /^(source|dsn|keyfile)\./.test(key)));
  return [
    ...errorMessages(nested).map((message) => message.replace(/^Source: /, "")),
    ...notGeneric(pick(details, "source", "dsn_source", "keyfile_source")),
  ];
}

function ruleErrors(details: Details): Record<number, string[]> {
  const out: Record<number, string[]> = {};
  for (const [key, messages] of Object.entries(details)) {
    const match = /^rules\[(\d+)\]\./.exec(key);
    if (!match) continue;
    const index = Number(match[1]);
    const lines = errorMessages({ [key]: messages }).map((message) => message.replace(/^Rule \d+: /, ""));
    out[index] = [...(out[index] ?? []), ...lines];
  }
  return out;
}

export function SecretForm({ kind, record }: { kind: FormKind; record?: KindRecord }) {
  const navigate = useNavigate();
  const save = useSaveSecret(kind, record?.id);
  const [details, setDetails] = useState<Details>({});
  const [clientErrors, setClientErrors] = useState<string[]>([]);
  const form = useForm<SecretFormValues>({ defaultValues: toFormValues(kind, record) });
  const hasStoredInline = storedSource(kind, record)?.source_type === "control_plane";
  const summary = clientErrors.length > 0 ? clientErrors : errorMessages(details);

  const onSubmit = form.handleSubmit(async (values) => {
    setDetails({});
    const checked = validateForm(kind, values, hasStoredInline);
    setClientErrors(checked.messages);
    if (checked.messages.length > 0) {
      for (const [field, message] of Object.entries(checked.fields)) {
        form.setError(field as FieldPath<SecretFormValues>, { type: "client", message });
      }
      return;
    }
    try {
      const body = await save.mutateAsync(toPayload(kind, values, record));
      toast.success(record ? "Secret updated." : "Secret created.");
      navigate(`/console/secrets/${kind}/${body.data.id}`);
    } catch (error) {
      if (error instanceof ApiError && error.status === 422) {
        const fieldErrors = error.fieldErrors;
        setDetails(Object.keys(fieldErrors).length > 0 ? fieldErrors : { base: [error.message] });
        for (const [key, messages] of Object.entries(fieldErrors)) {
          const field = key === "header" && kind === "gcp_id_token" ? "id_token_header" : FIELD_KEYS[key];
          if (field) form.setError(field, { type: "server", message: messages.join(", ") });
        }
      } else {
        setDetails({});
        toast.error(errorMessage(error));
        form.setError("root", { type: "server", message: errorMessage(error) });
      }
    }
  });

  const rootError = form.formState.errors.root?.message;

  return (
    <>
      {summary.length > 0 && (
        <ErrorAlert>
          <p className="mb-1 font-semibold">{pluralize(summary.length, "error")} prevented this secret from saving:</p>
          <ul className="list-disc space-y-0.5 pl-5">
            {summary.map((message, index) => (
              <li key={`${index}-${message}`}>{message}</li>
            ))}
          </ul>
        </ErrorAlert>
      )}
      {rootError && summary.length === 0 && <ErrorAlert>{rootError}</ErrorAlert>}
      <FormProvider {...form}>
        <form onSubmit={onSubmit} className="space-y-6" noValidate>
          <IdentitySection foreignIdRequired={kind === "pg_dsn"} />
          {kind === "static" && <StaticSections details={details} hasStoredInline={hasStoredInline} />}
          {kind === "pg_dsn" && <PgDsnSections details={details} hasStoredInline={hasStoredInline} />}
          {kind === "gcp_auth" && <GcpAuthSections details={details} hasStoredInline={hasStoredInline} />}
          {kind === "gcp_id_token" && <GcpIdTokenSections details={details} hasStoredInline={hasStoredInline} />}
          <LabelsSection />
          <div className="flex items-center gap-3">
            <Button type="submit" disabled={form.formState.isSubmitting}>
              {record ? "Save changes" : "Create secret"}
            </Button>
            <Button asChild variant="secondary">
              <Link to="/console/secrets">Cancel</Link>
            </Button>
          </div>
        </form>
      </FormProvider>
    </>
  );
}

interface KindSectionProps {
  details: Details;
  hasStoredInline: boolean;
}

function StaticSections({ details, hasStoredInline }: KindSectionProps) {
  const { control } = useFormContextTyped();
  const mode = useWatch({ control, name: "mode" });
  return (
    <>
      <FormSection title="Credential profile">
        <SelectField
          control={control}
          name="profile"
          label="Credential profile"
          options={STATIC_PROFILES.map((option) => ({ ...option }))}
          hint="Profiles apply and validate the proxy configuration and request rules for known credential types."
        />
      </FormSection>
      <FormSection title="Injection">
        <SectionError messages={pick(details, "base", "inject_config", "replace_config")} />
        <Controller
          control={control}
          name="mode"
          render={({ field }) => (
            <RadioChoice
              name="static-mode"
              value={field.value}
              onChange={field.onChange}
              options={[
                { value: "inject", label: "Inject a header or query param" },
                { value: "replace", label: "Replace a placeholder value" },
              ]}
            />
          )}
        />
        {mode === "inject" ? (
          <div className="space-y-4">
            <div className="grid gap-5 sm:grid-cols-2">
              <TextField control={control} name="inject_header" label="Header" placeholder="Authorization" />
              <TextField control={control} name="query_param" label="Query param" placeholder="api_key" />
            </div>
            <TextField
              control={control}
              name="formatter"
              label="Formatter (optional)"
              placeholder="Bearer {{ .Value }}"
              hint="Set a header or a query param (exactly one)."
            />
          </div>
        ) : (
          <div className="space-y-4">
            <TextField
              control={control}
              name="proxy_value"
              label="Placeholder value"
              placeholder="REPLACE_ME"
              hint="The literal value in the request that gets replaced by the resolved secret."
            />
            <TextField control={control} name="match_headers" label="Match headers (comma-separated)" />
            <div className="flex flex-wrap gap-6">
              <CheckboxField control={control} name="match_body" label="Match body" />
              <CheckboxField control={control} name="match_path" label="Match path" />
              <CheckboxField control={control} name="match_query" label="Match query" />
              <CheckboxField control={control} name="require" label="Require a match" />
            </div>
          </div>
        )}
      </FormSection>
      <SourceFields hasStoredInline={hasStoredInline} errors={sourceErrors(details)} />
      <RulesSection errors={notGeneric(pick(details, "rules"))} ruleErrors={ruleErrors(details)} />
    </>
  );
}

function PgDsnSections({ details, hasStoredInline }: KindSectionProps) {
  const { control } = useFormContextTyped();
  return (
    <>
      <FormSection title="Postgres Upstream">
        <div className="grid gap-5 sm:grid-cols-2">
          <TextField
            control={control}
            name="database"
            label="Database *"
            placeholder="analytics"
            hint="The routing key clients connect to. Must match the DSN's database for an inline DSN."
          />
          <TextField control={control} name="role" label="Role (optional)" hint="SET ROLE applied to the upstream session." />
        </div>
      </FormSection>
      <SettingsSection errors={pick(details, "settings")} />
      <SourceFields heading="DSN Source" hasStoredInline={hasStoredInline} errors={sourceErrors(details)} />
    </>
  );
}

function GcpAuthSections({ details, hasStoredInline }: KindSectionProps) {
  const { control } = useFormContextTyped();
  const mode = useWatch({ control, name: "credential_mode" });
  return (
    <>
      <FormSection title="Credentials">
        <SectionError messages={pick(details, "base", "credentials_provider")} />
        <Controller
          control={control}
          name="credential_mode"
          render={({ field }) => (
            <RadioChoice
              name="gcp-credential-mode"
              value={field.value}
              onChange={field.onChange}
              options={[
                { value: "keyfile", label: "Service account keyfile" },
                { value: "workload_identity", label: "Workload identity (metadata server)" },
              ]}
            />
          )}
        />
        {mode === "keyfile" ? (
          <div className="space-y-5">
            <SourceFields heading="Keyfile Source" bare hasStoredInline={hasStoredInline} errors={sourceErrors(details)} />
            <TextField
              control={control}
              name="subject"
              label="Subject (optional)"
              placeholder="user@example.com"
              hint="Domain-wide delegation: impersonate this user. Keyfile credentials only."
            />
          </div>
        ) : (
          <p className="text-xs text-muted-foreground">
            Uses Application Default Credentials from the proxy host&apos;s metadata server. No keyfile is stored.
          </p>
        )}
      </FormSection>
      <FormSection title="Scopes">
        <TextareaField
          control={control}
          name="scopes"
          label="Scopes"
          rows={3}
          placeholder="https://www.googleapis.com/auth/cloud-platform"
          hint="One OAuth scope per line (or comma-separated). At least one is required."
        />
      </FormSection>
      <RulesSection errors={notGeneric(pick(details, "rules"))} ruleErrors={ruleErrors(details)} />
    </>
  );
}

function GcpIdTokenSections({ details, hasStoredInline }: KindSectionProps) {
  const { control } = useFormContextTyped();
  return (
    <>
      <FormSection title="Token">
        <div className="grid gap-5 sm:grid-cols-2">
          <TextField
            control={control}
            name="audience"
            label="Audience"
            placeholder="https://my-service-abc123-uc.a.run.app"
          />
          <SelectField
            control={control}
            name="id_token_header"
            label="Header"
            options={GCP_ID_TOKEN_HEADERS.map((option) => ({ ...option }))}
          />
        </div>
      </FormSection>
      <FormSection title="Credentials">
        <SourceFields heading="Keyfile Source" bare hasStoredInline={hasStoredInline} errors={sourceErrors(details)} />
      </FormSection>
      <RulesSection
        hint="Each rule matches by host or CIDR. GCP ID token secrets require at least one rule."
        errors={notGeneric(pick(details, "rules"))}
        ruleErrors={ruleErrors(details)}
      />
    </>
  );
}

function useFormContextTyped() {
  return useFormContext<SecretFormValues>();
}
