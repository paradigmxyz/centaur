// Form field patterns for react-hook-form (+ zod via @hookform/resolvers).
//
//   const form = useForm<Values>({ resolver: zodResolver(schema), defaultValues });
//   <TextField control={form.control} name="name" label="Display name" />
//
// Server validation errors (422 details) map onto fields with applyApiErrors.
import type { ReactNode } from "react";
import { useController, type Control, type FieldPath, type FieldValues } from "react-hook-form";

import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";

interface BaseFieldProps<T extends FieldValues> {
  control: Control<T>;
  name: FieldPath<T>;
  label: ReactNode;
  hint?: ReactNode;
  className?: string;
  disabled?: boolean;
}

export function fieldId(name: string) {
  return `field-${name.replace(/[^a-zA-Z0-9_-]/g, "-")}`;
}

/** Label + control + hint + inline error (legacy form-label/form-hint/field-error). */
export function FieldShell({
  id,
  label,
  hint,
  error,
  className,
  children,
}: {
  id: string;
  label: ReactNode;
  hint?: ReactNode;
  error?: string;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div className={className}>
      <Label htmlFor={id} className="mb-2 block">
        {label}
      </Label>
      {children}
      {error && (
        <p id={`${id}-error`} className="mt-1.5 text-xs text-destructive">
          {error}
        </p>
      )}
      {hint && <p className="mt-1.5 text-xs text-muted-foreground">{hint}</p>}
    </div>
  );
}

export function TextField<T extends FieldValues>({
  control,
  name,
  label,
  hint,
  className,
  disabled,
  placeholder,
  type = "text",
  autoComplete,
}: BaseFieldProps<T> & { placeholder?: string; type?: string; autoComplete?: string }) {
  const { field, fieldState } = useController({ control, name });
  const id = fieldId(name);
  return (
    <FieldShell id={id} label={label} hint={hint} error={fieldState.error?.message} className={className}>
      <Input
        id={id}
        type={type}
        placeholder={placeholder}
        autoComplete={autoComplete}
        disabled={disabled}
        aria-invalid={fieldState.invalid || undefined}
        aria-describedby={fieldState.error ? `${id}-error` : undefined}
        {...field}
        value={field.value ?? ""}
      />
    </FieldShell>
  );
}

export function TextareaField<T extends FieldValues>({
  control,
  name,
  label,
  hint,
  className,
  disabled,
  placeholder,
  rows = 4,
}: BaseFieldProps<T> & { placeholder?: string; rows?: number }) {
  const { field, fieldState } = useController({ control, name });
  const id = fieldId(name);
  return (
    <FieldShell id={id} label={label} hint={hint} error={fieldState.error?.message} className={className}>
      <Textarea
        id={id}
        rows={rows}
        placeholder={placeholder}
        disabled={disabled}
        aria-invalid={fieldState.invalid || undefined}
        {...field}
        value={field.value ?? ""}
      />
    </FieldShell>
  );
}

export interface SelectOption {
  value: string;
  label: string;
}

export function SelectField<T extends FieldValues>({
  control,
  name,
  label,
  hint,
  className,
  disabled,
  options,
  placeholder,
}: BaseFieldProps<T> & { options: SelectOption[]; placeholder?: string }) {
  const { field, fieldState } = useController({ control, name });
  const id = fieldId(name);
  return (
    <FieldShell id={id} label={label} hint={hint} error={fieldState.error?.message} className={className}>
      <Select value={field.value ?? ""} onValueChange={field.onChange} disabled={disabled}>
        <SelectTrigger id={id} className="w-full" aria-invalid={fieldState.invalid || undefined} onBlur={field.onBlur}>
          <SelectValue placeholder={placeholder} />
        </SelectTrigger>
        <SelectContent>
          {options.map((option) => (
            <SelectItem key={option.value} value={option.value}>
              {option.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </FieldShell>
  );
}

/** Checkbox with a bold label and a muted description underneath. */
export function CheckboxField<T extends FieldValues>({
  control,
  name,
  label,
  hint,
  className,
  disabled,
}: BaseFieldProps<T>) {
  const { field } = useController({ control, name });
  const id = fieldId(name);
  return (
    <div className={cn("flex items-start gap-3", className)}>
      <Checkbox
        id={id}
        className="mt-0.5"
        checked={Boolean(field.value)}
        onCheckedChange={(checked) => field.onChange(checked === true)}
        onBlur={field.onBlur}
        disabled={disabled}
        aria-describedby={hint ? `${id}-hint` : undefined}
      />
      <div>
        <label htmlFor={id} className="block cursor-pointer text-sm leading-5 font-medium">
          {label}
        </label>
        {hint && (
          <span id={`${id}-hint`} className="mt-0.5 block text-xs text-muted-foreground">
            {hint}
          </span>
        )}
      </div>
    </div>
  );
}

/** A bordered card grouping related fields (legacy .form-section). */
export function FormSection({
  title,
  actions,
  children,
  className,
}: {
  title: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cn("rounded-xl border bg-card p-5 shadow-xs", className)}>
      <div className="mb-4 flex items-center justify-between gap-3">
        <h2 className="text-sm font-semibold">{title}</h2>
        {actions}
      </div>
      {children}
    </section>
  );
}
