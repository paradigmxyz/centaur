// The broker credential create/edit form (legacy broker_credentials/_form).
// Every write-only value (client secret, refresh token, username, password,
// API key, header values) starts blank and is omitted from the request when
// left blank, so the stored value is kept. Header values are never prefilled:
// a stored header shows its name with an "(unchanged)" value placeholder and is
// sent as `null`, which the API resolves to the stored value.
import { zodResolver } from "@hookform/resolvers/zod";
import { Trash2 } from "lucide-react";
import { FormProvider, useController, useFieldArray, useForm, useWatch, type Control } from "react-hook-form";
import { Link } from "react-router";
import { z } from "zod";

import { FieldShell, FormSection, TextareaField, TextField, fieldId } from "@/components/form/fields";
import { applyApiErrors, FormErrorSummary } from "@/components/form/form-errors";
import { KeyValueRowsField, recordToRows, rowsToRecord } from "@/components/form/key-value-rows";
import { SecretField, omitBlankSecrets } from "@/components/form/secret-field";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";

import { GRANTS, PREQIN_TOKEN_ENDPOINT, type BrokerCredential, type CredentialPayload, type Grant } from "../api";
import { grantLabel } from "./provider";

const UNCHANGED = "•••••••• (unchanged)";

const headerRow = z.object({
  key: z.string(),
  value: z.string(),
  /** The stored header name this row was loaded from (edit only). */
  stored: z.string().optional(),
});

const schema = z
  .object({
    foreign_id: z
      .string()
      .trim()
      .regex(/^[A-Za-z0-9\-._~]*$/, "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"),
    name: z.string(),
    description: z.string(),
    token_endpoint: z.string(),
    client_id: z.string(),
    client_secret: z.string(),
    scopes: z.string(),
    headers: z.array(headerRow),
    early_refresh_fraction: z.string(),
    early_refresh_slack_seconds: z.string(),
    max_refresh_interval_seconds: z.string(),
    refresh_timeout_seconds: z.string(),
    grant: z.enum(GRANTS),
    refresh_token: z.string(),
    username: z.string(),
    password: z.string(),
    preqin_username: z.string(),
    api_key: z.string(),
    labels: z.array(z.object({ key: z.string(), value: z.string() })),
  })
  .superRefine((values, ctx) => {
    values.headers.forEach((row, index) => {
      const key = row.key.trim();
      // A renamed stored header has no value to keep.
      if (key && row.stored && key !== row.stored && row.value === "") {
        ctx.addIssue({ code: "custom", path: ["headers", index, "value"], message: "Enter a value for the renamed header." });
      }
    });
  });

export type CredentialFormValues = z.infer<typeof schema>;

const FIELDS = [
  "foreign_id",
  "name",
  "description",
  "token_endpoint",
  "client_id",
  "client_secret",
  "scopes",
  "early_refresh_fraction",
  "early_refresh_slack_seconds",
  "max_refresh_interval_seconds",
  "refresh_timeout_seconds",
  "grant",
  "refresh_token",
  "username",
  "password",
  "preqin_username",
  "api_key",
] as const;

const SECRET_FIELDS = ["client_secret", "refresh_token", "username", "password", "preqin_username", "api_key"] as const;

function defaultValues(credential?: BrokerCredential): CredentialFormValues {
  return {
    foreign_id: credential?.foreign_id ?? "",
    name: credential?.name ?? "",
    description: credential?.description ?? "",
    token_endpoint: credential?.token_endpoint ?? "",
    client_id: credential?.client_id ?? "",
    client_secret: "",
    scopes: (credential?.scopes ?? []).join("\n"),
    headers: (credential?.token_endpoint_header_names ?? []).map((name) => ({ key: name, value: "", stored: name })),
    // BrokerCredential column defaults for a new record.
    early_refresh_fraction: String(credential?.early_refresh_fraction ?? 0.2),
    early_refresh_slack_seconds: String(credential?.early_refresh_slack_seconds ?? 300),
    max_refresh_interval_seconds: String(credential?.max_refresh_interval_seconds ?? 86400),
    refresh_timeout_seconds: String(credential?.refresh_timeout_seconds ?? 30),
    grant: credential?.grant ?? "refresh_token",
    refresh_token: "",
    username: "",
    password: "",
    preqin_username: "",
    api_key: "",
    labels: recordToRows(credential?.labels),
  };
}

