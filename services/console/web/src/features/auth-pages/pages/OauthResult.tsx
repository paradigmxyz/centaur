// /oauth/result — where a failed broker OAuth consent (/oauth/<slug>/start or
// /callback) lands. The outcome arrives once through GET /api/v1/auth/config;
// it never travels in the URL, so a crafted link cannot put text on this page.
import { ErrorAlert } from "@/components/alerts";
import { useDocumentTitle } from "@/components/page-header";
import { errorMessage } from "@/lib/api";
import lockup from "@/assets/centaur-lockup-white.svg";

import { useAuthConfig } from "../api";
import { isOauthStartPath } from "../navigation";
import { AuthCard, AuthLoading } from "../sections/AuthLayout";

const EXPIRED_MESSAGE = "This result is no longer available. Start the connection again from the console.";

export default function OauthResultPage() {
  const config = useAuthConfig();
  const result = config.data?.oauth_result ?? null;
  const denied = result?.kind === "denied";
  const heading = denied ? "Not connected" : "Something went wrong";
  useDocumentTitle(config.isPending ? undefined : heading);

  if (config.isPending) return <AuthLoading />;

  if (config.isError) {
    return (
      <AuthCard>
        <h1 className="mb-3 text-lg font-bold text-red-300 light:text-red-700">Something went wrong</h1>
        <ErrorAlert className="mb-0">{errorMessage(config.error)}</ErrorAlert>
      </AuthCard>
    );
  }

  // Only a successful response without a result means "already shown".
  const message = result?.message ?? EXPIRED_MESSAGE;
  const retryPath = isOauthStartPath(result?.retry_path) ? result.retry_path : null;

  return (
    <AuthCard>
      <div className="mb-3">
        <img src={lockup} alt="Centaur" width={497} height={127} className="logo-themed h-6 w-auto" />
      </div>
      <h1
        className={
          denied
            ? "text-lg font-bold text-amber-300 light:text-amber-700"
            : "text-lg font-bold text-red-300 light:text-red-700"
        }
      >
        {heading}
      </h1>
      <p className="mt-3 text-sm text-muted-foreground">{message}</p>
      {retryPath ? (
        <a href={retryPath} className="mt-4 inline-block text-sm text-primary hover:underline">
          Try again
        </a>
      ) : (
        !result && (
          <a href="/console/integrations" className="mt-4 inline-block text-sm text-primary hover:underline">
            Go to Integrations
          </a>
        )
      )}
    </AuthCard>
  );
}
