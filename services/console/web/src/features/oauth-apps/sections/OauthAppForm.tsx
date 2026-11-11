// The OAuth app create/edit form (legacy console/oauth_apps/_form). The
// client secret is write-only: never prefilled, and a blank value on edit is
// omitted so the stored secret is kept. `shared` is create-only.
import { zodResolver } from "@hookform/resolvers/zod";
import { FormProvider, useForm, type FieldError } from "react-hook-form";
import { Link } from "react-router";
import { z } from "zod";

import { CheckboxField, FormSection, SelectField, TextareaField, TextField } from "@/components/form/fields";
import { applyApiErrors } from "@/components/form/form-errors";
import { KeyValueRowsField, recordToRows, rowsToRecord, type KeyValueRow } from "@/components/form/key-value-rows";
import { SecretField } from "@/components/form/secret-field";
import { Button } from "@/components/ui/button";

import type { OauthApp, OauthAppInput, OauthProvider } from "../api";

export const CUSTOM_PROVIDER = "custom";

/** Labels for the custom provider's config fields, in form order. */
const CONFIG_LABELS: Record<string, string> = {
  authorization_endpoint: "Authorization endpoint",
  token_endpoint: "Token endpoint",
  userinfo_endpoint: "Userinfo endpoint (optional)",
  resource: "Resource URL",
};

// Only the slug is checked client-side: the API ignores a blank slug on update
// (keeping the stored one), so clearing it would otherwise "succeed" silently.
// Everything else is validated by the model and mapped back from the 422.
const schema = z.object({
  slug: z.string().trim().min(1, "can't be blank"),
  description: z.string(),
  provider: z.string(),
  client_id: z.string(),
  client_secret: z.string(),
  provider_config: z.record(z.string(), z.string().optional()),
  allowed_scopes: z.string(),
  enabled: z.boolean(),
  shared: z.boolean(),
  always_available: z.boolean(),
  labels: z.array(z.object({ key: z.string(), value: z.string() })),
});

export type OauthAppFormValues = z.infer<typeof schema> & { labels: KeyValueRow[] };

const FIELDS = [
  "slug",
  "description",
  "provider",
  "client_id",
  "client_secret",
  "provider_config",
  "allowed_scopes",
  "enabled",
  "shared",
  "always_available",
  "labels",
] as const;

export function initialValues(app: OauthApp | undefined, defaultProvider: string): OauthAppFormValues {
  return {
    slug: app?.slug ?? "",
    description: app?.description ?? "",
    provider: app?.provider ?? defaultProvider,
    client_id: app?.client_id ?? "",
    client_secret: "",
    provider_config: { ...(app?.provider_config ?? {}) },
    allowed_scopes: (app?.allowed_scopes ?? []).join("\n"),
    enabled: app?.enabled ?? true,
    shared: app?.shared ?? false,
    always_available: app?.always_available ?? false,
    labels: recordToRows(app?.labels),
  };
}

