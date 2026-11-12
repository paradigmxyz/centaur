import type { FieldValues, Path, UseFormSetError } from "react-hook-form";

import { ErrorAlert } from "@/components/alerts";
import { ApiError, errorMessage, humanize } from "@/lib/api";

/**
 * Maps a 422 `details` hash onto form fields. `fieldMap` renames server keys
 * (e.g. { foreign_id: "foreignId" }). Returns the messages that matched no
 * field so the caller can show them in a summary.
 */
export function applyApiErrors<T extends FieldValues>(
  setError: UseFormSetError<T>,
  error: unknown,
  fields: readonly string[],
  fieldMap: Record<string, string> = {},
): string[] {
  if (!(error instanceof ApiError) || error.status !== 422) return [errorMessage(error)];
  const unmatched: string[] = [];
  for (const [key, messages] of Object.entries(error.fieldErrors)) {
    const target = fieldMap[key] ?? key;
    if (fields.includes(target)) {
      setError(target as Path<T>, { type: "server", message: messages.join(", ") });
    } else {
      for (const message of messages) unmatched.push(key === "base" ? message : `${humanize(key)} ${message}`);
    }
  }
  if (Object.keys(error.fieldErrors).length === 0) unmatched.push(error.message);
  return unmatched;
}

export function FormErrorSummary({ error, subject, className = "max-w-4xl" }: { error: unknown; subject: string; className?: string }) {
  if (!error) return null;
  const messages =
    error instanceof ApiError && error.status === 422 && Object.keys(error.fieldErrors).length > 0
      ? Object.entries(error.fieldErrors).flatMap(([key, list]) =>
          list.map((message) => (key === "base" ? message : `${humanize(key)} ${message}`)),
        )
      : [errorMessage(error)];
  return (
    <ErrorAlert className={className}>
      <p className="font-semibold">{subject} could not be saved.</p>
      <ul className="mt-1 list-disc pl-5">
        {messages.map((message) => (
          <li key={message}>{message}</li>
        ))}
      </ul>
    </ErrorAlert>
  );
}
