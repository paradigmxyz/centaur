import { zodResolver } from "@hookform/resolvers/zod";
import { FormProvider, useForm } from "react-hook-form";
import { Link } from "react-router";
import { z } from "zod";

import { FieldShell, FormSection, TextField } from "@/components/form/fields";
import { applyApiErrors, FormErrorSummary } from "@/components/form/form-errors";
import { KeyValueRowsField, recordToRows, rowsToRecord } from "@/components/form/key-value-rows";
import { Button } from "@/components/ui/button";

import type { Role, RoleFormData } from "../api";

const schema = z.object({
  foreign_id: z
    .string()
    .trim()
    .regex(/^[A-Za-z0-9\-._~]*$/, "may only contain letters, digits, and - . _ ~"),
  name: z.string().trim(),
  labels: z.array(z.object({ key: z.string(), value: z.string() })),
});

type Values = z.infer<typeof schema>;
const FIELDS = ["foreign_id", "name", "labels"] as const;

interface RoleFormProps {
  /** The role being edited; absent on the new-role page. */
  role?: Role;
  /** Persist; throw an ApiError to show validation errors. */
  onSubmit: (data: RoleFormData) => Promise<unknown>;
  error: unknown;
}

/** There is no namespace field, and the foreign ID can only be set when creating. */
export function RoleForm({ role, onSubmit, error }: RoleFormProps) {
  const form = useForm<Values>({
    resolver: zodResolver(schema),
    defaultValues: {
      foreign_id: role?.foreign_id ?? "",
      name: role?.name ?? "",
      labels: recordToRows(role?.labels),
    },
  });

  const submit = form.handleSubmit(async (values) => {
    const data: RoleFormData = { name: values.name, labels: rowsToRecord(values.labels) };
    if (!role) data.foreign_id = values.foreign_id || null;
    try {
      await onSubmit(data);
    } catch (submitError) {
      applyApiErrors(form.setError, submitError, FIELDS);
    }
  });

  return (
    <>
      <FormErrorSummary error={error} subject="Role" />
      <FormProvider {...form}>
        <form onSubmit={submit} className="max-w-4xl space-y-6" noValidate>
          <FormSection title="Identity">
            <div className="grid gap-5 sm:grid-cols-2">
              {role ? (
                <FieldShell id="role-foreign-id" label="Foreign ID">
                  <div
                    id="role-foreign-id"
                    className="rounded-sm border border-input bg-muted px-3 py-2 text-sm text-muted-foreground"
                  >
                    {role.foreign_id || "—"}
                  </div>
                </FieldShell>
              ) : (
                <TextField
                  control={form.control}
                  name="foreign_id"
                  label="Foreign ID"
                  placeholder="stable-handle"
                  hint="Optional. A stable, URL-safe handle operators can reference."
                />
              )}
              <TextField control={form.control} name="name" label="Name" />
            </div>
          </FormSection>
          <KeyValueRowsField control={form.control} name="labels" title="Labels" />
          <div className="flex items-center gap-3">
            <Button type="submit" disabled={form.formState.isSubmitting}>
              {role ? "Save role" : "Create role"}
            </Button>
            <Button asChild variant="secondary">
              <Link to={role ? `/console/roles/${role.id}` : "/console/roles"}>Cancel</Link>
            </Button>
          </div>
        </form>
      </FormProvider>
    </>
  );
}