/** Form values -> API body. Exported for tests. */
export function toPayload(values: CredentialFormValues, credential?: BrokerCredential): CredentialPayload {
  const headers: Record<string, string | null> = {};
  for (const row of values.headers) {
    const key = row.key.trim();
    if (!key) continue;
    headers[key] = row.value === "" && row.stored === key ? null : row.value;
  }

  // Only the selected grant's write-only fields are sent; the Preqin panel's
  // username is stored as `username`.
  const grantFields: Pick<CredentialPayload, "refresh_token" | "username" | "password" | "api_key"> = {};
  if (values.grant === "refresh_token") grantFields.refresh_token = values.refresh_token;
  if (values.grant === "password") Object.assign(grantFields, { username: values.username, password: values.password });
  if (values.grant === "preqin") Object.assign(grantFields, { username: values.preqin_username, api_key: values.api_key });

  const foreignIdLocked = Boolean(credential?.foreign_id);
  const payload: CredentialPayload = {
    ...(foreignIdLocked ? {} : { foreign_id: values.foreign_id.trim() || null }),
    name: values.name,
    description: values.description,
    grant: values.grant,
    token_endpoint: values.token_endpoint.trim(),
    client_id: values.client_id.trim() || null,
    client_secret: values.client_secret,
    scopes: values.scopes.split(/\s+/).filter(Boolean),
    token_endpoint_headers: Object.keys(headers).length > 0 ? headers : null,
    early_refresh_fraction: values.early_refresh_fraction,
    early_refresh_slack_seconds: values.early_refresh_slack_seconds,
    max_refresh_interval_seconds: values.max_refresh_interval_seconds,
    refresh_timeout_seconds: values.refresh_timeout_seconds,
    ...grantFields,
    labels: rowsToRecord(values.labels),
  };
  return omitBlankSecrets(payload, ["client_secret", "refresh_token", "username", "password", "api_key"]) as CredentialPayload;
}

interface CredentialFormProps {
  credential?: BrokerCredential;
  submitLabel: string;
  error: unknown;
  onSubmit: (payload: CredentialPayload) => Promise<unknown>;
}

