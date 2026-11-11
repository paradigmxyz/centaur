import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Component, type ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, describe, expect, it, vi } from "vitest";

import { errorMessage } from "@/lib/api";
import { adminMe, apiResponse, mockApi, operatorMe, renderRoute } from "@/test/utils";

import SecretDetailPage from "./pages/SecretDetail";
import SecretEditPage from "./pages/SecretEdit";
import SecretNewPage from "./pages/SecretNew";
import SecretsListPage from "./pages/SecretsList";

vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

const meta = (total: number, page = 1) => ({ page, limit: 50, total, total_pages: Math.ceil(total / 50) });

const listItem = {
  kind: "static",
  kind_label: "Static",
  id: "ssr_gh",
  foreign_id: "github-token",
  name: "GitHub token",
  grant_key: "static_secret_id",
  description: null,
  labels: {},
  source_types: ["Env"],
  managed_by: null,
  created_at: "2026-06-01T10:00:00Z",
  updated_at: "2026-06-01T10:00:00Z",
};

const managedItem = {
  ...listItem,
  id: "ssr_managed",
  name: "Managed Gmail token",
  source_types: ["Token-Broker"],
  managed_by: { broker_credential_id: "bcr_gmail", oauth_app_id: "oap_google", oauth_app_slug: "google" },
};

const detail = {
  ...listItem,
  description: "Token for the CI bot",
  labels: { team: "platform" },
  editable: true,
  injection: "Authorization",
  source_segments: [{ role: null, type: "env", type_label: "Env", ref: "GITHUB_TOKEN" }],
  configuration: [
    { label: "Injects into", value: "Authorization" },
    { label: "Credential profile", value: "custom" },
    { label: "Mode", value: "inject" },
  ],
  signed_headers: null,
  rules: [{ host: "api.github.com", cidr: null, position: 0, http_methods: [], paths: ["/repos/*"] }],
};

const roleGrants = {
  data: [
    {
      id: "grant_1",
      role_id: "role_infra",
      role: { id: "role_infra", foreign_id: "infra", name: "Infra" },
      priority: 0,
      created_at: "",
    },
  ],
  assignable_roles: [{ id: "role_admin", foreign_id: "admin", name: "Admin" }],
};

function detailApi(overrides: Record<string, unknown> = {}) {
  return mockApi({
    "GET /api/v1/secrets/static/ssr_gh": { data: detail },
    "GET /api/v1/secrets/static/ssr_gh/role_grants": roleGrants,
    ...overrides,
  });
}

describe("secrets list", () => {
  it("lists every kind with type, source backends, managed badge, and total", async () => {
    const api = mockApi({
      "GET /api/v1/secrets": ({ url }: { url: URL }) =>
        url.searchParams.get("limit") === "1"
          ? { data: [], meta: meta(51) }
          : {
              data: [listItem, managedItem, { ...listItem, kind: "pg_dsn", kind_label: "Postgres DSN", id: "pgd_1", name: null, source_types: [] }],
              meta: meta(51),
            },
    });
    renderRoute(<SecretsListPage />, { path: "/console/secrets", url: "/console/secrets?q=tok&type=static" });

    expect(await screen.findByRole("link", { name: "GitHub token" })).toHaveAttribute("href", "/console/secrets/static/ssr_gh");
    expect(screen.getByRole("link", { name: "unnamed" })).toHaveAttribute("href", "/console/secrets/pg_dsn/pgd_1");
    expect(screen.getByText("managed")).toHaveAttribute("title", "Managed by the google OAuth integration");
    expect(screen.getByText("Env")).toBeInTheDocument();
    const table = screen.getByRole("table", { name: "Secrets" });
    expect(within(table).getByText("Postgres DSN")).toBeInTheDocument();
    expect(within(table).getByText("—")).toBeInTheDocument();
    expect(await screen.findByText("51 secrets total.")).toBeInTheDocument();
    expect(screen.getByText(/page 1 of 2/)).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Filter secrets by type" })).toHaveTextContent("Static");
    expect(screen.getByRole("button", { name: "Clear" })).toBeInTheDocument();

    const call = api.calls("GET /api/v1/secrets").find((c) => c.url.searchParams.get("limit") === "50");
    expect(call?.url.searchParams.get("q")).toBe("tok");
    expect(call?.url.searchParams.get("kind")).toBe("static");
  });

  it("offers the editable kinds in the Add Secret menu", async () => {
    mockApi({ "GET /api/v1/secrets": { data: [], meta: meta(0) } });
    renderRoute(<SecretsListPage />, { path: "/console/secrets" });
    expect(await screen.findByText("No secrets yet.")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /Add Secret/ }));
    const items = await screen.findAllByRole("menuitem");
    expect(items.map((item) => item.textContent)).toEqual(["Static", "GCP Auth", "GCP ID Token", "Postgres DSN"]);
    expect(screen.getByRole("menuitem", { name: "Postgres DSN" })).toHaveAttribute("href", "/console/secrets/pg_dsn/new");
  });

  it("applies filters to the URL and shows the filtered empty state", async () => {
    mockApi({ "GET /api/v1/secrets": { data: [], meta: meta(0) } });
    const { router } = renderRoute(<SecretsListPage />, { path: "/console/secrets", url: "/console/secrets?type=bogus" });
    // An unknown type is ignored.
    expect(await screen.findByText("No secrets yet.")).toBeInTheDocument();
    await userEvent.type(screen.getByRole("searchbox", { name: "Search secrets by name" }), "prod");
    await userEvent.click(screen.getByRole("button", { name: "Filter" }));
    expect(await screen.findByText("No secrets match these filters.")).toBeInTheDocument();
    expect(router.state.location.search).toBe("?q=prod");
  });

  it("sends non-admins to their landing page without fetching", async () => {
    const api = mockApi({});
    renderRoute(<SecretsListPage />, {
      path: "/console/secrets",
      me: operatorMe({ nav: { ...operatorMe().nav, integrations: true } }),
    });
    expect(await screen.findByText("other page")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/secrets")).toHaveLength(0);
  });
});

