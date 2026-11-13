import { zodResolver } from "@hookform/resolvers/zod";
import { useState } from "react";
import { Controller, useForm } from "react-hook-form";
import { Link } from "react-router";
import { z } from "zod";

import { FormSection, TextField } from "@/components/form/fields";
import { applyApiErrors, FormErrorSummary } from "@/components/form/form-errors";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";

import { API_CLIENT_SCOPES, type ApiClientInput } from "../api";

const schema = z.object({
  name: z.string().trim().min(1, "can't be blank"),
  principal: z.string().trim(),
  scopes: z.array(z.string()),
});

type Values = z.infer<typeof schema>;
const FIELDS = ["name", "principal", "scopes"] as const;

interface ApiClientFormProps {
  /** New clients pick an optional default principal; existing ones manage principals on the detail page. */
  mode: "new" | "edit";
  defaults: { name: string; scopes: string[] };
  cancelTo: string;
  onSubmit: (input: ApiClientInput) => Promise<void>;
}

export function ApiClientForm({ mode, defaults, cancelTo, onSubmit }: ApiClientFormProps) {
  const form = useForm<Values>({
    resolver: zodResolver(schema),
    defaultValues: { name: defaults.name, principal: "", scopes: defaults.scopes },
  });

  // The last submit error, for the summary (the server response only).
  const [error, setError] = useState<unknown>(null);

  const submit = form.handleSubmit(async (values) => {
    setError(null);
    try {
      await onSubmit({
        name: values.name,
        scopes: values.scopes,
        ...(mode === "new" && values.principal ? { principal: values.principal } : {}),
      });
    } catch (caught) {
      setError(caught);
      applyApiErrors(form.setError, caught, FIELDS);
    }
  });

  const scopesError = form.formState.errors.scopes?.message;

  return (
    <>
      <FormErrorSummary error={error} subject="API client" />
      <form onSubmit={submit} className="max-w-4xl space-y-6" noValidate>
        <FormSection title="Client">
          <div className="grid gap-5 sm:grid-cols-2">
            <TextField control={form.control} name="name" label="Name *" placeholder="eval-runner" />
            {mode === "new" && (
              <TextField
                className="sm:col-span-2"
                control={form.control}
                name="principal"
                label="Default principal"
                placeholder="prn_… or foreign ID"
                hint="Principal OID or foreign ID. Sessions use it unless they name another attached principal. You can attach more after creating the client."
              />
            )}
          </div>
        </FormSection>

        <FormSection title="Scopes">
          <Controller
            control={form.control}
            name="scopes"
            render={({ field }) => (
              <fieldset className="grid gap-3" aria-describedby="api-client-scopes-hint">
                <legend className="sr-only">Scopes</legend>
                {API_CLIENT_SCOPES.map((scope) => {
                  const id = `api-client-scope-${scope.replace(/[^a-z]/g, "-")}`;
                  return (
                    <div key={scope} className="flex items-center gap-2">
                      <Checkbox
                        id={id}
                        checked={field.value.includes(scope)}
                        onCheckedChange={(checked) =>
                          field.onChange(
                            checked === true
                              ? API_CLIENT_SCOPES.filter((s) => s === scope || field.value.includes(s))
                              : field.value.filter((s) => s !== scope),
                          )
                        }
                      />
                      <label htmlFor={id} className="cursor-pointer text-sm font-medium">
                        {scope}
                      </label>
                    </div>
                  );
                })}
              </fieldset>
            )}
          />
          {scopesError && <p className="mt-1 text-xs text-destructive">{scopesError}</p>}
          <p id="api-client-scopes-hint" className="mt-1 text-xs text-muted-foreground">
            sessions:write creates sessions and starts turns; sessions:read reads session state and events.
          </p>
        </FormSection>

        <div className="flex items-center gap-3">
          <Button type="submit" disabled={form.formState.isSubmitting}>
            {mode === "new" ? "Create API client" : "Save API client"}
          </Button>
          <Button asChild variant="secondary">
            <Link to={cancelTo}>Cancel</Link>
          </Button>
        </div>
      </form>
    </>
  );
}