export function CredentialForm({ credential, submitLabel, error, onSubmit }: CredentialFormProps) {
  const foreignIdLocked = Boolean(credential?.foreign_id);
  const form = useForm<CredentialFormValues>({
    resolver: zodResolver(schema),
    defaultValues: defaultValues(credential),
  });
  const grant = useWatch({ control: form.control, name: "grant" });

  const submit = form.handleSubmit(async (values) => {
    try {
      await onSubmit(toPayload(values, credential));
    } catch (caught) {
      // The Preqin panel's username field reports the shared `username` error.
      applyApiErrors(form.setError, caught, FIELDS, values.grant === "preqin" ? { username: "preqin_username" } : {});
    } finally {
      clearSecretFields();
    }
  });

  // Write-only plaintext never outlives a submit, whether it succeeded or not:
  // the inputs are blanked (a stored header keeps its name and reverts to
  // "(unchanged)"; a new header's value must be re-entered).
  const clearSecretFields = () => {
    for (const name of SECRET_FIELDS) form.setValue(name, "");
    form.getValues("headers").forEach((_, index) => form.setValue(`headers.${index}.value`, ""));
  };

  return (
    <>
      <FormErrorSummary error={error} subject="Credential" />
      <FormProvider {...form}>
        <form onSubmit={submit} className="max-w-4xl space-y-6" noValidate autoComplete="off">
          <FormSection title="Identity">
            <div className="grid gap-5 sm:grid-cols-2">
              <TextField
                control={form.control}
                name="foreign_id"
                label="Foreign ID"
                placeholder="stable-handle"
                disabled={foreignIdLocked}
                hint={`Optional. A stable, URL-safe handle operators can reference.${foreignIdLocked ? " It cannot be changed once set." : ""}`}
              />
              <TextField control={form.control} name="name" label="Name" />
              <TextField control={form.control} name="description" label="Description" />
            </div>
          </FormSection>

          <FormSection title="Token Client">
            <div className="grid gap-5 sm:grid-cols-2">
              <TextField
                control={form.control}
                name="token_endpoint"
                label="Token endpoint *"
                placeholder={grant === "preqin" ? PREQIN_TOKEN_ENDPOINT : "https://oauth2.googleapis.com/token"}
                hint="The token endpoint control refreshes against. Preqin uses its fixed Operational API endpoint."
              />
              <TextField
                control={form.control}
                name="client_id"
                label={grant === "preqin" ? "Client ID" : "Client ID *"}
                hint="Not secret. Not used for Preqin."
              />
              <SecretField
                control={form.control}
                name="client_secret"
                label="Client secret"
                hasStoredValue={Boolean(credential?.has_client_secret)}
                hint={`Stored encrypted. Not used for Preqin.${credential?.has_client_secret ? " Leave blank to keep the current secret." : ""}`}
              />
              <TextareaField
                control={form.control}
                name="scopes"
                label="Scopes"
                rows={3}
                placeholder="https://www.googleapis.com/auth/gmail.readonly"
                hint="One scope per line. Not used for Preqin."
              />
            </div>
          </FormSection>

          <HeaderRowsField control={form.control} />

          <FormSection title="Refresh Policy">
            <div className="grid gap-5 sm:grid-cols-2">
              <NumberField
                control={form.control}
                name="early_refresh_fraction"
                label="Early refresh fraction"
                step="any"
                min={0}
                hint="Refresh once this fraction of the token's life remains (0 to <1)."
              />
              <NumberField
                control={form.control}
                name="early_refresh_slack_seconds"
                label="Early refresh slack (s)"
                min={1}
                hint="Minimum lead time before expiry to refresh."
              />
              <NumberField
                control={form.control}
                name="max_refresh_interval_seconds"
                label="Max refresh interval (s)"
                min={1}
                hint="Refresh at least this often, even for long-lived tokens."
              />
              <NumberField
                control={form.control}
                name="refresh_timeout_seconds"
                label="Refresh timeout (s)"
                min={1}
                hint="HTTP timeout for a single refresh request."
              />
            </div>
          </FormSection>

          <FormSection title="Bootstrap">
            <GrantRadios control={form.control} />
            {grant === "client_credentials" && (
              <p className="text-xs text-muted-foreground">
                Uses the client ID and client secret above to mint each access token. No refresh token is required.
              </p>
            )}
            {grant === "refresh_token" && (
              <SecretField
                control={form.control}
                name="refresh_token"
                label="Refresh Token"
                multiline
                hasStoredValue={Boolean(credential?.has_refresh_token)}
                placeholder="initial refresh token for the loop"
                hint="Write-only initial value, stored encrypted. Leave blank to keep the current token; entering a new one reschedules the credential and clears any dead state."
              />
            )}
            {grant === "password" && (
              <div className="grid gap-5 sm:grid-cols-2">
                <SecretField
                  control={form.control}
                  name="username"
                  label="Username *"
                  hasStoredValue={Boolean(credential?.has_username)}
                  hint="Stored encrypted. Leave blank to keep the current username."
                />
                <SecretField
                  control={form.control}
                  name="password"
                  label="Password *"
                  hasStoredValue={Boolean(credential?.has_password)}
                  hint="Stored encrypted. Leave blank to keep the current password."
                />
              </div>
            )}
            {grant === "preqin" && (
              <div className="grid gap-5 sm:grid-cols-2">
                <SecretField
                  control={form.control}
                  name="preqin_username"
                  label="Username *"
                  hasStoredValue={Boolean(credential?.has_username)}
                  hint="Stored encrypted. Leave blank to keep the current username."
                />
                <SecretField
                  control={form.control}
                  name="api_key"
                  label="API key *"
                  hasStoredValue={Boolean(credential?.has_api_key)}
                  hint="Stored encrypted. Leave blank to keep the current API key."
                />
              </div>
            )}
          </FormSection>

          <KeyValueRowsField control={form.control} name="labels" title="Labels" />

          <div className="flex items-center gap-3">
            <Button type="submit" disabled={form.formState.isSubmitting}>
              {submitLabel}
            </Button>
            <Button asChild variant="secondary">
              <Link to="/console/credentials">Cancel</Link>
            </Button>
          </div>
        </form>
      </FormProvider>
    </>
  );
}

