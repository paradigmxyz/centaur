import { useEffect } from "react";
import { useForm } from "react-hook-form";
import { toast } from "sonner";

import { CheckboxField, SelectField } from "@/components/form/fields";
import { SectionHeading } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";

import type { SandboxAccess } from "../api";
import type { SectionProps } from "./types";

const REPO_CACHE_OPTIONS = [
  { value: "none", label: "None" },
  { value: "public", label: "Public" },
  { value: "all", label: "All" },
];

const FLAGS: { name: Exclude<keyof SandboxAccess, "sandbox_repo_cache">; label: string; hint: string }[] = [
  { name: "sandbox_observability_enabled", label: "Observability", hint: "Allow sandbox access to logs and metrics surfaces." },
  { name: "sandbox_sessions_read_enabled", label: "Read sessions", hint: "Allow sandbox API tokens to read sessions and session events." },
  { name: "sandbox_workflows_read_enabled", label: "Read workflows", hint: "Allow sandbox API tokens to list workflow schedules and inspect runs." },
  { name: "sandbox_workflows_write_enabled", label: "Write workflows", hint: "Allow sandbox API tokens to create and cancel workflow runs." },
];

function accessOf(principal: SectionProps["principal"]): SandboxAccess {
  return {
    sandbox_repo_cache: principal.sandbox_repo_cache,
    sandbox_observability_enabled: principal.sandbox_observability_enabled,
    sandbox_sessions_read_enabled: principal.sandbox_sessions_read_enabled,
    sandbox_workflows_read_enabled: principal.sandbox_workflows_read_enabled,
    sandbox_workflows_write_enabled: principal.sandbox_workflows_write_enabled,
  };
}

export function SandboxAccessSection({ principal, mutations }: SectionProps) {
  const form = useForm<SandboxAccess>({ defaultValues: accessOf(principal) });
  const { reset } = form;
  useEffect(() => reset(accessOf(principal)), [principal, reset]);

  const onSubmit = form.handleSubmit(async (values) => {
    try {
      await mutations.updateSandboxAccess.mutateAsync(values);
      toast.success("Updated sandbox access.");
    } catch (error) {
      toast.error(errorMessage(error));
    }
  });

  return (
    <section className="mb-8" aria-labelledby="sandbox-access-heading">
      <div className="mb-4">
        <SectionHeading>
          <span id="sandbox-access-heading">Sandbox Access</span>
        </SectionHeading>
      </div>
      <div className="rounded-xl border bg-card p-5 shadow-xs">
        <form onSubmit={onSubmit} className="space-y-4">
          <SelectField
            control={form.control}
            name="sandbox_repo_cache"
            label="Repo cache"
            hint="Mount repo-cache backed workspace and tool sources."
            options={REPO_CACHE_OPTIONS}
          />
          {FLAGS.map((flag) => (
            <CheckboxField key={flag.name} control={form.control} name={flag.name} label={flag.label} hint={flag.hint} />
          ))}
          <Button type="submit" disabled={form.formState.isSubmitting}>
            Save sandbox access
          </Button>
        </form>
      </div>
    </section>
  );
}
