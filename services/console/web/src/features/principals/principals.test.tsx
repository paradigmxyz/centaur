import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { adminMe, apiResponse, mockApi, operatorMe, renderRoute } from "@/test/utils";

import PrincipalDetailPage from "./pages/PrincipalDetail";
import PrincipalNewPage from "./pages/PrincipalNew";
import PrincipalsListPage from "./pages/PrincipalsList";

afterEach(() => {
  vi.unstubAllGlobals();
});

const meta = (total: number, page = 1) => ({ page, limit: 50, total, total_pages: Math.ceil(total / 50) });

const principal = {
  id: "prn_bob",
  foreign_id: "C0123456789",
  name: "Bob",
  kind: "slack_dm",
  slack_user_id: "U0123456789",
  labels: { team: "platform", "centaur.sandbox_repo_cache": "all" },
  roles: [{ id: "role_infra", foreign_id: "infra", name: "Infra" }],
  slack_channel_permissions: [
    { channel_id: "D0123456789", upload_enabled: true, download_enabled: false, history_enabled: true },
  ],
  inherited_slack_channel_permissions: [],
  effective_slack_channel_permissions: [],
  sandbox_repo_cache: "all",
  sandbox_observability_enabled: true,
  sandbox_sessions_read_enabled: true,
  sandbox_workflows_read_enabled: false,
  sandbox_workflows_write_enabled: false,
  created_at: "2026-06-01T10:00:00Z",
  updated_at: "2026-06-02T10:00:00Z",
};

const staticSecret = { kind: "static", kind_label: "Static", id: "ssr_gh", foreign_id: "github-token", name: "GitHub token" };
const bigQuery = { kind: "gcp_auth", kind_label: "GCP Auth", id: "gas_bq", foreign_id: null, name: "BigQuery" };

/**
 * GET /api/v1/secrets as the server answers it: `exclude_grantee` drops every
 * secret in `granted` (the grantee's direct grants across all pages).
 */
function secretsHandler(all: { id: string }[], granted: Record<string, string[]>) {
  return ({ url }: { url: URL }) => {
    const excluded = new Set(granted[url.searchParams.get("exclude_grantee") ?? ""] ?? []);
    const data = all.filter((secret) => !excluded.has(secret.id));
    const limit = Number(url.searchParams.get("limit") ?? 50);
    return { data: data.slice(0, limit), meta: { page: 1, limit, total: data.length, total_pages: Math.ceil(data.length / limit) } };
  };
}

const adminRole = { id: "role_admin", foreign_id: "admin", name: "Admin" };

/**
 * GET /api/v1/roles as the server answers it: `exclude_principal` drops the
 * roles in `assigned`, `q` matches name or foreign_id, and pages hold `limit`.
 */
function rolesHandler(all: { id: string; foreign_id: string | null; name: string | null }[], assigned: Record<string, string[]>) {
  return ({ url }: { url: URL }) => {
    const excluded = new Set(assigned[url.searchParams.get("exclude_principal") ?? ""] ?? []);
    const q = (url.searchParams.get("q") ?? "").toLowerCase();
    const data = all
      .filter((role) => !excluded.has(role.id))
      .filter((role) => !q || `${role.name ?? ""} ${role.foreign_id ?? ""}`.toLowerCase().includes(q));
    const limit = Number(url.searchParams.get("limit") ?? 50);
    const page = Number(url.searchParams.get("page") ?? 1);
    return {
      data: data.slice((page - 1) * limit, page * limit),
      meta: { page, limit, total: data.length, total_pages: Math.ceil(data.length / limit) },
    };
  };
}