type NumberFieldName =
  | "early_refresh_fraction"
  | "early_refresh_slack_seconds"
  | "max_refresh_interval_seconds"
  | "refresh_timeout_seconds";

function NumberField({
  control,
  name,
  label,
  hint,
  step,
  min,
}: {
  control: Control<CredentialFormValues>;
  name: NumberFieldName;
  label: string;
  hint: string;
  step?: string;
  min: number;
}) {
  const { field, fieldState } = useController({ control, name });
  const id = fieldId(name);
  return (
    <FieldShell id={id} label={label} hint={hint} error={fieldState.error?.message}>
      <Input
        id={id}
        type="number"
        inputMode="decimal"
        step={step}
        min={min}
        aria-invalid={fieldState.invalid || undefined}
        {...field}
      />
    </FieldShell>
  );
}

function GrantRadios({ control }: { control: Control<CredentialFormValues> }) {
  const { field, fieldState } = useController({ control, name: "grant" });
  return (
    <fieldset className="mb-4">
      <legend className="sr-only">Grant</legend>
      <RadioGroup
        name={field.name}
        value={field.value}
        onValueChange={(next) => field.onChange(next as Grant)}
        className="flex flex-wrap gap-2"
      >
        {GRANTS.map((value: Grant) => (
          <label
            key={value}
            className="inline-flex cursor-pointer items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors has-data-[state=checked]:border-primary/50 has-data-[state=checked]:bg-primary/10"
          >
            <RadioGroupItem value={value} onBlur={field.onBlur} />
            <span>{grantLabel(value)}</span>
          </label>
        ))}
      </RadioGroup>
      {fieldState.error && <p className="mt-1.5 text-xs text-destructive">{fieldState.error.message}</p>}
    </fieldset>
  );
}

/**
 * Token endpoint header rows. Values are write-only: stored headers load with
 * their name only, and a blank value keeps the stored one.
 */
function HeaderRowsField({ control }: { control: Control<CredentialFormValues> }) {
  const { fields, append, remove } = useFieldArray({ control, name: "headers" });
  return (
    <FormSection
      title="Token Endpoint Headers"
      actions={
        <Button type="button" variant="secondary" size="sm" onClick={() => append({ key: "", value: "" })}>
          Add header
        </Button>
      }
    >
      <p className="-mt-1 mb-3 text-xs text-muted-foreground">
        Extra headers sent on the token request (for example a Basic auth header). Values are stored encrypted and never
        shown; leave a stored header&apos;s value blank to keep it.
      </p>
      <div className="space-y-2">
        {fields.map((row, index) => (
          <HeaderRow key={row.id} control={control} index={index} stored={row.stored} onRemove={() => remove(index)} />
        ))}
      </div>
    </FormSection>
  );
}

function HeaderRow({
  control,
  index,
  stored,
  onRemove,
}: {
  control: Control<CredentialFormValues>;
  index: number;
  stored: string | undefined;
  onRemove: () => void;
}) {
  const key = useController({ control, name: `headers.${index}.key` });
  const value = useController({ control, name: `headers.${index}.value` });
  const keepsStored = Boolean(stored) && key.field.value.trim() === stored;
  const error = value.fieldState.error?.message;
  return (
    <div>
      <div className="flex items-center gap-3">
        <Input aria-label={`Header ${index + 1} name`} placeholder="Authorization" {...key.field} />
        <span className="text-muted-foreground">:</span>
        <Input
          aria-label={`Header ${index + 1} value`}
          type="password"
          autoComplete="new-password"
          spellCheck={false}
          data-secret-field=""
          placeholder={keepsStored ? UNCHANGED : "Basic …"}
          aria-invalid={error ? true : undefined}
          {...value.field}
        />
        <Button type="button" variant="ghost" size="icon-sm" aria-label={`Remove header ${index + 1}`} onClick={onRemove}>
          <Trash2 />
        </Button>
      </div>
      {error && <p className="mt-1 text-xs text-destructive">{error}</p>}
    </div>
  );
}
