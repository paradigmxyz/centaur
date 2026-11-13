import { zodResolver } from "@hookform/resolvers/zod";
import { useQueryClient } from "@tanstack/react-query";
import { FormProvider, useForm } from "react-hook-form";
import { Link, useNavigate } from "react-router";
import { toast } from "sonner";
import { z } from "zod";

import { BackLink } from "@/components/back-link";
import { TextField, FormSection } from "@/components/form/fields";
import { applyApiErrors, FormErrorSummary } from "@/components/form/form-errors";
import { KeyValueRowsField, rowsToRecord } from "@/components/form/key-value-rows";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { useSubmit } from "@/lib/use-submit";

import { createPrincipal, type NewPrincipal } from "../api";

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

export default function PrincipalNewPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.principals}>
      <PrincipalNew />
    </RequireAccess>
  );
}

function PrincipalNew() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const create = useSubmit((data: NewPrincipal) => createPrincipal(queryClient, data));
  const form = useForm<Values>({
    resolver: zodResolver(schema),
    defaultValues: { foreign_id: "", name: "", labels: [] },
  });

  const onSubmit = form.handleSubmit(async (values) => {
    try {
      const body = await create.run({
        foreign_id: values.foreign_id || null,
        name: values.name,
        labels: rowsToRecord(values.labels),
      });
      toast.success("Principal created.");
      navigate(`/console/principals/${body.data.id}`);
    } catch (error) {
      applyApiErrors(form.setError, error, FIELDS);
    }
  });

  return (
    <>
      <div className="mb-2">
        <BackLink to="/console/principals">back to principals</BackLink>
      </div>
      <PageHeader title="New Principal" />
      <FormErrorSummary error={create.error} subject="Principal" />
      <FormProvider {...form}>
        <form onSubmit={onSubmit} className="max-w-4xl space-y-6" noValidate>
          <FormSection title="Identity">
            <div className="grid gap-5 sm:grid-cols-2">
              <TextField
                control={form.control}
                name="foreign_id"
                label="Foreign ID"
                placeholder="slack-channel-T123-C456"
                hint="Optional. A stable, URL-safe handle for the service identity."
              />
              <TextField control={form.control} name="name" label="Display name" />
            </div>
          </FormSection>
          <KeyValueRowsField control={form.control} name="labels" title="Labels" />
          <div className="flex items-center gap-3">
            <Button type="submit" disabled={form.formState.isSubmitting}>
              Add Principal
            </Button>
            <Button asChild variant="secondary">
              <Link to="/console/principals">Cancel</Link>
            </Button>
          </div>
        </form>
      </FormProvider>
    </>
  );
}
