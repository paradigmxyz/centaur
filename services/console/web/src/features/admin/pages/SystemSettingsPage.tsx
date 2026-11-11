import { useEffect, useState, type FormEvent } from "react";
import { Controller, useForm } from "react-hook-form";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { AppLink } from "@/components/app-link";
import { ControlTabs } from "@/components/control-tabs";
import { CheckboxField, FieldShell, FormSection, SelectField } from "@/components/form/fields";
import { applyApiErrors, FormErrorSummary } from "@/components/form/form-errors";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Textarea } from "@/components/ui/textarea";
import { errorMessage } from "@/lib/api";

import {
  useReplaceSlackExternalTeams,
  useSlackExternalTeams,
  useSystemSettings,
  useUpdateSystemSettings,
  type SystemSettings,
  type SystemSettingsInput,
} from "../api";

const REPO_CACHE_OPTIONS = [
  { value: "none", label: "None" },
  { value: "public", label: "Public" },
  { value: "all", label: "All" },
];

type Flag = Exclude<keyof SystemSettingsInput, "default_sandbox_repo_cache" | "default_role_ids">;

const FLAGS: { name: Flag; label: string; hint: string }[] = [
  {
    name: "default_sandbox_observability_enabled",
    label: "Observability",
    hint: "Allow new sandboxes to reach logs and metrics surfaces.",
  },
  {
    name: "default_sandbox_sessions_read_enabled",
    label: "Read sessions",
    hint: "Allow new sandbox API tokens to read sessions and session events.",
  },
  {
    name: "default_sandbox_workflows_read_enabled",
    label: "Read workflows",
    hint: "Allow new sandbox API tokens to list workflow schedules and inspect runs.",
  },
  {
    name: "default_sandbox_workflows_write_enabled",
    label: "Write workflows",
    hint: "Allow new sandbox API tokens to create and cancel workflow runs.",
  },
];

const FIELDS = ["default_sandbox_repo_cache", ...FLAGS.map((flag) => flag.name), "default_role_ids"] as const;

function inputOf(settings: SystemSettings): SystemSettingsInput {
  return {
    default_sandbox_repo_cache: settings.default_sandbox_repo_cache,
    default_sandbox_observability_enabled: settings.default_sandbox_observability_enabled,
    default_sandbox_sessions_read_enabled: settings.default_sandbox_sessions_read_enabled,
    default_sandbox_workflows_read_enabled: settings.default_sandbox_workflows_read_enabled,
    default_sandbox_workflows_write_enabled: settings.default_sandbox_workflows_write_enabled,
    default_role_ids: settings.default_role_ids,
  };
}

export default function SystemSettingsPageRoute() {
  return (
    <RequireAccess allowed={(me) => me.nav.settings}>
      <SystemSettingsPage />
    </RequireAccess>
  );
}

function SystemSettingsPage() {
  return (
    <>
      <ControlTabs />
      <PageHeader title="Settings" subtitle="Deployment-wide defaults for new control-plane records." />
      <DefaultsForm />
      <SlackConnectSection />
    </>
  );
}

