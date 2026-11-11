import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { navigation } from "@/lib/api";
import { adminMe, apiResponse, mockApi, operatorMe, renderRoute } from "@/test/utils";

import IntegrationsPage from "./pages/Integrations";
import OauthAppDetailPage from "./pages/OauthAppDetail";
import OauthAppEditPage from "./pages/OauthAppEdit";
import OauthAppNewPage from "./pages/OauthAppNew";
import OauthAppsListPage from "./pages/OauthAppsList";
import { lineList, toPayload, initialValues } from "./sections/OauthAppForm";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const meta = (total: number) => ({ page: 1, limit: 200, total, total_pages: total === 0 ? 0 : 1 });

const app = {
  id: "oap_google",
  slug: "google",
  description: "Gmail and Calendar",
  labels: { team: "comms" },
  provider: "google",
  provider_config: {},
  client_id: "acme-google-client-id",
  allowed_scopes: ["https://www.googleapis.com/auth/gmail.readonly", "openid"],
  enabled: true,
  always_available: false,
  shared: false,
  client_secret_set: true,
  redirect_uri: "https://console.example.test/oauth/google/callback",
  start_url: "https://console.example.test/oauth/google/start",
  credentials_count: 2,
  created_at: "2026-06-01T10:00:00Z",
  updated_at: "2026-06-01T10:00:00Z",
};

const providers = [
  { key: "google", display_name: "Google", presettable: false, config_keys: [], optional_config_keys: [] },
  { key: "github", display_name: "GitHub", presettable: false, config_keys: [], optional_config_keys: [] },
  {
    key: "custom",
    display_name: "Custom",
    presettable: false,
    config_keys: ["authorization_endpoint", "token_endpoint", "userinfo_endpoint", "resource"],
    optional_config_keys: ["userinfo_endpoint"],
  },
];

describe("apps list", () => {
  it("lists apps with provider, client id, enabled state, and credential counts", async () => {
    mockApi({
      "GET /api/v1/oauth_apps": {
        data: [app, { ...app, id: "oap_slack", slug: "slack", provider: "slack", description: null, enabled: false, credentials_count: 0 }],
        meta: meta(2),
      },
    });
    renderRoute(<OauthAppsListPage />, { path: "/console/oauth_apps" });

    expect(await screen.findByRole("link", { name: "google" })).toHaveAttribute("href", "/console/oauth_apps/oap_google");
    expect(screen.getByText("2 OAuth apps driving the public consent flow.")).toBeInTheDocument();
    expect(screen.getByText("oap_google")).toBeInTheDocument();
    expect(screen.getByText("Gmail and Calendar")).toBeInTheDocument();
    expect(screen.getAllByText("acme-google-client-id")).toHaveLength(2);
    expect(screen.getByText("enabled")).toBeInTheDocument();
    expect(screen.getByText("disabled")).toBeInTheDocument();
    const slackRow = screen.getByRole("link", { name: "slack" }).closest("tr") as HTMLElement;
    expect(within(slackRow).getByText("—")).toBeInTheDocument();
    expect(within(slackRow).getByText("0")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "+ Add App" })).toHaveAttribute("href", "/console/oauth_apps/new");
  });

  it("shows the empty state", async () => {
    mockApi({ "GET /api/v1/oauth_apps": { data: [], meta: meta(0) } });
    renderRoute(<OauthAppsListPage />, { path: "/console/oauth_apps" });
    expect(await screen.findByText("No apps.")).toBeInTheDocument();
    expect(screen.getByText("0 OAuth apps driving the public consent flow.")).toBeInTheDocument();
  });

  it("sends operators to their landing page", async () => {
    const api = mockApi({});
    renderRoute(<OauthAppsListPage />, { path: "/console/oauth_apps", me: operatorMe() });
    expect(await screen.findByText("other page")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/oauth_apps")).toHaveLength(0);
  });
});

