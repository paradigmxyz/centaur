import { zodResolver } from "@hookform/resolvers/zod";
import { useMemo } from "react";
import { Controller, useForm } from "react-hook-form";
import { Link } from "react-router";
import { z } from "zod";

import { ErrorAlert } from "@/components/alerts";
import { FieldShell, fieldId, TextField } from "@/components/form/fields";
import { applyApiErrors } from "@/components/form/form-errors";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { UserTypeahead, type UserOption } from "@/components/user-typeahead";
import { ApiError, errorMessage } from "@/lib/api";

import { useEditorCandidates, userLabel, type Skill, type SkillInput, type SkillUser } from "../api";
import { validationMessages } from "./helpers";

const schema = z.object({
  name: z.string().trim().min(1, "can't be blank").max(64, "is too long (maximum is 64 characters)"),
  description: z.string().trim().min(1, "can't be blank"),
  instructions: z.string().trim().min(1, "can't be blank"),
  editor_ids: z.array(z.string()),
});

type SkillFormValues = z.infer<typeof schema>;
const FIELDS = ["name", "description", "instructions"] as const;

const NEW_SKILL_INSTRUCTIONS = "# Instructions";

function skillFormValues(skill: Skill): SkillFormValues {
  return {
    name: skill.name,
    description: skill.description,
    instructions: skill.instructions,
    editor_ids: skill.editors.map((editor) => editor.id),
  };
}

interface SkillFormProps {
  /** The skill being edited; undefined for New. */
  skill?: Skill;
  /** Whether the caller may manage editors (owner, or anyone creating a skill). */
  canManageEditors: boolean;
  /** Called with the request body; throw the ApiError to show it on the form. */
  onSubmit: (input: SkillInput) => Promise<void>;
  cancelTo: string;
}

export function SkillForm({ skill, canManageEditors, onSubmit, cancelTo }: SkillFormProps) {
  const form = useForm<SkillFormValues>({
    resolver: zodResolver(schema),
    defaultValues: skill
      ? skillFormValues(skill)
      : { name: "", description: "", instructions: NEW_SKILL_INSTRUCTIONS, editor_ids: [] },
  });
  const errors = form.formState.errors;
  const serverError = errors.root?.server?.message;

  const submit = form.handleSubmit(async (values) => {
    const input: SkillInput = {
      name: values.name,
      description: values.description,
      instructions: values.instructions,
    };
    if (skill) input.lock_version = skill.lock_version;
    if (canManageEditors) input.editor_ids = values.editor_ids;
    try {
      await onSubmit(input);
    } catch (error) {
      if (error instanceof ApiError && error.status === 409) return;
      applyApiErrors(form.setError, error, FIELDS);
      form.setError("root.server", { type: "server", message: validationMessages(error).join("\n") });
    }
  });

  return (
    <form onSubmit={submit} className="max-w-5xl space-y-6 rounded-xl border bg-card p-6 shadow-xs" noValidate>
      {serverError && (
        <ErrorAlert>
          <p className="font-semibold">Could not save this skill:</p>
          <ul className="mt-2 list-disc space-y-1 pl-5">
            {serverError.split("\n").map((message) => (
              <li key={message}>{message}</li>
            ))}
          </ul>
        </ErrorAlert>
      )}

      <div className="grid gap-5 lg:grid-cols-2">
        <TextField
          control={form.control}
          name="name"
          label="Name"
          placeholder="incident-triage"
          autoComplete="off"
          hint="Globally unique. Use lowercase letters, numbers, and hyphens."
        />
        <TextField
          control={form.control}
          name="description"
          label="Description"
          placeholder="What this skill does and when an agent should use it"
          hint="Agents use this description to decide when the skill applies."
        />
      </div>

      {canManageEditors ? (
        <Controller
          control={form.control}
          name="editor_ids"
          render={({ field }) => (
            <EditorPicker value={field.value} onChange={field.onChange} current={skill?.editors ?? []} />
          )}
        />
      ) : (
        <div>
          <h2 className="mb-2 text-sm font-medium">Editors</h2>
          <div className="space-y-2">
            {(skill?.editors ?? []).map((editor) => (
              <div key={editor.id} className="rounded-lg border bg-muted/40 px-3 py-2 text-sm">
                {userLabel(editor)}
              </div>
            ))}
          </div>
          <p className="mt-2 text-xs text-muted-foreground">Only the skill owner can manage editors.</p>
        </div>
      )}

      <Controller
        control={form.control}
        name="instructions"
        render={({ field, fieldState }) => (
          <FieldShell
            id={fieldId("instructions")}
            label="Instructions"
            error={fieldState.error?.message}
            hint="Markdown instructions loaded when the skill is selected. The complete generated SKILL.md may be up to 64 KiB."
          >
            <Textarea
              id={fieldId("instructions")}
              rows={24}
              spellCheck={false}
              aria-invalid={fieldState.invalid || undefined}
              className="min-h-[32rem] resize-y font-mono text-sm leading-6"
              {...field}
            />
          </FieldShell>
        )}
      />

      <div className="flex flex-wrap items-center gap-3">
        <Button type="submit" disabled={form.formState.isSubmitting}>
          {skill ? "Save skill" : "Create skill"}
        </Button>
        <Button asChild variant="secondary">
          <Link to={cancelTo}>Cancel</Link>
        </Button>
      </div>
    </form>
  );
}

function EditorPicker({
  value,
  onChange,
  current,
}: {
  value: string[];
  onChange: (ids: string[]) => void;
  current: SkillUser[];
}) {
  const candidates = useEditorCandidates(true);
  const options = useMemo<UserOption[]>(() => {
    const byId = new Map<string, SkillUser>();
    for (const user of [...current, ...(candidates.data ?? [])]) byId.set(user.id, user);
    return [...byId.values()]
      .sort((a, b) => a.email.localeCompare(b.email))
      .map((user) => ({ id: user.id, label: userLabel(user), searchText: userLabel(user).toLowerCase() }));
  }, [candidates.data, current]);

  return (
    <div>
      <p className="mb-2 block text-sm font-medium">Editors</p>
      {candidates.isError && (
        <ErrorAlert className="mb-2 max-w-xl">Could not load console users: {errorMessage(candidates.error)}</ErrorAlert>
      )}
      <div className="max-w-xl">
        <UserTypeahead
          options={options}
          value={value}
          onChange={onChange}
          label="Editors"
          placeholder="Search console users by name or email"
          emptyText="No additional editors."
        />
      </div>
      <p className="mt-2 text-xs text-muted-foreground">
        Editors can read and update this skill. Their names and emails are visible to anyone who can view the skill.
        Only you can manage editors, visibility, or archiving.
      </p>
    </div>
  );
}
