import { ErrorAlert } from "@/components/alerts";
import { ApiError, errorMessage, humanize } from "@/lib/api";
import { pluralize } from "@/lib/format";

/** Legacy console/oauth_apps/_errors: "N errors prevented this app from saving:". */
export function OauthAppErrors({ error }: { error: unknown }) {
  if (!error) return null;
  const messages =
    error instanceof ApiError && error.status === 422 && Object.keys(error.fieldErrors).length > 0
      ? Object.entries(error.fieldErrors).flatMap(([key, list]) =>
          list.map((message) => (key === "base" ? message : `${humanize(key)} ${message}`)),
        )
      : [errorMessage(error)];
  return (
    <ErrorAlert className="max-w-4xl">
      <p className="mb-1 font-semibold">{pluralize(messages.length, "error")} prevented this app from saving:</p>
      <ul className="list-disc space-y-0.5 pl-5">
        {messages.map((message) => (
          <li key={message}>{message}</li>
        ))}
      </ul>
    </ErrorAlert>
  );
}