describe("app detail", () => {
  it("shows config, the redirect URI, the start URL, and minted credentials but never the secret", async () => {
    mockApi({
      "GET /api/v1/oauth_apps/oap_google": { data: app },
      "GET /api/v1/oauth_apps/oap_google/credentials": {
        data: [
          {
            id: "bcr_1",
            foreign_id: "minted-1",
            name: null,
            status: "live",
            provider_email: "person@example.com",
            external_user_key: "user-1",
            created_at: "",
          },
          { id: "bcr_2", foreign_id: null, name: null, status: "dead", provider_email: null, external_user_key: null, created_at: "" },
        ],
        meta: meta(2),
      },
    });
    renderRoute(<OauthAppDetailPage />, { path: "/console/oauth_apps/:id", url: "/console/oauth_apps/oap_google" });

    expect(await screen.findByRole("heading", { name: "google", level: 1 })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Edit" })).toHaveAttribute("href", "/console/oauth_apps/oap_google/edit");
    expect(screen.getByText("team=comms")).toBeInTheDocument();
    const client = screen.getByRole("region", { name: "OAuth Client" });
    expect(within(client).getByText("acme-google-client-id")).toBeInTheDocument();
    expect(within(client).getByText("set")).toBeInTheDocument();
    expect(screen.getByText("https://console.example.test/oauth/google/callback")).toBeInTheDocument();
    expect(screen.getByText("https://console.example.test/oauth/google/start")).toBeInTheDocument();
    expect(screen.getByText(/Share the start URL with a team member/)).toBeInTheDocument();

    expect(await screen.findByRole("link", { name: "minted-1" })).toHaveAttribute("href", "/console/credentials/bcr_1");
    expect(screen.getByRole("link", { name: "bcr_2" })).toBeInTheDocument();
    expect(screen.getByText("person@example.com")).toBeInTheDocument();
    expect(screen.getByText("The client secret is never shown. Credentials minted by this app inherit its client id and secret.")).toBeInTheDocument();
    // A personal app has no provider panel to fetch.
    expect(screen.queryByRole("region", { name: "Mercator wallet" })).not.toBeInTheDocument();
  });

  it("shows custom provider config, an unset secret, and the empty credentials state", async () => {
    const api = mockApi({
      "GET /api/v1/oauth_apps/oap_custom": {
        data: {
          ...app,
          id: "oap_custom",
          slug: "custom-app",
          provider: "custom",
          shared: true,
          client_secret_set: false,
          provider_config: { token_endpoint: "https://auth.example.test/token" },
        },
      },
      "GET /api/v1/oauth_apps/oap_custom/credentials": { data: [], meta: meta(0) },
    });
    renderRoute(<OauthAppDetailPage />, { path: "/console/oauth_apps/:id", url: "/console/oauth_apps/oap_custom" });

    expect(await screen.findByText("not set")).toBeInTheDocument();
    expect(screen.getByText("Token endpoint")).toBeInTheDocument();
    expect(screen.getByText("https://auth.example.test/token")).toBeInTheDocument();
    expect(screen.getByText(/An admin must connect this shared account/)).toBeInTheDocument();
    expect(await screen.findByText("No credentials minted yet.")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/oauth_apps/oap_custom/provider_details")).toHaveLength(0);
  });

  const mercator = {
    ...app,
    id: "oap_mercator",
    slug: "mercator",
    provider: "mercator",
    shared: true,
  };
  const wallet = "0xabababababababababababababababababababab";

  function mercatorApi(details: Record<string, unknown>, staticSecretId: string | null = "ssr_wallet") {
    return mockApi({
      "GET /api/v1/oauth_apps/oap_mercator": { data: mercator },
      "GET /api/v1/oauth_apps/oap_mercator/credentials": { data: [], meta: meta(0) },
      "GET /api/v1/oauth_apps/oap_mercator/provider_details": {
        data: { provider: "mercator", credential: { id: "bcr_w", provider_subject: wallet, static_secret_id: staticSecretId }, details },
      },
    });
  }

  it("shows the Mercator wallet panel with a claim link until MACH is claimed", async () => {
    mercatorApi({ claimed: false });
    renderRoute(<OauthAppDetailPage />, { path: "/console/oauth_apps/:id", url: "/console/oauth_apps/oap_mercator" });

    const panel = await screen.findByRole("region", { name: "Mercator wallet" });
    expect(within(panel).getByRole("link", { name: "0xabab…abab ↗" })).toHaveAttribute(
      "href",
      `https://explore.tempo.xyz/address/${wallet}`,
    );
    expect(within(panel).getByRole("link", { name: "Manage wallet ↗" })).toHaveAttribute("href", "https://mercator.sh/account");
    expect(within(panel).getByRole("link", { name: "Claim MACH" })).toHaveAttribute(
      "href",
      `https://mercator.sh/slack-claim?wallet=${wallet}`,
    );
    expect(within(panel).getByRole("link", { name: "Agent access" })).toHaveAttribute(
      "href",
      "/console/secrets/static/ssr_wallet#roles",
    );
  });

  it("disables the claim once MACH is claimed and keeps the link when details fail", async () => {
    mercatorApi({ claimed: true }, null);
    const { unmount } = renderRoute(<OauthAppDetailPage />, {
      path: "/console/oauth_apps/:id",
      url: "/console/oauth_apps/oap_mercator",
    });
    expect(await screen.findByRole("button", { name: "MACH already claimed" })).toBeDisabled();
    expect(screen.queryByRole("link", { name: "Claim MACH" })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Agent access" })).not.toBeInTheDocument();
    unmount();

    mercatorApi({});
    renderRoute(<OauthAppDetailPage />, { path: "/console/oauth_apps/:id", url: "/console/oauth_apps/oap_mercator" });
    expect(await screen.findByRole("link", { name: "Claim MACH" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Manage wallet ↗" })).toBeInTheDocument();
  });
});

describe("app form", () => {
  it("creates an app with all fields and goes to its page", async () => {
    const user = userEvent.setup();
    const api = mockApi({
      "GET /api/v1/oauth_providers": { data: providers },
      "POST /api/v1/oauth_apps": { data: { ...app, id: "oap_new" } },
    });
    renderRoute(<OauthAppNewPage />, {
      path: "/console/oauth_apps/new",
      routes: [{ path: "/console/oauth_apps/:id", element: <p>detail page</p> }],
    });

    await user.type(await screen.findByLabelText("Slug *"), "new-google");
    await user.type(screen.getByLabelText("Description"), "New Google integration");
    await user.type(screen.getByLabelText("Client ID *"), "cid");
    await user.type(screen.getByLabelText("Client secret *"), "shh");
    await user.type(screen.getByLabelText("Allowed scopes *"), "scope.a{Enter}{Enter}scope.b ");
    await user.click(screen.getByLabelText("Always available to the connecting user"));
    await user.click(screen.getByRole("button", { name: "Add label" }));
    await user.type(screen.getByLabelText("Labels 1 key"), "team");
    await user.type(screen.getByLabelText("Labels 1 value"), "comms");
    expect(screen.getByLabelText("Enabled")).toBeChecked();
    await user.click(screen.getByRole("button", { name: "Create app" }));

    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(api.calls("POST /api/v1/oauth_apps")[0]?.body).toEqual({
      data: {
        slug: "new-google",
        description: "New Google integration",
        provider: "google",
        client_id: "cid",
        client_secret: "shh",
        provider_config: {},
        allowed_scopes: ["scope.a", "scope.b"],
        enabled: true,
        shared: false,
        always_available: true,
        labels: { team: "comms" },
      },
    });
  });

  it("shows server validation errors with the legacy summary", async () => {
    const user = userEvent.setup();
    mockApi({
      "GET /api/v1/oauth_providers": { data: providers },
      "POST /api/v1/oauth_apps": apiResponse(422, {
        error: {
          message: "Validation failed",
          details: { allowed_scopes: ["can't be blank"], slug: ["can't be blank"] },
        },
      }),
    });
    renderRoute(<OauthAppNewPage />, { path: "/console/oauth_apps/new" });
    await user.click(await screen.findByRole("button", { name: "Create app" }));

    expect(await screen.findByText("2 errors prevented this app from saving:")).toBeInTheDocument();
    expect(screen.getByText("Allowed scopes can't be blank")).toBeInTheDocument();
    expect(screen.getAllByText("can't be blank").length).toBeGreaterThanOrEqual(2);
  });

  it("edits an app without resending the secret or shared mode", async () => {
    const user = userEvent.setup();
    const api = mockApi({
      "GET /api/v1/oauth_providers": { data: providers },
      "GET /api/v1/oauth_apps/oap_google": { data: { ...app, shared: true } },
      "PATCH /api/v1/oauth_apps/oap_google": { data: app },
    });
    renderRoute(<OauthAppEditPage />, {
      path: "/console/oauth_apps/:id/edit",
      url: "/console/oauth_apps/oap_google/edit",
      routes: [{ path: "/console/oauth_apps/:id", element: <p>detail page</p> }],
    });

    expect(await screen.findByRole("heading", { name: "Edit google" })).toBeInTheDocument();
    const secret = await screen.findByLabelText("Client secret");
    expect(secret).toHaveValue("");
    expect(secret).toHaveAttribute("placeholder", "•••••••• (unchanged)");
    expect(screen.getByLabelText(/^Shared:/)).toBeDisabled();
    expect(screen.getByLabelText(/^Shared:/)).toBeChecked();
    expect(screen.getByLabelText("Allowed scopes *")).toHaveValue("https://www.googleapis.com/auth/gmail.readonly\nopenid");

    await user.click(screen.getByLabelText("Enabled"));
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByText("detail page")).toBeInTheDocument();

    const body = api.calls("PATCH /api/v1/oauth_apps/oap_google")[0]?.body as { data: Record<string, unknown> };
    expect(body.data).not.toHaveProperty("client_secret");
    expect(body.data).not.toHaveProperty("shared");
    expect(body.data.enabled).toBe(false);
    expect(body.data.labels).toEqual({ team: "comms" });
  });

  it("only sends a trimmed provider config for the custom provider", () => {
    const keys = providers[2]?.config_keys ?? [];
    const values = {
      ...initialValues(undefined, "custom"),
      provider_config: { authorization_endpoint: " https://a.test/authorize ", token_endpoint: "", resource: "https://r.test" },
      allowed_scopes: "read\r\n\r\nwrite",
    };
    expect(toPayload(values, true, keys).provider_config).toEqual({
      authorization_endpoint: "https://a.test/authorize",
      resource: "https://r.test",
    });
    expect(toPayload({ ...values, provider: "google" }, true, keys).provider_config).toEqual({});
    expect(toPayload(values, true, keys).allowed_scopes).toEqual(["read", "write"]);
    expect(toPayload(values, false, keys)).not.toHaveProperty("shared");
    expect(lineList("  a \n\nb")).toEqual(["a", "b"]);
  });

  it("lists providers capitalized like the legacy select, defaulting to the first", async () => {
    const user = userEvent.setup();
    mockApi({ "GET /api/v1/oauth_providers": { data: providers } });
    renderRoute(<OauthAppNewPage />, { path: "/console/oauth_apps/new" });
    const select = await screen.findByRole("combobox", { name: "Provider *" });
    expect(select).toHaveTextContent("Google");
    await user.click(select);
    expect(await screen.findByRole("option", { name: "Github" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Custom" })).toBeInTheDocument();
  });
});

describe("integrations", () => {
  const integrations = {
    apps: [
      {
        id: "oap_github",
        slug: "github",
        provider: "github",
        description: "Repos",
        shared: false,
        always_available: true,
        usage: "always_available",
        start_url: "https://console.example.test/oauth/github/start",
        connection: { status: "needs_reconnect" },
      },
      {
        id: "oap_google",
        slug: "google",
        provider: "google",
        description: "Gmail",
        shared: false,
        always_available: false,
        usage: "direct_messages",
        start_url: "https://console.example.test/oauth/google/start",
        connection: null,
      },
      {
        id: "oap_shared",
        slug: "shared-notion",
        provider: "notion",
        description: "Hidden for shared apps",
        shared: true,
        always_available: false,
        usage: "shared",
        start_url: "https://console.example.test/oauth/shared-notion/start",
        connection: { status: "connected" },
      },
    ],
    presets: [{ provider: "mercator", display_name: "Mercator", description: "Shared with your agents." }],
    ssh_key: { configured: false },
  };

  it("shows each app's start link, usage, and connection state", async () => {
    mockApi({ "GET /api/v1/integrations": { data: integrations } });
    renderRoute(<IntegrationsPage />, { path: "/console/integrations", me: adminMe() });

    expect(await screen.findByRole("heading", { name: "Integrations" })).toBeInTheDocument();
    expect(screen.getByText("Connect your accounts so Centaur can act on your behalf.")).toBeInTheDocument();

    const ssh = await screen.findByRole("region", { name: "SSH key" });
    expect(within(ssh).getByRole("link", { name: "Connect" })).toHaveAttribute("href", "/console/ssh_key/edit");
    expect(within(ssh).queryByText("Configured")).not.toBeInTheDocument();

    const github = screen.getByRole("region", { name: "github" });
    expect(within(github).getByRole("link", { name: "Reconnect" })).toHaveAttribute(
      "href",
      "https://console.example.test/oauth/github/start",
    );
    expect(within(github).getByText("Needs reconnecting")).toBeInTheDocument();
    expect(within(github).getByText("Used automatically whenever you ask the bot.")).toBeInTheDocument();
    expect(within(github).queryByRole("link", { name: "Manage" })).not.toBeInTheDocument();
    expect(github.querySelector("[data-provider-logo=github]")).not.toBeNull();

    const google = screen.getByRole("region", { name: "google" });
    expect(within(google).getByRole("link", { name: "Connect" })).toHaveAttribute(
      "href",
      "https://console.example.test/oauth/google/start",
    );
    expect(within(google).getByText("Used only in your DMs with the bot.")).toBeInTheDocument();
    expect(within(google).getByText("Gmail")).toBeInTheDocument();

    const shared = screen.getByRole("region", { name: "shared-notion" });
    expect(within(shared).getByRole("link", { name: "Manage" })).toHaveAttribute("href", "/console/oauth_apps/oap_shared");
    expect(within(shared).getByText("Connected")).toBeInTheDocument();
    expect(within(shared).getByText("Shared with your agents.")).toBeInTheDocument();
    expect(within(shared).queryByText("Hidden for shared apps")).not.toBeInTheDocument();
    expect(within(shared).getByText("no")).toBeInTheDocument(); // two-letter fallback for unknown providers
  });

  it("links a configured SSH key to its page", async () => {
    mockApi({ "GET /api/v1/integrations": { data: { apps: [], presets: [], ssh_key: { configured: true } } } });
    renderRoute(<IntegrationsPage />, { path: "/console/integrations", me: operatorMe({ nav: { ...operatorMe().nav, integrations: true } }) });
    const ssh = await screen.findByRole("region", { name: "SSH key" });
    expect(within(ssh).getByRole("link", { name: "Manage" })).toHaveAttribute("href", "/console/ssh_key");
    expect(within(ssh).getByText("Configured")).toBeInTheDocument();
  });

  it("sets up a preset and sends the admin to its consent screen", async () => {
    const user = userEvent.setup();
    const assign = vi.spyOn(navigation, "assign").mockImplementation(() => {});
    const api = mockApi({
      "GET /api/v1/integrations": { data: integrations },
      "POST /api/v1/oauth_apps/presets/mercator": {
        data: { id: "oap_m", slug: "mercator", start_url: "https://console.example.test/oauth/mercator/start" },
      },
    });
    renderRoute(<IntegrationsPage />, { path: "/console/integrations" });

    const preset = await screen.findByRole("region", { name: "Mercator" });
    expect(within(preset).getByText("Shared with your agents.")).toBeInTheDocument();
    await user.click(within(preset).getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith("https://console.example.test/oauth/mercator/start"));
    expect(api.calls("POST /api/v1/oauth_apps/presets/mercator")).toHaveLength(1);
  });

  it("reports a preset conflict with the legacy alert", async () => {
    const user = userEvent.setup();
    const assign = vi.spyOn(navigation, "assign").mockImplementation(() => {});
    mockApi({
      "GET /api/v1/integrations": { data: integrations },
      "POST /api/v1/oauth_apps/presets/mercator": apiResponse(409, {
        error: { message: "The preset conflicts with an existing OAuth app. Review its configuration." },
      }),
    });
    const toast = await import("sonner");
    const error = vi.spyOn(toast.toast, "error");
    renderRoute(<IntegrationsPage />, { path: "/console/integrations" });
    await user.click(await within(await screen.findByRole("region", { name: "Mercator" })).findByRole("button", { name: "Connect" }));
    await waitFor(() =>
      expect(error).toHaveBeenCalledWith("The preset conflicts with an existing OAuth app. Review its configuration."),
    );
    expect(assign).not.toHaveBeenCalled();
  });
});