describe("secret detail", () => {
  it("renders identity, sources, configuration, rules, and role grants", async () => {
    detailApi();
    renderRoute(<SecretDetailPage />, { path: "/console/secrets/:kind/:id", url: "/console/secrets/static/ssr_gh" });

    expect(await screen.findByRole("heading", { name: "GitHub token" })).toBeInTheDocument();
    expect(screen.getByText("ssr_gh")).toBeInTheDocument();
    expect(screen.getByText("github-token")).toBeInTheDocument();
    expect(screen.getByText("Token for the CI bot")).toBeInTheDocument();
    expect(screen.getByText("team=platform")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Edit" })).toHaveAttribute("href", "/console/secrets/static/ssr_gh/edit");
    expect(screen.getByRole("button", { name: "Delete" })).toBeInTheDocument();

    const sources = screen.getByRole("table", { name: "Sources" });
    expect(within(sources).queryByText("Field")).not.toBeInTheDocument();
    expect(within(sources).getByText("GITHUB_TOKEN")).toBeInTheDocument();
    expect(screen.getByText("Credential profile").nextSibling).toHaveTextContent("custom");
    const rules = screen.getByRole("table", { name: "Request rules" });
    expect(within(rules).getByText("host: api.github.com")).toBeInTheDocument();
    expect(within(rules).getByText("any")).toBeInTheDocument();

    const roles = await screen.findByRole("table", { name: "Roles" });
    expect(within(roles).getByRole("link", { name: "role_infra" })).toHaveAttribute("href", "/console/roles/role_infra");
    expect(screen.getByRole("combobox", { name: "Role to assign" })).toHaveTextContent("Admin");
    expect(screen.queryByText(/Managed secret/)).not.toBeInTheDocument();
  });

  it("hides edit and delete for a read-only kind and shows field roles", async () => {
    mockApi({
      "GET /api/v1/secrets/oauth_token/ots_1": {
        data: {
          ...detail,
          kind: "oauth_token",
          kind_label: "OAuth Token",
          id: "ots_1",
          editable: false,
          source_segments: [
            { role: "client_id", type: "env", type_label: "Env", ref: "GMAIL_CLIENT_ID" },
            { role: "refresh_token", type: "1password", type_label: "1Password", ref: "op://eng/gmail/refresh-token" },
          ],
          rules: [],
        },
      },
      "GET /api/v1/secrets/oauth_token/ots_1/role_grants": { data: [], assignable_roles: [] },
    });
    renderRoute(<SecretDetailPage />, { path: "/console/secrets/:kind/:id", url: "/console/secrets/oauth_token/ots_1" });
    expect(await screen.findByText("OAuth Token")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delete" })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Edit" })).not.toBeInTheDocument();
    expect(within(screen.getByRole("table", { name: "Sources" })).getByText("Field")).toBeInTheDocument();
    expect(screen.getByText("op://eng/gmail/refresh-token")).toBeInTheDocument();
    expect(screen.getByText("No request rules.")).toBeInTheDocument();
    expect(await screen.findByText("No roles assigned.")).toBeInTheDocument();
  });

  it("links a managed secret to its integration and credential", async () => {
    mockApi({
      "GET /api/v1/secrets/static/ssr_managed": { data: { ...detail, ...managedItem, source_segments: [] } },
      "GET /api/v1/secrets/static/ssr_managed/role_grants": { data: [], assignable_roles: [] },
    });
    renderRoute(<SecretDetailPage />, { path: "/console/secrets/:kind/:id", url: "/console/secrets/static/ssr_managed" });
    expect(await screen.findByText("Managed secret.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "google OAuth integration" })).toHaveAttribute("href", "/console/oauth_apps/oap_google");
    expect(screen.getByRole("link", { name: "bcr_gmail" })).toHaveAttribute("href", "/console/credentials/bcr_gmail");
    expect(screen.getByText("No source configured.")).toBeInTheDocument();
  });

  it("assigns and unassigns roles", async () => {
    const api = detailApi({
      "POST /api/v1/secrets/static/ssr_gh/role_grants": { data: { id: "grant_2" } },
      "DELETE /api/v1/secrets/static/ssr_gh/role_grants/grant_1": undefined,
    });
    renderRoute(<SecretDetailPage />, { path: "/console/secrets/:kind/:id", url: "/console/secrets/static/ssr_gh" });

    await userEvent.click(await screen.findByRole("button", { name: "Assign role" }));
    await waitFor(() => expect(api.calls("POST /api/v1/secrets/static/ssr_gh/role_grants")).toHaveLength(1));
    expect(api.calls("POST /api/v1/secrets/static/ssr_gh/role_grants")[0]?.body).toEqual({ data: { role_id: "role_admin" } });
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Assigned secret to Admin."));

    await userEvent.click(screen.getByRole("button", { name: "Unassign" }));
    expect(await screen.findByText("Unassign this secret from Infra?")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Unassign" }));
    await waitFor(() => expect(api.calls("DELETE /api/v1/secrets/static/ssr_gh/role_grants/grant_1")).toHaveLength(1));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Unassigned secret from role."));
  });

  it("deletes the secret after confirmation and returns to the list", async () => {
    const api = detailApi({ "DELETE /api/v1/static_secrets/ssr_gh": undefined });
    const { router } = renderRoute(<SecretDetailPage />, {
      path: "/console/secrets/:kind/:id",
      url: "/console/secrets/static/ssr_gh",
      routes: [{ path: "/console/secrets", element: <p>secrets list</p> }],
    });
    await userEvent.click(await screen.findByRole("button", { name: "Delete" }));
    expect(screen.getByText("Delete this secret?")).toBeInTheDocument();
    expect(screen.getByText(/Any grants of it are removed too/)).toBeInTheDocument();
    await userEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Delete" }));
    expect(await screen.findByText("secrets list")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/secrets");
    expect(toast.success).toHaveBeenCalledWith("Secret deleted.");
    expect(api.calls("DELETE /api/v1/static_secrets/ssr_gh")[0]?.headers["X-CSRF-Token"]).toBe("csrf-test-token");
  });
});

describe("new secret", () => {
  it("creates a static secret with source, rules, and labels", async () => {
    const api = mockApi({ "POST /api/v1/static_secrets": { data: { id: "ssr_new" } } });
    const { router } = renderRoute(<SecretNewPage />, {
      path: "/console/secrets/:kind/new",
      url: "/console/secrets/static/new",
      routes: [{ path: "/console/secrets/:kind/:id", element: <p>detail page</p> }],
    });

    expect(screen.getByRole("heading", { name: "New secret" })).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Foreign ID"), "ui-static");
    await userEvent.type(screen.getByLabelText("Name"), "ui-static");
    await userEvent.type(screen.getByLabelText("Header"), "Authorization");
    await userEvent.type(screen.getByLabelText("Environment variable"), "UI_TOKEN");
    await userEvent.click(screen.getByRole("button", { name: "Add rule" }));
    await userEvent.type(screen.getByLabelText("Rule 1 host"), "api.example.com");
    await userEvent.click(within(screen.getByRole("group", { name: "Rule 1 HTTP methods" })).getByRole("button", { name: "GET" }));
    await userEvent.click(within(screen.getByRole("group", { name: "Rule 1 HTTP methods" })).getByRole("button", { name: "POST" }));
    await userEvent.type(screen.getByLabelText("Rule 1 paths"), "/v1/*,");
    await userEvent.click(screen.getByRole("button", { name: "Add rule" }));
    await userEvent.click(screen.getByRole("button", { name: "Add label" }));
    await userEvent.type(screen.getByLabelText("Labels 1 key"), "team");
    await userEvent.type(screen.getByLabelText("Labels 1 value"), "platform");
    await userEvent.click(screen.getByRole("button", { name: "Create secret" }));

    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/secrets/static/ssr_new");
    expect(toast.success).toHaveBeenCalledWith("Secret created.");
    expect(api.calls("POST /api/v1/static_secrets")[0]?.body).toEqual({
      data: {
        foreign_id: "ui-static",
        name: "ui-static",
        description: null,
        labels: { team: "platform" },
        kind: "custom",
        inject_config: { header: "Authorization" },
        replace_config: null,
        source: { source_type: "env", config: { var: "UI_TOKEN" } },
        // The second, blank rule row is dropped.
        rules: [{ host: "api.example.com", cidr: null, http_methods: ["GET", "POST"], paths: ["/v1/*"] }],
      },
    });
  });

  it("makes ALL and specific methods mutually exclusive", async () => {
    mockApi({});
    renderRoute(<SecretNewPage />, { path: "/console/secrets/:kind/new", url: "/console/secrets/gcp_id_token/new" });
    expect(screen.getByText(/GCP ID token secrets require at least one rule/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Add rule" }));
    const methods = screen.getByRole("group", { name: "Rule 1 HTTP methods" });
    await userEvent.click(within(methods).getByRole("button", { name: "GET" }));
    await userEvent.click(within(methods).getByRole("button", { name: "ALL" }));
    expect(within(methods).getByRole("button", { name: "GET" })).toHaveAttribute("aria-pressed", "false");
    expect(within(methods).getByRole("button", { name: "ALL" })).toHaveAttribute("aria-pressed", "true");
    await userEvent.click(within(methods).getByRole("button", { name: "PUT" }));
    expect(within(methods).getByRole("button", { name: "ALL" })).toHaveAttribute("aria-pressed", "false");
  });

  it("shows server errors in the summary with source and rule prefixes", async () => {
    mockApi({
      "POST /api/v1/static_secrets": apiResponse(422, {
        error: {
          message: "validation failed",
          details: { "rules[0].base": ["must set exactly one of host or cidr"], name: ["is too long"] },
        },
      }),
    });
    renderRoute(<SecretNewPage />, { path: "/console/secrets/:kind/new", url: "/console/secrets/static/new" });
    await userEvent.type(screen.getByLabelText("Environment variable"), "TOKEN");
    await userEvent.click(screen.getByRole("button", { name: "Add rule" }));
    await userEvent.type(screen.getByLabelText("Rule 1 host"), "h.example.com");
    await userEvent.type(screen.getByLabelText("Rule 1 CIDR"), "10.0.0.0/8");
    await userEvent.click(screen.getByRole("button", { name: "Create secret" }));

    expect(await screen.findByText("2 errors prevented this secret from saving:")).toBeInTheDocument();
    expect(screen.getByText("Rule 1: must set exactly one of host or cidr")).toBeInTheDocument();
    expect(screen.getByText("Name is too long")).toBeInTheDocument();
    expect(screen.getByText("is too long")).toBeInTheDocument();
  });

  it("creates a pg_dsn secret with an inline DSN and ordered settings", async () => {
    const api = mockApi({ "POST /api/v1/pg_dsn_secrets": { data: { id: "pgd_new" } } });
    renderRoute(<SecretNewPage />, {
      path: "/console/secrets/:kind/new",
      url: "/console/secrets/pg_dsn/new",
      routes: [{ path: "/console/secrets/:kind/:id", element: <p>detail page</p> }],
    });
    expect(screen.getByText("Foreign ID *")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Foreign ID *"), "ui-shop");
    await userEvent.type(screen.getByLabelText("Database *"), "shop");
    await userEvent.click(screen.getByRole("button", { name: "Add setting" }));
    await userEvent.type(screen.getByLabelText("Setting 1 name"), "app.tenant");
    await userEvent.type(screen.getByLabelText("Setting 1 value"), "centaur");
    await userEvent.click(screen.getByRole("button", { name: "Add setting" }));
    await userEvent.click(screen.getByRole("button", { name: "Add setting" }));
    await userEvent.type(screen.getByLabelText("Setting 3 name"), "centaur.principal");
    await userEvent.click(screen.getByRole("combobox", { name: "Setting 3 kind" }));
    await userEvent.click(await screen.findByRole("option", { name: "Principal field" }));
    await userEvent.type(screen.getByLabelText("Setting 3 value"), "foreign_id");
    await userEvent.click(screen.getByRole("combobox", { name: "Backend" }));
    await userEvent.click(await screen.findByRole("option", { name: "Inline Value" }));
    const inline = screen.getByLabelText("Inline value");
    expect(inline).toHaveAttribute("placeholder", "stored encrypted at rest");
    await userEvent.type(inline, "postgres://u:p@db.example/shop");
    await userEvent.click(screen.getByRole("button", { name: "Create secret" }));

    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(api.calls("POST /api/v1/pg_dsn_secrets")[0]?.body).toEqual({
      data: {
        foreign_id: "ui-shop",
        name: null,
        description: null,
        labels: {},
        database: "shop",
        role: null,
        settings: [
          { name: "app.tenant", value: "centaur" },
          { name: "centaur.principal", value_from: { principal_field: "foreign_id" } },
        ],
        dsn: { source_type: "control_plane", config: {}, secret: "postgres://u:p@db.example/shop" },
      },
    });
  });

  it("creates a workload-identity gcp_auth secret without a keyfile", async () => {
    const api = mockApi({ "POST /api/v1/gcp_auth_secrets": { data: { id: "gas_new" } } });
    renderRoute(<SecretNewPage />, {
      path: "/console/secrets/:kind/new",
      url: "/console/secrets/gcp_auth/new",
      routes: [{ path: "/console/secrets/:kind/:id", element: <p>detail page</p> }],
    });
    expect(screen.getByText("Keyfile Source")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("radio", { name: "Workload identity (metadata server)" }));
    expect(screen.queryByText("Keyfile Source")).not.toBeInTheDocument();
    expect(screen.getByText(/No keyfile is stored/)).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Foreign ID"), "ui-gcp-wi");
    await userEvent.type(screen.getByLabelText("Scopes"), "https://www.googleapis.com/auth/cloud-platform");
    await userEvent.click(screen.getByRole("button", { name: "Create secret" }));

    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(api.calls("POST /api/v1/gcp_auth_secrets")[0]?.body).toEqual({
      data: {
        foreign_id: "ui-gcp-wi",
        name: null,
        description: null,
        labels: {},
        scopes: ["https://www.googleapis.com/auth/cloud-platform"],
        rules: [],
        credentials_provider: { type: "workload_identity" },
        subject: null,
      },
    });
  });

  it("refuses a kind without a form", () => {
    mockApi({});
    renderRoute(<SecretNewPage />, { path: "/console/secrets/:kind/new", url: "/console/secrets/hmac/new" });
    expect(screen.getByText("Secret not found.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Create secret" })).not.toBeInTheDocument();
  });
});

describe("edit secret", () => {
  const record = {
    id: "ssr_gh",
    foreign_id: "github-token",
    name: "GitHub token",
    description: null,
    labels: { team: "platform" },
    kind: "custom",
    inject_config: { header: "Authorization", formatter: "Bearer {{ .Value }}" },
    replace_config: null,
    source: { source_type: "control_plane", config: {} },
    rules: [{ host: "api.github.com", cidr: null, position: 0, http_methods: ["GET"], paths: [] }],
    created_at: "",
    updated_at: "2026-06-01T10:00:00Z",
  };

  it("never prefills the inline value and keeps it when left blank", async () => {
    const api = detailApi({
      "GET /api/v1/static_secrets/ssr_gh": { data: record },
      "PATCH /api/v1/static_secrets/ssr_gh": { data: record },
    });
    renderRoute(<SecretEditPage />, {
      path: "/console/secrets/:kind/:id/edit",
      url: "/console/secrets/static/ssr_gh/edit",
      routes: [{ path: "/console/secrets/:kind/:id", element: <p>detail page</p> }],
    });

    expect(await screen.findByRole("heading", { name: "Edit GitHub token" })).toBeInTheDocument();
    const inline = screen.getByLabelText("Inline value");
    expect(inline).toHaveValue("");
    expect(inline).toHaveAttribute("placeholder", "•••••••• (unchanged)");
    expect(screen.getByLabelText("Header")).toHaveValue("Authorization");
    expect(screen.getByLabelText("Rule 1 host")).toHaveValue("api.github.com");
    expect(screen.queryByText(/Managed secret/)).not.toBeInTheDocument();

    await userEvent.clear(screen.getByLabelText("Name"));
    await userEvent.type(screen.getByLabelText("Name"), "renamed");
    await userEvent.click(screen.getByRole("button", { name: "Save changes" }));

    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(toast.success).toHaveBeenCalledWith("Secret updated.");
    const body = api.calls("PATCH /api/v1/static_secrets/ssr_gh")[0]?.body as { data: Record<string, unknown> };
    expect(body.data).toMatchObject({
      name: "renamed",
      inject_config: { header: "Authorization", formatter: "Bearer {{ .Value }}" },
      source: { source_type: "control_plane", config: {} },
      rules: [{ host: "api.github.com", cidr: null, http_methods: ["GET"], paths: [] }],
      labels: { team: "platform" },
    });
    expect(body.data.source).not.toHaveProperty("secret");
  });

  it("warns when the secret is managed by an OAuth integration", async () => {
    mockApi({
      "GET /api/v1/secrets/static/ssr_managed": { data: { ...detail, ...managedItem } },
      "GET /api/v1/static_secrets/ssr_managed": {
        data: { ...record, id: "ssr_managed", source: { source_type: "token_broker", config: { credential_id: "managed-gmail" } } },
      },
    });
    renderRoute(<SecretEditPage />, {
      path: "/console/secrets/:kind/:id/edit",
      url: "/console/secrets/static/ssr_managed/edit",
    });
    expect(await screen.findByText(/re-consent will not repair it/)).toHaveTextContent(
      "Managed secret. Maintained by the google OAuth integration (broker credential bcr_gmail).",
    );
    expect(screen.getByLabelText("Credential ID")).toHaveValue("managed-gmail");
  });

  it("preserves the GitHub token profile in replace mode", async () => {
    const github = {
      ...record,
      kind: "github_token",
      inject_config: null,
      replace_config: { proxy_value: "GITHUB_TOKEN", match_headers: ["Authorization"] },
      source: { source_type: "env", config: { var: "GITHUB_TOKEN" } },
    };
    const api = detailApi({
      "GET /api/v1/static_secrets/ssr_gh": { data: github },
      "PATCH /api/v1/static_secrets/ssr_gh": { data: github },
    });
    renderRoute(<SecretEditPage />, {
      path: "/console/secrets/:kind/:id/edit",
      url: "/console/secrets/static/ssr_gh/edit",
      me: adminMe(),
    });
    expect(await screen.findByLabelText("Placeholder value")).toHaveValue("GITHUB_TOKEN");
    expect(screen.getByRole("radio", { name: "Replace a placeholder value" })).toBeChecked();
    await userEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(api.calls("PATCH /api/v1/static_secrets/ssr_gh")).toHaveLength(1));
    expect(api.calls("PATCH /api/v1/static_secrets/ssr_gh")[0]?.body).toMatchObject({
      data: {
        kind: "github_token",
        inject_config: null,
        replace_config: { proxy_value: "GITHUB_TOKEN", match_headers: ["Authorization"] },
        source: { source_type: "env", config: { var: "GITHUB_TOKEN" } },
      },
    });
  });
});

const failure = apiResponse(500, { error: { message: "upstream exploded" } });

class Boundary extends Component<{ children: ReactNode }, { error: unknown }> {
  state = { error: null as unknown };
  static getDerivedStateFromError(error: unknown) {
    return { error };
  }
  render() {
    return this.state.error ? <p>boundary: {errorMessage(this.state.error)}</p> : this.props.children;
  }
}

describe("failed requests", () => {
  it("shows an error instead of the empty list state", async () => {
    mockApi({ "GET /api/v1/secrets": failure });
    renderRoute(<SecretsListPage />, { path: "/console/secrets" });
    expect(await screen.findByText("Could not load secrets: upstream exploded")).toBeInTheDocument();
    expect(screen.queryByText("No secrets yet.")).not.toBeInTheDocument();
    expect(screen.queryByRole("table", { name: "Secrets" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
  });

  it("keeps the rows and shows an error when a refetch fails", async () => {
    let fail = false;
    mockApi({
      "GET /api/v1/secrets": ({ url }: { url: URL }) =>
        fail ? failure : url.searchParams.get("limit") === "1" ? { data: [], meta: meta(1) } : { data: [listItem], meta: meta(1) },
    });
    const { queryClient } = renderRoute(<SecretsListPage />, { path: "/console/secrets" });
    expect(await screen.findByRole("link", { name: "GitHub token" })).toBeInTheDocument();
    fail = true;
    await queryClient.refetchQueries();
    expect(await screen.findByText("Could not load secrets: upstream exploded")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "GitHub token" })).toBeInTheDocument();
  });

  it("reports a failed secret count", async () => {
    mockApi({
      "GET /api/v1/secrets": ({ url }: { url: URL }) =>
        url.searchParams.get("limit") === "1" ? failure : { data: [listItem], meta: meta(1) },
    });
    renderRoute(<SecretsListPage />, { path: "/console/secrets" });
    expect(await screen.findByText("Could not load the secret count: upstream exploded")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "GitHub token" })).toBeInTheDocument();
  });

  it("shows an error instead of 'No roles assigned.' when role grants fail", async () => {
    detailApi({ "GET /api/v1/secrets/static/ssr_gh/role_grants": failure });
    renderRoute(<SecretDetailPage />, { path: "/console/secrets/:kind/:id", url: "/console/secrets/static/ssr_gh" });
    expect(await screen.findByText("Could not load role grants: upstream exploded")).toBeInTheDocument();
    expect(screen.queryByText("No roles assigned.")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Assign role" })).not.toBeInTheDocument();
  });

  it("sends an initial detail failure to the error boundary", async () => {
    mockApi({ "GET /api/v1/secrets/static/ssr_gh": failure });
    renderRoute(
      <Boundary>
        <SecretDetailPage />
      </Boundary>,
      { path: "/console/secrets/:kind/:id", url: "/console/secrets/static/ssr_gh" },
    );
    expect(await screen.findByText("boundary: upstream exploded")).toBeInTheDocument();
  });

  it("keeps the detail and shows an error when its refetch fails", async () => {
    let fail = false;
    detailApi({ "GET /api/v1/secrets/static/ssr_gh": () => (fail ? failure : { data: detail }) });
    const { queryClient } = renderRoute(<SecretDetailPage />, {
      path: "/console/secrets/:kind/:id",
      url: "/console/secrets/static/ssr_gh",
    });
    expect(await screen.findByRole("heading", { name: "GitHub token" })).toBeInTheDocument();
    fail = true;
    await queryClient.refetchQueries();
    expect(await screen.findByText("Could not load this secret: upstream exploded")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "GitHub token" })).toBeInTheDocument();
  });

  it("shows an error on the edit page when a refetch fails", async () => {
    let fail = false;
    detailApi({
      "GET /api/v1/static_secrets/ssr_gh": () =>
        fail ? failure : { data: { ...detail, source: { source_type: "env", config: { var: "X" } }, created_at: "" } },
    });
    const { queryClient } = renderRoute(<SecretEditPage />, {
      path: "/console/secrets/:kind/:id/edit",
      url: "/console/secrets/static/ssr_gh/edit",
    });
    expect(await screen.findByRole("heading", { name: "Edit GitHub token" })).toBeInTheDocument();
    fail = true;
    await queryClient.refetchQueries();
    expect(await screen.findByText("Could not load this secret: upstream exploded")).toBeInTheDocument();
  });
});

describe("client-side validation", () => {
  it("requires pg_dsn identity, database, and a source reference before sending", async () => {
    const api = mockApi({});
    renderRoute(<SecretNewPage />, { path: "/console/secrets/:kind/new", url: "/console/secrets/pg_dsn/new" });
    await userEvent.click(screen.getByRole("button", { name: "Add setting" }));
    await userEvent.type(screen.getByLabelText("Setting 1 value"), "orphan");
    await userEvent.click(screen.getByRole("button", { name: "Create secret" }));

    expect(await screen.findByText("4 errors prevented this secret from saving:")).toBeInTheDocument();
    expect(screen.getByText("Foreign ID can't be blank")).toBeInTheDocument();
    expect(screen.getByText("Database can't be blank")).toBeInTheDocument();
    expect(screen.getByText("Source: Environment variable can't be blank")).toBeInTheDocument();
    expect(screen.getByText("Setting 1: name can't be blank")).toBeInTheDocument();
    expect(screen.getByLabelText("Database *")).toHaveAttribute("aria-invalid", "true");
    expect(api.calls()).toHaveLength(0);
  });

  it("requires an inline value for a new inline source, scopes, and a host or CIDR per rule", async () => {
    const api = mockApi({});
    renderRoute(<SecretNewPage />, { path: "/console/secrets/:kind/new", url: "/console/secrets/gcp_auth/new" });
    await userEvent.click(screen.getByRole("combobox", { name: "Backend" }));
    await userEvent.click(await screen.findByRole("option", { name: "Inline Value" }));
    await userEvent.click(screen.getByRole("button", { name: "Add rule" }));
    await userEvent.click(within(screen.getByRole("group", { name: "Rule 1 HTTP methods" })).getByRole("button", { name: "GET" }));
    await userEvent.click(screen.getByRole("button", { name: "Add label" }));
    await userEvent.type(screen.getByLabelText("Labels 1 value"), "platform");
    await userEvent.click(screen.getByRole("button", { name: "Create secret" }));

    expect(await screen.findByText("Scopes must include at least one scope")).toBeInTheDocument();
    expect(screen.getByText("Source: Inline value can't be blank")).toBeInTheDocument();
    expect(screen.getByText("Rule 1: set a host or a CIDR")).toBeInTheDocument();
    expect(screen.getByText("Label 1: key can't be blank")).toBeInTheDocument();
    expect(api.calls()).toHaveLength(0);
  });

  it("requires a rule for gcp_id_token secrets", async () => {
    const api = mockApi({});
    renderRoute(<SecretNewPage />, { path: "/console/secrets/:kind/new", url: "/console/secrets/gcp_id_token/new" });
    await userEvent.type(screen.getByLabelText("Environment variable"), "KEYFILE");
    await userEvent.click(screen.getByRole("button", { name: "Create secret" }));
    expect(await screen.findByText("Rules must include at least one rule")).toBeInTheDocument();
    expect(api.calls()).toHaveLength(0);
  });
});

describe("inline values in client caches", () => {
  const PLAINTEXT = "postgres://u:plaintext-pw@db.example/shop";
  const cachedState = (queryClient: import("@tanstack/react-query").QueryClient) =>
    JSON.stringify([
      queryClient.getMutationCache().getAll().map((mutation) => mutation.state),
      queryClient.getQueryCache().getAll().map((query) => [query.queryKey, query.state.data]),
    ]);

  async function fillInlineDsn() {
    await userEvent.type(screen.getByLabelText("Foreign ID *"), "ui-shop");
    await userEvent.type(screen.getByLabelText("Database *"), "shop");
    await userEvent.click(screen.getByRole("combobox", { name: "Backend" }));
    await userEvent.click(await screen.findByRole("option", { name: "Inline Value" }));
    await userEvent.type(screen.getByLabelText("Inline value"), PLAINTEXT);
  }

  it("leaves no plaintext in the mutation or query cache after a successful create and unmount", async () => {
    const api = mockApi({ "POST /api/v1/pg_dsn_secrets": { data: { id: "pgd_new" } } });
    const { queryClient, unmount } = renderRoute(<SecretNewPage />, {
      path: "/console/secrets/:kind/new",
      url: "/console/secrets/pg_dsn/new",
      routes: [{ path: "/console/secrets/:kind/:id", element: <p>detail page</p> }],
    });
    await fillInlineDsn();
    await userEvent.click(screen.getByRole("button", { name: "Create secret" }));
    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(JSON.stringify(api.calls("POST /api/v1/pg_dsn_secrets")[0]?.body)).toContain(PLAINTEXT);
    unmount();
    await waitFor(() => expect(cachedState(queryClient)).not.toContain(PLAINTEXT));
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
  });

  it("clears the inline value from the form and caches after a rejected save", async () => {
    mockApi({
      "POST /api/v1/pg_dsn_secrets": apiResponse(422, {
        error: { message: "validation failed", details: { database: ["must match the DSN's database"] } },
      }),
    });
    const { queryClient, unmount } = renderRoute(<SecretNewPage />, {
      path: "/console/secrets/:kind/new",
      url: "/console/secrets/pg_dsn/new",
    });
    await fillInlineDsn();
    await userEvent.click(screen.getByRole("button", { name: "Create secret" }));
    expect(await screen.findByText("Database must match the DSN's database")).toBeInTheDocument();
    expect(screen.getByLabelText("Inline value")).toHaveValue("");
    expect(document.body.innerHTML).not.toContain(PLAINTEXT);
    await waitFor(() => expect(cachedState(queryClient)).not.toContain(PLAINTEXT));
    unmount();
    await waitFor(() => expect(cachedState(queryClient)).not.toContain(PLAINTEXT));
  });
});
