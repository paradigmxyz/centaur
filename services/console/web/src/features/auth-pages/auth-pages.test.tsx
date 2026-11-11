import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { buildRoutes } from "@/app/router";
import { navigation, setCsrfToken } from "@/lib/api";
import { adminMe, apiResponse, mockApi, operatorMe, renderRoute } from "@/test/utils";

import authPages from "./index";
import { isOauthStartPath, safeLocalPath } from "./navigation";
import LoginPage from "./pages/Login";
import McpConsentPage from "./pages/McpConsent";
import OauthResultPage from "./pages/OauthResult";
import PendingPage from "./pages/Pending";

let assign: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  assign = vi.spyOn(navigation, "assign").mockImplementation(() => {});
  setCsrfToken(null);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function authConfig(overrides: Record<string, unknown> = {}) {
  return {
    data: {
      providers: [],
      password_login: true,
      csrf_token: "login-csrf",
      signed_in: false,
      redirect_to: null,
      flash: {},
      oauth_result: null,
      ...overrides,
    },
  };
}

describe("auth pages module", () => {
  it("registers the public routes outside the shell", () => {
    const paths = (authPages.publicRoutes ?? []).flatMap((route) => (route.children ?? []).map((child) => child.path));
    expect(paths).toEqual(["/login", "/pending", "/oauth/result", "/mcp/oauth/consent"]);
    expect(authPages.routes).toBeUndefined();
    // Public routes come before the shell in the router.
    expect(buildRoutes([authPages])[0]).toBe(authPages.publicRoutes?.[0]);
  });
});

