// /login — SSO buttons and the (optional) email + password form. A browser
// that is already signed in to an active account goes straight on.
import { useEffect, useState, type FormEvent } from "react";
import { useSearchParams } from "react-router";

import { ErrorAlert } from "@/components/alerts";
import { useDocumentTitle } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { errorMessage, navigation } from "@/lib/api";

import { useAuthConfig, useSignIn } from "../api";
import { safeLocalPath } from "../navigation";
import { AuthCard, AuthHeader, AuthLoading, AuthNotice } from "../sections/AuthLayout";

const labelClass = "mb-1 block text-xs font-semibold tracking-wider text-muted-foreground uppercase";

export default function LoginPage() {
  useDocumentTitle("Sign In");
  const [searchParams] = useSearchParams();
  const returnTo = searchParams.get("return_to") ?? searchParams.get("next");
  const config = useAuthConfig(returnTo);
  const signIn = useSignIn();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [fieldErrors, setFieldErrors] = useState<{ email?: string; password?: string }>({});

  const redirectTo = config.data?.redirect_to;
  useEffect(() => {
    if (redirectTo) navigation.assign(safeLocalPath(redirectTo));
  }, [redirectTo]);

  const onSubmit = (event: FormEvent) => {
    event.preventDefault();
    // The API treats a blank field as bad credentials; say what is missing instead.
    const errors = {
      email: email.trim() === "" ? "Email is required." : undefined,
      password: password === "" ? "Password is required." : undefined,
    };
    setFieldErrors(errors);
    if (errors.email || errors.password) return;
    signIn.mutate(
      { email: email.trim(), password },
      { onSuccess: (result) => navigation.assign(safeLocalPath(result.redirect_to)) },
    );
  };

  if (config.isPending || redirectTo) {
    return (
      <>
        <AuthHeader subtitle="Operator console sign in." />
        <AuthLoading />
      </>
    );
  }
  if (config.isError) {
    return (
      <>
        <AuthHeader subtitle="Operator console sign in." />
        <AuthCard>
          <ErrorAlert className="mb-0">{errorMessage(config.error)}</ErrorAlert>
        </AuthCard>
      </>
    );
  }

  const { providers, password_login: passwordLogin, flash } = config.data;
  const alert = signIn.isError ? errorMessage(signIn.error) : flash.alert;

  return (
    <>
      <AuthHeader subtitle="Operator console sign in." />
      {flash.notice && <AuthNotice>{flash.notice}</AuthNotice>}

      <AuthCard>
        {alert && <ErrorAlert>{alert}</ErrorAlert>}

        {providers.length > 0 && (
          <div className="space-y-3">
            {providers.map((provider) => (
              <a
                key={provider.key}
                href={provider.start_path}
                className="flex w-full items-center justify-center rounded-sm border border-border bg-muted/60 px-4 py-2 text-sm text-heading transition-colors hover:border-primary/40 hover:text-centaur-200 light:hover:text-primary"
              >
                Continue with {provider.label}
              </a>
            ))}
          </div>
        )}

        {providers.length > 0 && passwordLogin && (
          <div className="my-6 flex items-center gap-3 text-xs tracking-wider text-subtle-foreground uppercase">
            <span className="h-px flex-1 bg-border" />
            or sign in with email
            <span className="h-px flex-1 bg-border" />
          </div>
        )}

        {passwordLogin && (
          <form onSubmit={onSubmit} noValidate>
            <div className="space-y-5">
              <div>
                <Label htmlFor="login-email" className={labelClass}>
                  Email
                </Label>
                <Input
                  id="login-email"
                  name="email"
                  type="email"
                  placeholder="you@example.com"
                  autoComplete="username"
                  autoFocus
                  required
                  value={email}
                  aria-invalid={fieldErrors.email ? true : undefined}
                  aria-describedby={fieldErrors.email ? "login-email-error" : undefined}
                  onChange={(event) => setEmail(event.target.value)}
                />
                {fieldErrors.email && (
                  <p id="login-email-error" className="mt-1 text-xs text-red-400 light:text-red-700">
                    {fieldErrors.email}
                  </p>
                )}
              </div>
              <div>
                <Label htmlFor="login-password" className={labelClass}>
                  Password
                </Label>
                <Input
                  id="login-password"
                  name="password"
                  type="password"
                  autoComplete="current-password"
                  required
                  value={password}
                  aria-invalid={fieldErrors.password ? true : undefined}
                  aria-describedby={fieldErrors.password ? "login-password-error" : undefined}
                  onChange={(event) => setPassword(event.target.value)}
                />
                {fieldErrors.password && (
                  <p id="login-password-error" className="mt-1 text-xs text-red-400 light:text-red-700">
                    {fieldErrors.password}
                  </p>
                )}
              </div>
            </div>
            <div className="mt-6">
              <Button type="submit" className="w-full" disabled={signIn.isPending || signIn.isSuccess}>
                Sign in
              </Button>
            </div>
          </form>
        )}
      </AuthCard>
    </>
  );
}
