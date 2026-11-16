import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { Controller, useForm, useWatch } from "react-hook-form";
import { useNavigate } from "react-router";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { AppLink } from "@/components/app-link";
import { FieldShell, fieldId, FormSection } from "@/components/form/fields";
import { applyApiErrors } from "@/components/form/form-errors";
import { PageHeader } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { ApiError } from "@/lib/api";

import { saveSshKey, usePersonalSshKey, validateSshKey } from "../api";
import { INTEGRATIONS_PATH, validationMessages } from "../sections/helpers";

interface Values {
  name: string;
  private_key: string;
}

const INITIAL_FEEDBACK =
  "Paste the complete private key, including the BEGIN and END lines. Validation does not save it.";
const UNAVAILABLE_FEEDBACK = "Could not check the key. Save to validate it, or try again.";

interface Feedback {
  text: string;
  invalid?: boolean;
}

/**
 * Live check of a pasted key: 500ms debounce, stale requests aborted, nothing
 * saved. The key is only sent to the validate endpoint, whose responses never
 * echo it, and never enters the query cache; results are matched to the input
 * by an edit counter (`version`) rather than by storing the key.
 */
function useKeyFeedback(privateKey: string, version: number, saved: boolean): Feedback {
  const [result, setResult] = useState<{ version: number; feedback: Feedback } | null>(null);
  const blank = privateKey.trim() === "";

  useEffect(() => {
    if (blank) return;
    const controller = new AbortController();
    const timer = setTimeout(async () => {
      try {
        const body = await validateSshKey(privateKey, controller.signal);
        const { message, fingerprint } = body.data;
        setResult({
          version,
          feedback: { text: `${message} Fingerprint: ${fingerprint}. Not saved yet—click Save SSH key.`, invalid: false },
        });
      } catch (error) {
        if (controller.signal.aborted) return;
        const feedback =
          error instanceof ApiError && error.status === 422
            ? { text: error.message, invalid: true }
            : { text: UNAVAILABLE_FEEDBACK };
        setResult({ version, feedback });
      }
    }, 500);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [privateKey, version, blank]);

  // The initial hint stays until the field is first edited, even if cleared again.
  if (version === 0) return { text: INITIAL_FEEDBACK };
  if (blank) {
    return {
      text: saved
        ? "Leave blank to keep your saved key."
        : "Paste the complete private key, including the BEGIN and END lines.",
    };
  }
  if (result?.version === version) return result.feedback;
  return { text: "Checking key…" };
}

// Any active user manages their own key; the API scopes it to the caller.
export default function SshKeyEditPage() {
  const { data: key } = usePersonalSshKey();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const savedKey = key.saved && key.has_private_key;
  const form = useForm<Values>({ defaultValues: { name: key.name, private_key: "" } });
  const privateKey = useWatch({ control: form.control, name: "private_key" });
  const [keyVersion, setKeyVersion] = useState(0);
  const feedback = useKeyFeedback(privateKey, keyVersion, savedKey);
  const [errors, setErrors] = useState<string[]>([]);

  const onSubmit = form.handleSubmit(async (values) => {
    setErrors([]);
    const data: { name: string; private_key?: string } = { name: values.name };
    if (values.private_key.trim() !== "") data.private_key = values.private_key;
    try {
      await saveSshKey(queryClient, data);
      toast.success("SSH key saved. Register the public key in your environments.");
      navigate("/console/ssh_key");
    } catch (error) {
      applyApiErrors(form.setError, error, ["name", "private_key"]);
      setErrors(validationMessages(error));
    } finally {
      // Drop the plaintext from the form field. The user re-pastes after an error.
      form.setValue("private_key", "");
      setKeyVersion((current) => current + 1);
    }
  });

  const nameError = form.formState.errors.name?.message;
  const keyError = form.formState.errors.private_key?.message;
  const keyInvalid = Boolean(keyError) || feedback.invalid;

  return (
    <>
      <PageHeader
        title="Connect an SSH key"
        documentTitle="Configure SSH key"
        subtitle="Save your key once. Choose a permitted environment URL when you ask Centaur to connect."
      />
      <div className="max-w-3xl">
        {errors.length > 0 && (
          <ErrorAlert>
            <ul className="list-disc pl-5">
              {errors.map((message) => (
                <li key={message}>{message}</li>
              ))}
            </ul>
          </ErrorAlert>
        )}
        <form onSubmit={onSubmit} className="max-w-4xl space-y-6" noValidate>
          <FormSection title="Your key">
            <FieldShell id={fieldId("name")} label="Name" error={nameError}>
              <Input
                id={fieldId("name")}
                maxLength={100}
                placeholder="My SSH key"
                aria-invalid={Boolean(nameError) || undefined}
                {...form.register("name", { required: "can't be blank" })}
              />
            </FieldShell>
          </FormSection>

          <FormSection title="Keys">
            <Controller
              control={form.control}
              name="private_key"
              rules={{ validate: (value) => savedKey || value.trim() !== "" || "can't be blank" }}
              render={({ field }) => (
                <FieldShell id={fieldId("private_key")} label="Your private SSH key" error={keyError}>
                  <Textarea
                    id={fieldId("private_key")}
                    rows={6}
                    className="font-mono"
                    autoComplete="off"
                    spellCheck={false}
                    data-secret-field=""
                    aria-invalid={keyInvalid ? true : undefined}
                    aria-describedby="ssh-key-feedback"
                    placeholder={savedKey ? "Leave blank to keep your saved key" : "Paste your dedicated private SSH key"}
                    {...field}
                    onChange={(event) => {
                      field.onChange(event);
                      setKeyVersion((current) => current + 1);
                    }}
                  />
                </FieldShell>
              )}
            />
            <p id="ssh-key-feedback" role="status" aria-live="polite" className="mt-1 text-xs text-muted-foreground">
              {feedback.text}
            </p>
            <p className="mt-1 text-xs text-muted-foreground">
              Use a dedicated key without a passphrase. Stored encrypted and never shown again. Centaur&apos;s sandbox
              receives a disposable key; the proxy holds your real key.
            </p>
          </FormSection>

          <FormSection title="Access">
            <p className="mb-4 text-xs text-muted-foreground">
              Your administrator controls which SSH destinations are allowed. Give Centaur the environment URL in your
              request; this key can be reused across allowed destinations.
            </p>
            <p className="text-xs text-muted-foreground">
              Saving grants this key to your Console sessions and, with a verified Slack identity linked to your account,
              your DMs with the bot. Channels can use it only if an administrator grants it to them.
            </p>
          </FormSection>

          <div className="flex flex-wrap gap-3">
            <Button type="submit" disabled={form.formState.isSubmitting}>
              {form.formState.isSubmitting ? "Saving…" : "Save SSH key"}
            </Button>
            <Button asChild variant="secondary">
              <AppLink to={INTEGRATIONS_PATH}>Cancel</AppLink>
            </Button>
          </div>
        </form>
      </div>
    </>
  );
}
