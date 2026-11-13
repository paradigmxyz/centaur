import { useController, type Control, type FieldPath, type FieldValues } from "react-hook-form";

import { FieldShell, fieldId } from "@/components/form/fields";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";

interface SecretFieldProps<T extends FieldValues> {
  control: Control<T>;
  name: FieldPath<T>;
  label: string;
  /** Whether the server already stores a value (e.g. `client_secret_set`). */
  hasStoredValue: boolean;
  hint?: string;
  multiline?: boolean;
  placeholder?: string;
}

/**
 * Write-only: never prefilled. A blank field keeps the stored value; strip it
 * with omitBlankSecrets before submitting.
 */
export function SecretField<T extends FieldValues>({
  control,
  name,
  label,
  hasStoredValue,
  hint,
  multiline,
  placeholder,
}: SecretFieldProps<T>) {
  const { field, fieldState } = useController({ control, name });
  const id = fieldId(name);
  const shownPlaceholder = hasStoredValue ? "•••••••• (unchanged)" : placeholder;
  const shownHint = hint ?? (hasStoredValue ? "Leave blank to keep the stored value." : undefined);
  const common = {
    id,
    name: field.name,
    value: (field.value as string | undefined) ?? "",
    onChange: field.onChange,
    onBlur: field.onBlur,
    ref: field.ref,
    placeholder: shownPlaceholder,
    autoComplete: "off",
    spellCheck: false,
    "aria-invalid": fieldState.invalid || undefined,
    "data-secret-field": "",
  };
  return (
    <FieldShell id={id} label={label} hint={shownHint} error={fieldState.error?.message}>
      {multiline ? (
        <Textarea {...common} rows={6} className="font-mono" />
      ) : (
        <Input {...common} type="password" autoComplete="new-password" />
      )}
    </FieldShell>
  );
}

export function omitBlankSecrets<T extends Record<string, unknown>>(values: T, keys: readonly (keyof T)[]): Partial<T> {
  const blank = (value: unknown) =>
    value === undefined || value === null || (typeof value === "string" && value.trim() === "");
  return Object.fromEntries(
    Object.entries(values).filter(([key, value]) => !(keys.includes(key as keyof T) && blank(value))),
  ) as Partial<T>;
}
