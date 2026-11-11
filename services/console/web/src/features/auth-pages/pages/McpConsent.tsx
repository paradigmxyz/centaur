// /mcp/oauth/consent?request=<ticket> — MCP OAuth consent. GET
// /mcp/oauth/authorize validated the client's request and issued the ticket;
// this page shows what is being authorized and submits the explicit decision
// as a classic same-origin form post (POST /mcp/oauth/consent). Rails answers
// with a 303 to the client's registered redirect_uri, so the authorization
// code never reaches JavaScript and this page never script-navigates to a
// client-supplied URL. A failed decision comes back as ?error=<code>, mapped
// to fixed text here.
import { PlugZap } from "lucide-react";
import { useRef, type FormEvent } from "react";
import { useSearchParams } from "react-router";

import { ErrorAlert } from "@/components/alerts";
import { useDocumentTitle } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";

import { useMcpAuthorization } from "../api";
import { AuthCard, AuthHeader, AuthLoading } from "../sections/AuthLayout";

const INVALID_REQUEST = "This authorization request is invalid or has expired. Start again from your MCP client.";

/** Decision failures (?error=) — codes only, never server- or URL-supplied text. */
const DECISION_ERRORS: Record<string, string> = {
  csrf: "Your session changed before the decision was sent. Start again from your MCP client.",
  invalid_decision: "Choose Allow or Deny.",
  invalid_consent_request: INVALID_REQUEST,
  invalid_authorization_request:
    "This MCP client's registration changed and the request is no longer valid. Start again from your MCP client.",
  server_error: "The authorization could not be completed. Start again from your MCP client.",
};

/** Only same-origin absolute paths may be a form action. */
function sameOriginPath(path: string): string | null {
  return /^\/[A-Za-z0-9/_-]*$/.test(path) && !path.startsWith("//") ? path : null;
}

function DetailRow({ label, children }: { label: string; children: string }) {
  return (
    <div>
      <dt className="text-xs font-medium text-muted-foreground">{label}</dt>
      <dd className="mt-1 font-mono text-[13px] break-all">{children}</dd>
    </div>
  );
}

export default function McpConsentPage() {
  useDocumentTitle("Authorize MCP Client");
  const [searchParams] = useSearchParams();
  const ticket = searchParams.get("request") ?? "";
  const errorCode = searchParams.get("error");
  const authorization = useMcpAuthorization(errorCode ? "" : ticket);

  let failure: string | null = null;
  if (errorCode) failure = DECISION_ERRORS[errorCode] ?? INVALID_REQUEST;
  else if (ticket === "") failure = INVALID_REQUEST;
  else if (authorization.isError) failure = errorMessage(authorization.error);

  // The ticket is single-use: ignore a second click while the first post is
  // in flight, or the second would land on the "already decided" error.
  const submitted = useRef(false);
  const onSubmit = (event: FormEvent) => {
    if (submitted.current) event.preventDefault();
    submitted.current = true;
  };

  const data = authorization.data;
  const action = data ? sameOriginPath(data.decision_path) : null;

  return (
    <>
      <AuthHeader subtitle="Authorize MCP access." />
      {failure ? (
        <AuthCard>
          <ErrorAlert className="mb-0">{failure}</ErrorAlert>
        </AuthCard>
      ) : !data ? (
        <AuthLoading />
      ) : (
        <AuthCard>
          <div className="space-y-5">
            <div className="flex items-center gap-3">
              <span className="grid size-10 shrink-0 place-items-center rounded-lg border bg-muted/50 text-muted-foreground">
                <PlugZap className="size-5" aria-hidden="true" />
              </span>
              <div className="min-w-0">
                <p className="truncate text-base font-semibold">{data.client.name}</p>
                <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground">{data.redirect_host}</p>
              </div>
            </div>

            <dl className="space-y-3 rounded-lg border bg-muted/30 p-4 text-sm">
              <DetailRow label="Resource">{data.resource}</DetailRow>
              <DetailRow label="Scope">{data.scopes.join(" ")}</DetailRow>
              <DetailRow label="Signed in as">{data.user.email}</DetailRow>
            </dl>
          </div>

          {action ? (
            <form method="post" action={action} className="mt-6" onSubmit={onSubmit}>
              <input type="hidden" name="authenticity_token" value={data.csrf_token} />
              <input type="hidden" name="request" value={ticket} />
              <div className="grid grid-cols-2 gap-3">
                <Button type="submit" name="decision" value="deny" variant="outline">
                  Deny
                </Button>
                <Button type="submit" name="decision" value="approve">
                  Allow
                </Button>
              </div>
            </form>
          ) : (
            <ErrorAlert className="mt-6 mb-0">{INVALID_REQUEST}</ErrorAlert>
          )}
        </AuthCard>
      )}
    </>
  );
}
