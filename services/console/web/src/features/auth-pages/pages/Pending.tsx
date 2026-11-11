// /pending — holding page for a signed-in account awaiting approval. Signed-out
// browsers go to /login and active accounts to their landing page.
import { Hourglass } from "lucide-react";
import { useEffect } from "react";

import { defaultLandingPath } from "@/app/session";
import { ErrorAlert } from "@/components/alerts";
import { useDocumentTitle } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import { errorMessage, navigation } from "@/lib/api";

import { usePendingSession, useSignOut } from "../api";
import { AuthCard, AuthHeader, AuthLoading } from "../sections/AuthLayout";

export default function PendingPage() {
  useDocumentTitle("Awaiting Approval");
  const session = usePendingSession();
  const signOut = useSignOut();
  const me = session.data;

  let target: string | null = null;
  if (session.isSuccess) {
    if (!me) target = "/login";
    else if (me.user.status === "active") target = defaultLandingPath(me);
  }
  useEffect(() => {
    if (target) navigation.assign(target);
  }, [target]);

  return (
    <>
      <AuthHeader subtitle="Operator console." />
      {session.isError ? (
        <AuthCard>
          <ErrorAlert className="mb-0">{errorMessage(session.error)}</ErrorAlert>
        </AuthCard>
      ) : !me || target ? (
        <AuthLoading />
      ) : (
        <AuthCard className="text-center">
          <div className="mx-auto mb-4 grid size-11 place-items-center rounded-full bg-warning/10 text-warning">
            <Hourglass className="size-5" aria-hidden="true" />
          </div>
          <h2 className="text-lg font-semibold">Awaiting approval</h2>
          <p className="mt-2 text-sm text-muted-foreground">
            You&apos;re signed in as <span className="font-medium text-foreground">{me.user.email}</span>, but your account is pending
            approval by an administrator. You&apos;ll have access once it&apos;s approved.
          </p>
          {signOut.isError && <ErrorAlert className="mt-4 mb-0 text-left">{errorMessage(signOut.error)}</ErrorAlert>}
          <div className="mt-6">
            <Button
              type="button"
              variant="outline"
              className="w-full"
              disabled={signOut.isPending || signOut.isSuccess}
              onClick={() => signOut.mutate(undefined, { onSuccess: () => navigation.assign("/login") })}
            >
              Sign out
            </Button>
          </div>
        </AuthCard>
      )}
    </>
  );
}