/** Scopes are entered one per line (they contain no spaces); blank lines dropped. */
export function lineList(raw: string): string[] {
  return raw
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

/**
 * Form values -> API payload. Only the custom provider takes a config (blank
 * fields dropped so the model reports them missing); a blank client secret is
 * omitted; `shared` is only sent on create.
 */
export function toPayload(values: OauthAppFormValues, isNew: boolean, configKeys: string[]): OauthAppInput {
  const providerConfig: Record<string, string> = {};
  if (values.provider === CUSTOM_PROVIDER) {
    for (const key of configKeys) {
      const value = (values.provider_config[key] ?? "").trim();
      if (value) providerConfig[key] = value;
    }
  }
  const payload: OauthAppInput = {
    slug: values.slug.trim(),
    description: values.description,
    provider: values.provider,
    client_id: values.client_id.trim(),
    provider_config: providerConfig,
    allowed_scopes: lineList(values.allowed_scopes),
    enabled: values.enabled,
    always_available: values.always_available,
    labels: rowsToRecord(values.labels),
  };
  if (values.client_secret.trim() !== "") payload.client_secret = values.client_secret;
  if (isNew) payload.shared = values.shared;
  return payload;
}

/** Rails `"github".capitalize` -> "Github" (the legacy select labels). */
function capitalize(key: string) {
  return key.charAt(0).toUpperCase() + key.slice(1).toLowerCase();
}

interface OauthAppFormProps {
  app?: OauthApp;
  providers: OauthProvider[];
  submitLabel: string;
  cancelTo: string;
  onSubmit: (payload: OauthAppInput) => Promise<void>;
}

export function OauthAppForm({ app, providers, submitLabel, cancelTo, onSubmit }: OauthAppFormProps) {
  const isNew = !app;
  const form = useForm<OauthAppFormValues>({
    resolver: zodResolver(schema),
    defaultValues: initialValues(app, providers[0]?.key ?? ""),
  });
  const customKeys = providers.find((p) => p.key === CUSTOM_PROVIDER)?.config_keys ?? Object.keys(CONFIG_LABELS);
  // provider_config errors are set on the whole map (one model attribute).
  const configError = (form.formState.errors.provider_config as FieldError | undefined)?.message;

  const submit = form.handleSubmit(async (values) => {
    try {
      await onSubmit(toPayload(values, isNew, customKeys));
    } catch (error) {
      applyApiErrors(form.setError, error, FIELDS);
    } finally {
      // Never keep the write-only secret in form state after a submit.
      form.setValue("client_secret", "");
    }
  });

  return (
    <FormProvider {...form}>
      <form onSubmit={submit} className="space-y-6" noValidate>
        <FormSection title="Identity">
          <div className="grid gap-5 sm:grid-cols-2">
            <TextField
              control={form.control}
              name="slug"
              label="Slug *"
              placeholder="google"
              hint={
                <>
                  Globally unique. The app's whole identity, and the name in the well-known link:{" "}
                  <code>/oauth/&lt;slug&gt;/start</code>.
                </>
              }
            />
            <TextField control={form.control} name="description" label="Description" />
          </div>
        </FormSection>

        <FormSection title="Provider & OAuth Client">
          <div className="grid gap-5 sm:grid-cols-2">
            <SelectField
              control={form.control}
              name="provider"
              label="Provider *"
              options={providers.map((p) => ({ value: p.key, label: capitalize(p.key) }))}
            />
            <TextField
              control={form.control}
              name="client_id"
              label="Client ID *"
              hint="Not secret. Sent on every authorization and refresh."
            />
            <SecretField
              control={form.control}
              name="client_secret"
              label={isNew ? "Client secret *" : "Client secret"}
              hasStoredValue={!isNew && Boolean(app?.client_secret_set)}
              hint={isNew ? "Stored encrypted." : "Stored encrypted. Leave blank to keep the current secret."}
            />
          </div>
        </FormSection>

        <FormSection title="Custom Provider">
          <p className="mb-4 text-xs text-muted-foreground">
            Required for the custom provider only, and ignored by the others. Copy the values from the authorization
            server's metadata (<code>/.well-known/oauth-authorization-server</code>) and the protected resource's
            metadata (<code>/.well-known/oauth-protected-resource</code>).
          </p>
          <div className="grid gap-5 sm:grid-cols-2">
            {customKeys.map((key) => (
              <TextField
                key={key}
                control={form.control}
                name={`provider_config.${key}`}
                label={CONFIG_LABELS[key] ?? key}
                type="url"
                placeholder="https://"
              />
            ))}
          </div>
          {configError && <p className="mt-2 text-xs text-red-400 light:text-red-700">{configError}</p>}
          <p className="mt-2 text-xs text-muted-foreground">
            The token endpoint must accept <code>client_secret_post</code>. Identity comes from the userinfo{" "}
            <code>sub</code>, or from the access token's claims when no userinfo endpoint is set. The minted token is
            sent only to the resource URL's host and path.
          </p>
        </FormSection>

        <FormSection title="Flow Config">
          <div className="grid gap-5">
            <TextareaField
              control={form.control}
              name="allowed_scopes"
              label="Allowed scopes *"
              rows={3}
              placeholder="https://www.googleapis.com/auth/gmail.readonly"
              hint="One scope per line. A flow's requested scopes must be a subset; omitting them requests all of these."
            />
            <CheckboxField
              control={form.control}
              name="enabled"
              label="Enabled"
              hint="A disabled app rejects new consent flows; existing credentials keep refreshing."
            />
            <div>
              <CheckboxField
                control={form.control}
                name="shared"
                label="Shared: one admin-connected account; agents get access through roles."
                hint="Cannot be changed after the app is created."
                disabled={!isNew}
              />
              {form.formState.errors.shared?.message && (
                <p className="mt-1 text-xs text-red-400 light:text-red-700">{form.formState.errors.shared.message}</p>
              )}
            </div>
            <CheckboxField
              control={form.control}
              name="always_available"
              label="Always available to the connecting user"
              hint="Used automatically on any conversation turn they start, channels included. Off: automatic use covers only their DMs."
            />
          </div>
        </FormSection>

        <KeyValueRowsField control={form.control} name="labels" title="Labels" />

        <div className="flex items-center gap-3">
          <Button type="submit" disabled={form.formState.isSubmitting}>
            {submitLabel}
          </Button>
          <Button asChild variant="secondary">
            <Link to={cancelTo}>Cancel</Link>
          </Button>
        </div>
      </form>
    </FormProvider>
  );
}
