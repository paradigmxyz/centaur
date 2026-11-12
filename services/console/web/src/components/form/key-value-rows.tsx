import { Trash2 } from "lucide-react";
import { useFieldArray, useFormContext, type ArrayPath, type Control, type FieldValues, type Path } from "react-hook-form";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { FormSection } from "@/components/form/fields";

export interface KeyValueRow {
  key: string;
  value: string;
}

export function recordToRows(record: Record<string, unknown> | undefined): KeyValueRow[] {
  return Object.entries(record ?? {}).map(([key, value]) => ({ key, value: String(value ?? "") }));
}

/** Rows -> record, dropping rows with a blank key (KvRowParams semantics). */
export function rowsToRecord(rows: KeyValueRow[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const row of rows) {
    const key = row.key.trim();
    if (key) out[key] = row.value;
  }
  return out;
}

interface KeyValueRowsFieldProps<T extends FieldValues> {
  control: Control<T>;
  name: ArrayPath<T>;
  title: string;
  addLabel?: string;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
}

export function KeyValueRowsField<T extends FieldValues>({
  control,
  name,
  title,
  addLabel = "Add label",
  keyPlaceholder = "key",
  valuePlaceholder = "value",
}: KeyValueRowsFieldProps<T>) {
  const { fields, append, remove } = useFieldArray({ control, name });
  const { register } = useFormContext<T>();
  return (
    <FormSection
      title={title}
      actions={
        <Button type="button" variant="secondary" size="sm" onClick={() => append({ key: "", value: "" } as never)}>
          {addLabel}
        </Button>
      }
    >
      <div className="space-y-2">
        {fields.map((field, index) => (
          <div key={field.id} className="flex items-center gap-2">
            <Input
              aria-label={`${title} ${index + 1} key`}
              placeholder={keyPlaceholder}
              {...register(`${name}.${index}.key` as Path<T>)}
            />
            <Input
              aria-label={`${title} ${index + 1} value`}
              placeholder={valuePlaceholder}
              {...register(`${name}.${index}.value` as Path<T>)}
            />
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={`Remove ${title.toLowerCase()} ${index + 1}`}
              onClick={() => remove(index)}
            >
              <Trash2 />
            </Button>
          </div>
        ))}
      </div>
    </FormSection>
  );
}