function DefaultsForm() {
  const { data: settings } = useSystemSettings();
  const update = useUpdateSystemSettings();
  const form = useForm<SystemSettingsInput>({ defaultValues: inputOf(settings) });
  const { reset } = form;
  useEffect(() => reset(inputOf(settings)), [settings, reset]);

  const onSubmit = form.handleSubmit(async (values) => {
    try {
      await update.mutateAsync(values);
      toast.success("System settings updated.");
    } catch (error) {
      applyApiErrors(form.setError, error, FIELDS);
    }
  });

  return (
    <>
      <FormErrorSummary error={update.error} subject="Settings" />
      <form onSubmit={onSubmit} className="space-y-6" noValidate>
        <FormSection title="Default Sandbox Capabilities">
          <div className="grid gap-5 md:grid-cols-3">
            <SelectField
              control={form.control}
              name="default_sandbox_repo_cache"
              label="Repo cache"
              hint="Initial repo-cache access for new principals."
              options={REPO_CACHE_OPTIONS}
            />
            {FLAGS.map((flag) => (
              <CheckboxField
                key={flag.name}
                className="rounded-sm border border-border bg-muted/40 p-4"
                control={form.control}
                name={flag.name}
                label={flag.label}
                hint={flag.hint}
              />
            ))}
          </div>
        </FormSection>

        <FormSection title="Default Roles">
          <p className="mb-4 text-sm text-subtle-foreground">
            New principals with no preassigned roles receive the selected roles. Existing principals are unchanged.
          </p>
          {settings.roles.length > 0 ? (
            <Controller
              control={form.control}
              name="default_role_ids"
              render={({ field }) => (
                <fieldset className="grid max-w-md gap-3 rounded-sm border border-border bg-muted/40 p-4">
                  <legend className="sr-only">Default roles</legend>
                  {settings.roles.map((role) => {
                    const id = `default-role-${role.id}`;
                    const checked = field.value.includes(role.id);
                    return (
                      <div key={role.id} className="flex items-start gap-3">
                        <Checkbox
                          id={id}
                          className="mt-1"
                          checked={checked}
                          onCheckedChange={(next) =>
                            field.onChange(
                              next === true
                                ? settings.roles.filter((r) => r.id === role.id || field.value.includes(r.id)).map((r) => r.id)
                                : field.value.filter((value) => value !== role.id),
                            )
                          }
                        />
                        <label htmlFor={id} className="cursor-pointer">
                          <span className="block text-sm font-medium text-heading">
                            {role.name || role.foreign_id || role.id}
                          </span>
                          <span className="block text-xs text-subtle-foreground">{role.foreign_id || role.id}</span>
                        </label>
                      </div>
                    );
                  })}
                </fieldset>
              )}
            />
          ) : (
            <p className="text-sm text-subtle-foreground">Create a role before configuring default role assignments.</p>
          )}
          {form.formState.errors.default_role_ids?.message && (
            <p className="mt-1 text-xs text-red-400 light:text-red-700">{form.formState.errors.default_role_ids.message}</p>
          )}
        </FormSection>

        <div className="flex items-center gap-3">
          <Button type="submit" disabled={form.formState.isSubmitting}>
            Save settings
          </Button>
          <Button asChild variant="secondary">
            <AppLink to="/console/principals">Cancel</AppLink>
          </Button>
        </div>
      </form>
    </>
  );
}

function SlackConnectSection() {
  const teams = useSlackExternalTeams();
  const replace = useReplaceSlackExternalTeams();
  const [text, setText] = useState<string | null>(null);
  const value = text ?? teams.data?.join("\n") ?? "";

  const onSubmit = async (event: FormEvent) => {
    event.preventDefault();
    try {
      const body = await replace.mutateAsync(value);
      setText(body.data.slack_external_team_ids.join("\n"));
      toast.success("Slack Connect workspaces updated.");
    } catch {
      // Shown inline below (validation message from the API, or unavailable).
    }
  };

  return (
    <section id="slack-connect" aria-labelledby="slack-connect-heading" className="mt-6 rounded-sm border border-border bg-card p-5">
      <h2 id="slack-connect-heading" className="mb-3 text-xs font-semibold tracking-wider text-foreground uppercase">
        Slack Connect
      </h2>
      {replace.error ? <ErrorAlert className="mb-4">{errorMessage(replace.error)}</ErrorAlert> : null}
      {teams.isLoading ? (
        <p className="text-sm text-subtle-foreground">Loading…</p>
      ) : teams.isError ? (
        <ErrorAlert className="mb-0">
          The allowed external workspaces could not be loaded from the Centaur API. Reload the page to try again.
        </ErrorAlert>
      ) : (
        <form onSubmit={onSubmit} className="space-y-4">
          <FieldShell
            id="slack_external_team_ids"
            label="Allowed external workspaces"
            hint="Slack team ids, one per line or comma-separated. Centaur ignores messages and actions from any other external workspace."
            className="max-w-xl"
          >
            <Textarea
              id="slack_external_team_ids"
              rows={4}
              placeholder="T01234567"
              spellCheck={false}
              className="font-mono"
              value={value}
              onChange={(event) => setText(event.target.value)}
            />
          </FieldShell>
          <Button type="submit" disabled={replace.isPending}>
            Save Slack Connect workspaces
          </Button>
        </form>
      )}
    </section>
  );
}