describe("login page", () => {
  it("shows the SSO buttons, divider, password form, and the handed-off flash", async () => {
    const api = mockApi({
      "GET /api/v1/auth/config": authConfig({
        providers: [
          { key: "google", label: "Google", start_path: "/auth/google/start" },
          { key: "okta", label: "Okta", start_path: "/auth/okta/start" },
        ],
        flash: { notice: "Signed out.", alert: "Sign in was canceled." },
      }),
    });
    renderRoute(<LoginPage />, { path: "/login", url: "/login?return_to=%2Fconsole%2Fcredentials%3Fkind%3Doauth" });

    expect(await screen.findByRole("link", { name: "Continue with Google" })).toHaveAttribute("href", "/auth/google/start");
    expect(screen.getByRole("link", { name: "Continue with Okta" })).toHaveAttribute("href", "/auth/okta/start");
    expect(screen.getByText("or sign in with email")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Signed out.");
    expect(screen.getByRole("alert")).toHaveTextContent("Sign in was canceled.");
    expect(screen.getByText("Operator console sign in.")).toBeInTheDocument();
    expect(screen.getByLabelText("Email")).toHaveAttribute("autocomplete", "username");
    expect(screen.getByLabelText("Password")).toHaveAttribute("autocomplete", "current-password");
    expect(api.calls("GET /api/v1/auth/config")[0]?.url.searchParams.get("return_to")).toBe("/console/credentials?kind=oauth");
    expect(document.title).toBe("Sign In · Centaur Console");
  });

  it("hides the password form and divider when password login is disabled", async () => {
    mockApi({
      "GET /api/v1/auth/config": authConfig({
        password_login: false,
        providers: [{ key: "google", label: "Google", start_path: "/auth/google/start" }],
      }),
    });
    renderRoute(<LoginPage />, { path: "/login" });

    expect(await screen.findByRole("link", { name: "Continue with Google" })).toBeInTheDocument();
    expect(screen.queryByLabelText("Email")).not.toBeInTheDocument();
    expect(screen.queryByText("or sign in with email")).not.toBeInTheDocument();
  });

  it("shows only the form (no divider) without SSO providers", async () => {
    mockApi({ "GET /api/v1/auth/config": authConfig() });
    renderRoute(<LoginPage />, { path: "/login" });
    expect(await screen.findByLabelText("Email")).toBeInTheDocument();
    expect(screen.queryByText("or sign in with email")).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("signs in with the login CSRF token and follows the server's redirect", async () => {
    const api = mockApi({
      "GET /api/v1/auth/config": authConfig(),
      "POST /api/v1/session": { data: { status: "active", redirect_to: "/mcp/oauth/authorize?client_id=moc_1" } },
    });
    renderRoute(<LoginPage />, { path: "/login" });

    await userEvent.type(await screen.findByLabelText("Email"), "ops@example.test");
    await userEvent.type(screen.getByLabelText("Password"), "secret-password");
    await userEvent.click(screen.getByRole("button", { name: "Sign in" }));

    await waitFor(() => expect(assign).toHaveBeenCalledWith("/mcp/oauth/authorize?client_id=moc_1"));
    const call = api.calls("POST /api/v1/session")[0];
    expect(call?.body).toEqual({ data: { email: "ops@example.test", password: "secret-password" } });
    expect(call?.headers["X-CSRF-Token"]).toBe("login-csrf");
  });

  it("shows a rejected sign-in inside the card and keeps the form", async () => {
    mockApi({
      "GET /api/v1/auth/config": authConfig(),
      "POST /api/v1/session": apiResponse(422, {
        error: { message: "Invalid email or password.", details: { reason: "invalid_credentials" } },
      }),
    });
    renderRoute(<LoginPage />, { path: "/login" });

    await userEvent.type(await screen.findByLabelText("Email"), "ops@example.test");
    await userEvent.type(screen.getByLabelText("Password"), "wrong");
    await userEvent.click(screen.getByRole("button", { name: "Sign in" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Invalid email or password.");
    expect(assign).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Sign in" })).toBeEnabled();
  });

  it("refreshes a stale CSRF token once and retries", async () => {
    let posts = 0;
    let configs = 0;
    const api = mockApi({
      "GET /api/v1/auth/config": () => authConfig({ csrf_token: `token-${++configs}` }),
      "POST /api/v1/session": () =>
        ++posts <= 2
          ? apiResponse(403, { error: { message: "invalid or missing CSRF token", details: { reason: "csrf" } } })
          : { data: { status: "pending", redirect_to: "/pending" } },
    });
    renderRoute(<LoginPage />, { path: "/login" });

    await userEvent.type(await screen.findByLabelText("Email"), "new@example.test");
    await userEvent.type(screen.getByLabelText("Password"), "pw");
    await userEvent.click(screen.getByRole("button", { name: "Sign in" }));

    await waitFor(() => expect(assign).toHaveBeenCalledWith("/pending"));
    expect(api.calls("POST /api/v1/session").at(-1)?.headers["X-CSRF-Token"]).toBe("token-2");
  });

  it("sends an already signed-in browser on without showing the form", async () => {
    mockApi({ "GET /api/v1/auth/config": authConfig({ signed_in: true, redirect_to: "/console/principals" }) });
    renderRoute(<LoginPage />, { path: "/login" });

    await waitFor(() => expect(assign).toHaveBeenCalledWith("/console/principals"));
    expect(screen.queryByLabelText("Email")).not.toBeInTheDocument();
  });

  it("never follows an off-origin redirect", async () => {
    mockApi({ "GET /api/v1/auth/config": authConfig({ signed_in: true, redirect_to: "//evil.example/" }) });
    renderRoute(<LoginPage />, { path: "/login" });
    await waitFor(() => expect(assign).toHaveBeenCalledWith("/"));
  });
});

describe("pending page", () => {
  const pendingMe = operatorMe({
    user: { id: "usr_p", email: "waiting@example.test", name: null, status: "pending", admin: false },
    csrf_token: "pending-csrf",
  });

  it("shows the holding message and signs out with the session's CSRF token", async () => {
    const api = mockApi({ "GET /api/v1/me": { data: pendingMe }, "DELETE /api/v1/session": undefined });
    renderRoute(<PendingPage />, { path: "/pending" });

    expect(await screen.findByRole("heading", { name: "Awaiting approval" })).toBeInTheDocument();
    expect(screen.getByText("waiting@example.test")).toBeInTheDocument();
    expect(screen.getByText(/pending approval by an administrator/)).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Sign out" }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith("/login"));
    expect(api.calls("DELETE /api/v1/session")[0]?.headers["X-CSRF-Token"]).toBe("pending-csrf");
  });

  it("sends signed-out browsers to the login page", async () => {
    mockApi({ "GET /api/v1/me": apiResponse(401, { error: { message: "invalid or missing API key" } }) });
    renderRoute(<PendingPage />, { path: "/pending" });
    await waitFor(() => expect(assign).toHaveBeenCalledWith("/login"));
  });

  it("sends active accounts to their landing page", async () => {
    mockApi({ "GET /api/v1/me": { data: adminMe() } });
    renderRoute(<PendingPage />, { path: "/pending" });
    await waitFor(() => expect(assign).toHaveBeenCalledWith("/console/principals"));

    assign.mockClear();
    mockApi({ "GET /api/v1/me": { data: operatorMe() } });
    renderRoute(<PendingPage />, { path: "/pending" });
    await waitFor(() => expect(assign).toHaveBeenCalledWith("/console/integrations"));
  });
});

describe("OAuth consent result page", () => {
  it("shows a denied consent with a retry link", async () => {
    mockApi({
      "GET /api/v1/auth/config": authConfig({
        oauth_result: { kind: "denied", message: "Consent was not granted (access_denied).", retry_path: "/oauth/google/start" },
      }),
    });
    renderRoute(<OauthResultPage />, { path: "/oauth/result" });

    expect(await screen.findByRole("heading", { name: "Not connected" })).toBeInTheDocument();
    expect(screen.getByText("Consent was not granted (access_denied).")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Try again" })).toHaveAttribute("href", "/oauth/google/start");
    expect(document.title).toBe("Not connected · Centaur Console");
  });

  it("shows an error without a retry link for unknown or disabled apps", async () => {
    mockApi({
      "GET /api/v1/auth/config": authConfig({ oauth_result: { kind: "error", message: "This integration is disabled." } }),
    });
    renderRoute(<OauthResultPage />, { path: "/oauth/result" });

    expect(await screen.findByRole("heading", { name: "Something went wrong" })).toBeInTheDocument();
    expect(screen.getByText("This integration is disabled.")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Try again" })).not.toBeInTheDocument();
  });

  it("ignores a retry path that is not an OAuth start", async () => {
    mockApi({
      "GET /api/v1/auth/config": authConfig({
        oauth_result: { kind: "error", message: "Failed.", retry_path: "https://evil.example/oauth/x/start" },
      }),
    });
    renderRoute(<OauthResultPage />, { path: "/oauth/result" });
    expect(await screen.findByText("Failed.")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Try again" })).not.toBeInTheDocument();
  });

  it("explains a result that was already shown (reload)", async () => {
    mockApi({ "GET /api/v1/auth/config": authConfig() });
    renderRoute(<OauthResultPage />, { path: "/oauth/result", url: "/oauth/result?message=Injected" });

    expect(await screen.findByText(/no longer available/)).toBeInTheDocument();
    expect(screen.queryByText("Injected")).not.toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Go to Integrations" })).toHaveAttribute("href", "/console/integrations");
  });
});

describe("MCP consent page", () => {
  const details = {
    data: {
      client: { id: "moc_1", name: "Amp" },
      redirect_host: "127.0.0.1",
      resource: "https://mcp.example.test/mcp",
      scopes: ["mcp:tools"],
      user: { email: "ops@example.test" },
    },
  };

  it("shows the client, host, resource, scope, and account, then approves", async () => {
    const api = mockApi({
      "GET /api/v1/mcp/authorization": details,
      "POST /api/v1/mcp/authorization/decision": {
        data: { decision: "approve", redirect_to: "http://127.0.0.1:49152/callback?code=abc&state=s" },
      },
    });
    renderRoute(<McpConsentPage />, {
      path: "/mcp/oauth/consent",
      url: "/mcp/oauth/consent?request=ticket-1&redirect_uri=https%3A%2F%2Fevil.example",
    });

    expect(await screen.findByText("Amp")).toBeInTheDocument();
    expect(screen.getByText("127.0.0.1")).toBeInTheDocument();
    expect(screen.getByText("https://mcp.example.test/mcp")).toBeInTheDocument();
    expect(screen.getByText("mcp:tools")).toBeInTheDocument();
    expect(screen.getByText("ops@example.test")).toBeInTheDocument();
    expect(screen.getByText("Authorize MCP access.")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/mcp/authorization")[0]?.url.searchParams.get("request")).toBe("ticket-1");

    await userEvent.click(screen.getByRole("button", { name: "Allow" }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith("http://127.0.0.1:49152/callback?code=abc&state=s"));
    const call = api.calls("POST /api/v1/mcp/authorization/decision")[0];
    expect(call?.body).toEqual({ data: { request: "ticket-1", decision: "approve" } });
    expect(call?.headers["X-CSRF-Token"]).toBe("csrf-test-token");
  });

  it("denies", async () => {
    const api = mockApi({
      "GET /api/v1/mcp/authorization": details,
      "POST /api/v1/mcp/authorization/decision": {
        data: { decision: "deny", redirect_to: "http://127.0.0.1:49152/callback?error=access_denied" },
      },
    });
    renderRoute(<McpConsentPage />, { path: "/mcp/oauth/consent", url: "/mcp/oauth/consent?request=ticket-1" });

    await userEvent.click(await screen.findByRole("button", { name: "Deny" }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith("http://127.0.0.1:49152/callback?error=access_denied"));
    expect(api.calls("POST /api/v1/mcp/authorization/decision")[0]?.body).toEqual({
      data: { request: "ticket-1", decision: "deny" },
    });
  });

  it("refuses an invalid or expired ticket without offering a decision", async () => {
    mockApi({
      "GET /api/v1/mcp/authorization": apiResponse(404, {
        error: {
          message: "This authorization request is invalid or has expired. Start again from your MCP client.",
          details: { reason: "invalid_consent_request" },
        },
      }),
    });
    renderRoute(<McpConsentPage />, { path: "/mcp/oauth/consent", url: "/mcp/oauth/consent?request=stale" });

    expect(await screen.findByRole("alert")).toHaveTextContent("invalid or has expired");
    expect(screen.queryByRole("button", { name: "Allow" })).not.toBeInTheDocument();
  });

  it("needs a ticket", async () => {
    const api = mockApi({});
    renderRoute(<McpConsentPage />, { path: "/mcp/oauth/consent" });
    expect(await screen.findByRole("alert")).toHaveTextContent("invalid or has expired");
    expect(api.calls()).toHaveLength(0);
  });

  it("offers the client's vouched-for error redirect when the request is no longer valid", async () => {
    mockApi({
      "GET /api/v1/mcp/authorization": apiResponse(422, {
        error: {
          message: "Unsupported scope: admin.",
          details: {
            reason: "invalid_authorization_request",
            redirect_to: "http://127.0.0.1:49152/callback?error=invalid_scope",
          },
        },
      }),
    });
    renderRoute(<McpConsentPage />, { path: "/mcp/oauth/consent", url: "/mcp/oauth/consent?request=t" });

    expect(await screen.findByRole("alert")).toHaveTextContent("Unsupported scope: admin.");
    await userEvent.click(screen.getByRole("button", { name: "Return to the application" }));
    expect(assign).toHaveBeenCalledWith("http://127.0.0.1:49152/callback?error=invalid_scope");
  });

  it("shows a failed decision", async () => {
    mockApi({
      "GET /api/v1/mcp/authorization": details,
      "POST /api/v1/mcp/authorization/decision": apiResponse(404, {
        error: { message: "This authorization request is invalid or has expired.", details: { reason: "invalid_consent_request" } },
      }),
    });
    renderRoute(<McpConsentPage />, { path: "/mcp/oauth/consent", url: "/mcp/oauth/consent?request=t" });

    await userEvent.click(await screen.findByRole("button", { name: "Allow" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("invalid or has expired");
    expect(assign).not.toHaveBeenCalled();
  });
});

describe("navigation guards", () => {
  it("keeps only same-origin paths", () => {
    expect(safeLocalPath("/console/x?y=1")).toBe("/console/x?y=1");
    for (const bad of ["//evil.example", "/\\evil.example", "https://evil.example", "javascript:alert(1)", "/\tx", "", null]) {
      expect(safeLocalPath(bad)).toBe("/");
    }
  });

  it("recognises OAuth start paths", () => {
    expect(isOauthStartPath("/oauth/google/start")).toBe(true);
    expect(isOauthStartPath("/oauth/google/callback")).toBe(false);
    expect(isOauthStartPath("//evil/oauth/x/start")).toBe(false);
  });
});