function detailApi(overrides: Record<string, unknown> = {}) {
  return mockApi({
    "GET /api/v1/principals/prn_bob": { data: principal },
    "GET /api/v1/principals/prn_bob/roles": { data: principal.roles },
    "GET /api/v1/roles": rolesHandler([...principal.roles, adminRole], { prn_bob: ["role_infra"] }),
    "GET /api/v1/principals/prn_bob/grants": {
      data: [{ id: "grant_1", principal_id: "prn_bob", static_secret_id: "ssr_gh", grantable: staticSecret, created_at: "" }],
      meta: meta(1),
    },
    "GET /api/v1/principals/prn_bob/effective_grants": {
      data: [
        { ...staticSecret, grant_key: "static_secret_id", direct: true, roles: [] },
        { kind: "pg_dsn", id: "pgd_1", foreign_id: null, name: "Analytics", direct: false, roles: principal.roles },
      ],
      meta: meta(2),
    },
    "GET /api/v1/slack_channels": { data: [], meta: { catalog_error: "Slack channel catalog is loading. Enter a channel ID or reload shortly." } },
    "GET /api/v1/secrets": secretsHandler([staticSecret, bigQuery], { "principal:prn_bob": ["ssr_gh"] }),
    ...overrides,
  });
}

describe("principals list", () => {
  it("lists principals with kind and role pills, sorted and searchable", async () => {
    const api = mockApi({
      "GET /api/v1/principals": ({ url }: { url: URL }) =>
        url.searchParams.get("limit") === "1"
          ? { data: [], meta: meta(51) }
          : { data: [principal, { ...principal, id: "prn_x", name: null, roles: [] }], meta: meta(51) },
    });
    renderRoute(<PrincipalsListPage />, { path: "/console/principals", url: "/console/principals?q=bob" });

    expect(await screen.findByRole("link", { name: "Bob" })).toHaveAttribute("href", "/console/principals/prn_bob");
    expect(screen.getAllByText("SLACK_DM")).toHaveLength(2);
    expect(screen.getByText("Infra")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "unnamed" })).toBeInTheDocument();
    expect(await screen.findByText("51 principals total.")).toBeInTheDocument();
    expect(screen.getByText(/page 1 of 2/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Add Principal" })).toHaveAttribute("href", "/console/principals/new");

    const listCall = api.calls("GET /api/v1/principals").find((call) => call.url.searchParams.get("view") === "console");
    expect(listCall?.url.searchParams.get("q")).toBe("bob");
    expect(listCall?.url.searchParams.get("sort")).toBe("name");
  });

  it("shows the search empty state", async () => {
    mockApi({ "GET /api/v1/principals": { data: [], meta: meta(0) } });
    renderRoute(<PrincipalsListPage />, { path: "/console/principals", url: "/console/principals?q=zzz" });
    expect(await screen.findByText("No principals match “zzz”.")).toBeInTheDocument();
  });

  it("shows a load error instead of the empty state", async () => {
    mockApi({
      "GET /api/v1/principals": apiResponse(500, { error: { message: "database unavailable" } }),
    });
    renderRoute(<PrincipalsListPage />, { path: "/console/principals" });
    expect(await screen.findByText("Could not load principals: database unavailable")).toBeInTheDocument();
    expect(screen.queryByText("No principals yet.")).not.toBeInTheDocument();
  });

  it("reports a failed principal count instead of a stale total", async () => {
    let fail = false;
    mockApi({
      "GET /api/v1/principals": ({ url }: { url: URL }) =>
        url.searchParams.get("limit") === "1"
          ? fail
            ? apiResponse(500, { error: { message: "database unavailable" } })
            : { data: [], meta: meta(3) }
          : { data: [principal], meta: meta(1) },
    });
    const { queryClient } = renderRoute(<PrincipalsListPage />, { path: "/console/principals" });
    expect(await screen.findByText("3 principals total.")).toBeInTheDocument();
    fail = true;
    await queryClient.refetchQueries({ queryKey: ["principals"] });
    expect(await screen.findByText("Could not load the principal count: database unavailable")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Bob" })).toBeInTheDocument();
  });

  it("sends non-admins to their landing page without fetching", async () => {
    const api = mockApi({});
    renderRoute(<PrincipalsListPage />, {
      path: "/console/principals",
      me: operatorMe({ nav: { ...operatorMe().nav, integrations: true } }),
      isSpaPath: () => true,
    });
    expect(await screen.findByText("other page")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/principals")).toHaveLength(0);
  });
});

describe("new principal", () => {
  it("creates a principal with labels and opens it", async () => {
    const api = mockApi({ "POST /api/v1/principals": { data: { ...principal, id: "prn_new" } } });
    const { router } = renderRoute(<PrincipalNewPage />, {
      path: "/console/principals/new",
      routes: [{ path: "/console/principals/:id", element: <p>detail page</p> }],
    });

    await userEvent.type(screen.getByLabelText("Foreign ID"), "C-new");
    await userEvent.type(screen.getByLabelText("Display name"), "New one");
    await userEvent.click(screen.getByRole("button", { name: "Add label" }));
    await userEvent.type(screen.getByLabelText("Labels 1 key"), "team");
    await userEvent.type(screen.getByLabelText("Labels 1 value"), "platform");
    await userEvent.click(screen.getByRole("button", { name: "Add Principal" }));

    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/principals/prn_new");
    expect(api.calls("POST /api/v1/principals")[0]?.body).toEqual({
      data: { foreign_id: "C-new", name: "New one", labels: { team: "platform" } },
    });
    expect(api.calls("POST /api/v1/principals")[0]?.headers["X-CSRF-Token"]).toBe("csrf-test-token");
  });

  it("shows server validation errors on the field and in the summary", async () => {
    mockApi({
      "POST /api/v1/principals": apiResponse(422, {
        error: { message: "validation failed", details: { foreign_id: ["has already been taken"] } },
      }),
    });
    renderRoute(<PrincipalNewPage />, { path: "/console/principals/new" });
    await userEvent.type(screen.getByLabelText("Foreign ID"), "dup");
    await userEvent.click(screen.getByRole("button", { name: "Add Principal" }));

    expect(await screen.findByText("Principal could not be saved.")).toBeInTheDocument();
    expect(screen.getAllByText(/has already been taken/).length).toBeGreaterThanOrEqual(2);
  });
});

describe("principal detail", () => {
  it("renders identity, labels, DM rows, sandbox access, roles, and grants", async () => {
    detailApi();
    renderRoute(<PrincipalDetailPage />, { path: "/console/principals/:id", url: "/console/principals/prn_bob" });

    expect(await screen.findByRole("heading", { name: "Bob" })).toBeInTheDocument();
    expect(screen.getByText("prn_bob")).toBeInTheDocument();
    expect(screen.getByText("team=platform")).toBeInTheDocument();
    expect(screen.queryByText(/slack_user_id=/)).not.toBeInTheDocument();
    expect(await screen.findByText("DM Bob")).toBeInTheDocument();
    expect(await screen.findByText(/catalog is loading/)).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Observability" })).toBeChecked();

    const roles = await screen.findByRole("table", { name: "Roles" });
    expect(within(roles).getByText("role_infra")).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Role to assign" })).toHaveTextContent("Admin");

    const direct = await screen.findByRole("table", { name: "Direct grants" });
    expect(within(direct).getByRole("link", { name: "ssr_gh" })).toHaveAttribute("href", "/console/secrets/static/ssr_gh");

    const effective = await screen.findByRole("table", { name: "Effective grants" });
    expect(within(effective).getByText("direct")).toBeInTheDocument();
    expect(within(effective).getByText("via Infra")).toBeInTheDocument();
  });

  it("saves sandbox access", async () => {
    const api = detailApi({ "PATCH /api/v1/principals/prn_bob": { data: principal } });
    renderRoute(<PrincipalDetailPage />, { path: "/console/principals/:id", url: "/console/principals/prn_bob" });
    await userEvent.click(await screen.findByRole("checkbox", { name: "Write workflows" }));
    await userEvent.click(screen.getByRole("button", { name: "Save sandbox access" }));
    await waitFor(() => expect(api.calls("PATCH /api/v1/principals/prn_bob")).toHaveLength(1));
    expect(api.calls("PATCH /api/v1/principals/prn_bob")[0]?.body).toEqual({
      data: {
        sandbox_repo_cache: "all",
        sandbox_observability_enabled: true,
        sandbox_sessions_read_enabled: true,
        sandbox_workflows_read_enabled: false,
        sandbox_workflows_write_enabled: true,
      },
    });
  });

  it("assigns a role and grants a secret not already granted", async () => {
    const api = detailApi({
      "POST /api/v1/principals/prn_bob/roles": { data: { id: "role_admin" } },
      "POST /api/v1/grants": { data: { id: "grant_2" } },
    });
    renderRoute(<PrincipalDetailPage />, { path: "/console/principals/:id", url: "/console/principals/prn_bob" });

    // The first assignable role is preselected, as the legacy select did.
    expect(await screen.findByRole("combobox", { name: "Role to assign" })).toHaveTextContent("Admin");
    await userEvent.click(await screen.findByRole("button", { name: "Assign role" }));
    await waitFor(() => expect(api.calls("POST /api/v1/principals/prn_bob/roles")).toHaveLength(1));
    expect(api.calls("POST /api/v1/principals/prn_bob/roles")[0]?.body).toEqual({ data: { role_id: "role_admin" } });
    expect(api.calls("GET /api/v1/roles").every((call) => call.url.searchParams.get("exclude_principal") === "prn_bob")).toBe(true);

    await userEvent.click(screen.getByRole("combobox", { name: "Secret to grant" }));
    expect(await screen.findByRole("option", { name: /BigQuery/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /GitHub token/ })).not.toBeInTheDocument();
    const pickerCall = api.calls("GET /api/v1/secrets").find((call) => call.url.searchParams.get("limit") === "200");
    expect(pickerCall?.url.searchParams.get("exclude_grantee")).toBe("principal:prn_bob");
    await userEvent.click(screen.getByRole("option", { name: /BigQuery/ }));
    await userEvent.click(screen.getByRole("button", { name: "Grant secret" }));
    await waitFor(() => expect(api.calls("POST /api/v1/grants")).toHaveLength(1));
    expect(api.calls("POST /api/v1/grants")[0]?.body).toEqual({
      data: { principal_id: "prn_bob", gcp_auth_secret_id: "gas_bq" },
    });
  });

  it("deletes after confirmation and returns to the list", async () => {
    const api = detailApi({ "DELETE /api/v1/principals/prn_bob": undefined });
    const { router } = renderRoute(<PrincipalDetailPage />, {
      path: "/console/principals/:id",
      url: "/console/principals/prn_bob",
      routes: [{ path: "/console/principals", element: <p>list page</p> }],
    });
    await userEvent.click(await screen.findByRole("button", { name: "Delete" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("MCP tokens");
    await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    expect(await screen.findByText("list page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/principals");
    expect(api.calls("DELETE /api/v1/principals/prn_bob")).toHaveLength(1);
  });
});

describe("principal detail failures and limits", () => {
  const renderDetail = () =>
    renderRoute(<PrincipalDetailPage />, { path: "/console/principals/:id", url: "/console/principals/prn_bob" });

  it("excludes secrets granted on later grant pages from the picker", async () => {
    // Page 1 of the direct grants only holds ssr_gh; gas_bq is granted on page 2.
    const api = detailApi({
      "GET /api/v1/principals/prn_bob/grants": {
        data: [{ id: "grant_1", principal_id: "prn_bob", static_secret_id: "ssr_gh", grantable: staticSecret, created_at: "" }],
        meta: meta(60),
      },
      "GET /api/v1/secrets": secretsHandler(
        [staticSecret, bigQuery, { kind: "hmac", kind_label: "HMAC", id: "hms_1", foreign_id: null, name: "Webhook" }],
        { "principal:prn_bob": ["ssr_gh", "gas_bq"] },
      ),
    });
    renderDetail();
    await userEvent.click(await screen.findByRole("combobox", { name: "Secret to grant" }));
    expect(await screen.findByRole("option", { name: /Webhook/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /BigQuery/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /GitHub token/ })).not.toBeInTheDocument();
    expect(api.calls("GET /api/v1/principals/prn_bob/grants").every((call) => call.url.searchParams.get("limit") !== "200")).toBe(true);
  });

  it("hides the grant form when every secret is already granted directly", async () => {
    const api = detailApi({ "GET /api/v1/secrets": secretsHandler([staticSecret], { "principal:prn_bob": ["ssr_gh"] }) });
    renderDetail();
    expect(await screen.findByRole("table", { name: "Direct grants" })).toBeInTheDocument();
    // Let the assignable-count request settle before checking the form stays hidden.
    await waitFor(() => expect(api.calls("GET /api/v1/secrets")).toHaveLength(1));
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(screen.queryByRole("button", { name: "Grant secret" })).not.toBeInTheDocument();
    expect(screen.queryByRole("combobox", { name: "Secret to grant" })).not.toBeInTheDocument();
  });

  it("offers and assigns roles beyond the first 200 through server-side search", async () => {
    const many = Array.from({ length: 201 }, (_, i) => ({
      id: `role_${String(i).padStart(3, "0")}`,
      foreign_id: `team-${String(i).padStart(3, "0")}`,
      name: `Team ${String(i).padStart(3, "0")}`,
    }));
    const api = detailApi({
      "GET /api/v1/roles": rolesHandler([...principal.roles, ...many], { prn_bob: ["role_infra"] }),
      "POST /api/v1/principals/prn_bob/roles": { data: { id: "role_200" } },
    });
    renderDetail();

    await userEvent.click(await screen.findByRole("combobox", { name: "Role to assign" }));
    expect(await screen.findByRole("option", { name: "Team 000" })).toBeInTheDocument();
    expect(screen.getAllByRole("option")).toHaveLength(200);
    expect(screen.queryByRole("option", { name: "Team 200" })).not.toBeInTheDocument();
    expect(screen.queryByRole("option", { name: "Infra" })).not.toBeInTheDocument();
    expect(screen.getByText("Showing 200 of 201 roles. Search to find the rest.")).toBeInTheDocument();

    await userEvent.type(screen.getByPlaceholderText("Search roles"), "team-200");
    await userEvent.click(await screen.findByRole("option", { name: "Team 200" }));
    expect(screen.getByRole("combobox", { name: "Role to assign" })).toHaveTextContent("Team 200");
    await userEvent.click(screen.getByRole("button", { name: "Assign role" }));
    await waitFor(() => expect(api.calls("POST /api/v1/principals/prn_bob/roles")).toHaveLength(1));
    expect(api.calls("POST /api/v1/principals/prn_bob/roles")[0]?.body).toEqual({ data: { role_id: "role_200" } });
    const search = api.calls("GET /api/v1/roles").find((call) => call.url.searchParams.get("q") === "team-200");
    expect(search?.url.searchParams.get("exclude_principal")).toBe("prn_bob");
  });

  it("hides the assign form only when every role is assigned", async () => {
    const api = detailApi({ "GET /api/v1/roles": rolesHandler(principal.roles, { prn_bob: ["role_infra"] }) });
    renderDetail();
    expect(await screen.findByRole("table", { name: "Roles" })).toBeInTheDocument();
    await waitFor(() => expect(api.calls("GET /api/v1/roles")).toHaveLength(1));
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(screen.queryByRole("button", { name: "Assign role" })).not.toBeInTheDocument();
    expect(screen.queryByRole("combobox", { name: "Role to assign" })).not.toBeInTheDocument();
  });

  it("keeps the assign form when the assignable-roles request fails, and reports picker errors", async () => {
    const api = detailApi({ "GET /api/v1/roles": apiResponse(500, { error: { message: "database unavailable" } }) });
    renderDetail();
    await userEvent.click(await screen.findByRole("combobox", { name: "Role to assign" }));
    expect(await screen.findByText("Could not load roles: database unavailable")).toBeInTheDocument();
    expect(screen.queryByText("No roles found.")).not.toBeInTheDocument();
    await userEvent.keyboard("{Escape}");
    // Nothing is preselected, so assigning asks for a pick instead of posting.
    await userEvent.click(screen.getByRole("button", { name: "Assign role" }));
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(api.calls("POST /api/v1/principals/prn_bob/roles")).toHaveLength(0);
  });

  it("reports failed Slack channel-name and secret-picker requests instead of hiding them", async () => {
    const failure = apiResponse(500, { error: { message: "database unavailable" } });
    detailApi({
      "GET /api/v1/slack_channels": failure,
      "GET /api/v1/secrets": ({ url }: { url: URL }) =>
        url.searchParams.get("limit") === "1" ? { data: [], meta: meta(5) } : failure,
    });
    renderDetail();
    expect(await screen.findByText("Could not load Slack channel names: database unavailable")).toBeInTheDocument();
    await userEvent.click(await screen.findByRole("combobox", { name: "Secret to grant" }));
    expect(await screen.findByText("Could not load secrets: database unavailable")).toBeInTheDocument();
    expect(screen.queryByText("No secrets found.")).not.toBeInTheDocument();
  });

  it("keeps the assign forms when a count refetch fails after an empty result", async () => {
    let fail = false;
    const failure = apiResponse(500, { error: { message: "database unavailable" } });
    detailApi({
      "GET /api/v1/roles": (call: { url: URL }) =>
        fail ? failure : rolesHandler(principal.roles, { prn_bob: ["role_infra"] })(call),
      "GET /api/v1/secrets": (call: { url: URL }) =>
        fail ? failure : secretsHandler([staticSecret], { "principal:prn_bob": ["ssr_gh"] })(call),
    });
    const { queryClient } = renderDetail();
    expect(await screen.findByRole("table", { name: "Direct grants" })).toBeInTheDocument();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(screen.queryByRole("button", { name: "Assign role" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Grant secret" })).not.toBeInTheDocument();
    fail = true;
    await queryClient.refetchQueries({ predicate: (query) => query.queryKey[0] !== "principals" });
    expect(await screen.findByRole("button", { name: "Assign role" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Grant secret" })).toBeInTheDocument();
  });

  it("shows load errors instead of empty states for grants and roles", async () => {
    const failure = apiResponse(500, { error: { message: "database unavailable" } });
    detailApi({
      "GET /api/v1/principals/prn_bob/grants": failure,
      "GET /api/v1/principals/prn_bob/effective_grants": failure,
      "GET /api/v1/principals/prn_bob/roles": failure,
    });
    renderDetail();
    expect(await screen.findByText(/Could not load direct grants: database unavailable/)).toBeInTheDocument();
    expect(await screen.findByText(/Could not load effective grants: database unavailable/)).toBeInTheDocument();
    expect(await screen.findByText(/Could not load assigned roles: database unavailable/)).toBeInTheDocument();
    expect(screen.queryByText(/No direct grants/)).not.toBeInTheDocument();
    expect(screen.queryByText("This principal has no effective grants.")).not.toBeInTheDocument();
    expect(screen.queryByText("No roles assigned.")).not.toBeInTheDocument();
  });
});

describe("session fixtures", () => {
  it("admin fixture can see principals", () => {
    expect(adminMe().nav.principals).toBe(true);
  });
});
