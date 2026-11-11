import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { toast } from "sonner";

import { apiResponse, mockApi, operatorMe, renderRoute } from "@/test/utils";

import RoleDetailPage from "./pages/RoleDetail";
import RoleEditPage from "./pages/RoleEdit";
import RoleNewPage from "./pages/RoleNew";
import RolesListPage from "./pages/RolesList";

vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

const meta = (total: number, page = 1, limit = 200) => ({
  page,
  limit,
  total,
  total_pages: Math.ceil(total / limit),
});

const role = {
  id: "role_infra",
  foreign_id: "acme-infra",
  name: "Infra",
  labels: { kind: "shared" },
  slack_channel_permissions: [
    { channel_id: "C0123456789", upload_enabled: true, download_enabled: false, history_enabled: true },
  ],
  created_at: "2026-06-01T10:00:00Z",
  updated_at: "2026-06-02T10:00:00Z",
};

const prodKey = { kind: "static", kind_label: "Static", id: "ssr_prod", foreign_id: "prod-key", name: "Prod API key" };
const bigQuery = { kind: "gcp_auth", kind_label: "GCP Auth", id: "gas_bq", foreign_id: null, name: "BigQuery" };

/**
 * GET /api/v1/secrets as the server answers it: `exclude_grantee` drops every
 * secret in `granted` (the grantee's grants across all pages).
 */
function secretsHandler(all: { id: string }[], granted: Record<string, string[]>) {
  return ({ url }: { url: URL }) => {
    const excluded = new Set(granted[url.searchParams.get("exclude_grantee") ?? ""] ?? []);
    const data = all.filter((secret) => !excluded.has(secret.id));
    const limit = Number(url.searchParams.get("limit") ?? 50);
    return { data: data.slice(0, limit), meta: meta(data.length, 1, limit) };
  };
}

function detailApi(overrides: Record<string, unknown> = {}) {
  return mockApi({
    "GET /api/v1/roles/role_infra": { data: role },
    "GET /api/v1/roles/role_infra/grants": {
      data: [{ id: "grant_1", role_id: "role_infra", static_secret_id: "ssr_prod", grantable: prodKey, created_at: "" }],
      meta: meta(1),
    },
    "GET /api/v1/slack_channels": { data: [{ channel_id: "C0123456789", name: "general" }], meta: { catalog_error: null } },
    "GET /api/v1/secrets": secretsHandler([prodKey, bigQuery], { "role:role_infra": ["ssr_prod"] }),
    ...overrides,
  });
}

const renderDetail = (options: Parameters<typeof renderRoute>[1] = {}) =>
  renderRoute(<RoleDetailPage />, { path: "/console/roles/:id", url: "/console/roles/role_infra", ...options });

describe("roles list", () => {
  it("lists roles with foreign id, oid, name, and labels", async () => {
    const api = mockApi({
      "GET /api/v1/roles": {
        data: [role, { ...role, id: "role_bare", foreign_id: null, name: null, labels: {} }],
        meta: meta(2),
      },
    });
    renderRoute(<RolesListPage />, { path: "/console/roles" });

    expect(await screen.findByRole("link", { name: "acme-infra" })).toHaveAttribute("href", "/console/roles/role_infra");
    expect(screen.getByText("role_infra")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "role_bare" })).toHaveAttribute("href", "/console/roles/role_bare");
    expect(screen.getByText("Infra")).toBeInTheDocument();
    expect(screen.getByText("unnamed")).toBeInTheDocument();
    expect(screen.getByText("kind=shared")).toBeInTheDocument();
    expect(screen.getByText("—")).toBeInTheDocument();
    expect(screen.getByText("2 roles.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Add Role" })).toHaveAttribute("href", "/console/roles/new");
    expect(screen.queryByText(/page 1 of/)).not.toBeInTheDocument();
    expect(api.calls("GET /api/v1/roles")[0]?.url.searchParams.get("limit")).toBe("200");
  });

  it("shows the empty state", async () => {
    mockApi({ "GET /api/v1/roles": { data: [], meta: meta(0) } });
    renderRoute(<RolesListPage />, { path: "/console/roles" });
    expect(await screen.findByText("No roles yet.")).toBeInTheDocument();
    expect(screen.getByText("0 roles.")).toBeInTheDocument();
  });

  it("shows a load error instead of the empty state", async () => {
    mockApi({ "GET /api/v1/roles": apiResponse(500, { error: { message: "database unavailable" } }) });
    renderRoute(<RolesListPage />, { path: "/console/roles" });
    expect(await screen.findByText("Could not load roles: database unavailable")).toBeInTheDocument();
    expect(screen.queryByText("No roles yet.")).not.toBeInTheDocument();
  });

  it("sends non-admins to their landing page without fetching", async () => {
    const api = mockApi({});
    renderRoute(<RolesListPage />, {
      path: "/console/roles",
      me: operatorMe({ nav: { ...operatorMe().nav, integrations: true } }),
      isSpaPath: () => true,
    });
    expect(await screen.findByText("other page")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/roles")).toHaveLength(0);
  });
});

describe("new role", () => {
  it("has no namespace field and creates a role with labels", async () => {
    const api = mockApi({ "POST /api/v1/roles": { data: { ...role, id: "role_new" } } });
    const { router } = renderRoute(<RoleNewPage />, {
      path: "/console/roles/new",
      routes: [{ path: "/console/roles/:id", element: <p>detail page</p> }],
    });

    expect(screen.queryByLabelText(/namespace/i)).not.toBeInTheDocument();
    expect(screen.getByText("Optional. A stable, URL-safe handle operators can reference.")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Foreign ID"), "payments");
    await userEvent.type(screen.getByLabelText("Name"), "Payments");
    await userEvent.click(screen.getByRole("button", { name: "Add label" }));
    await userEvent.type(screen.getByLabelText("Labels 1 key"), "team");
    await userEvent.type(screen.getByLabelText("Labels 1 value"), "finance");
    await userEvent.click(screen.getByRole("button", { name: "Create role" }));

    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/roles/role_new");
    expect(api.calls("POST /api/v1/roles")[0]?.body).toEqual({
      data: { foreign_id: "payments", name: "Payments", labels: { team: "finance" } },
    });
  });

  it("sends a blank foreign id as null", async () => {
    const api = mockApi({ "POST /api/v1/roles": { data: { ...role, id: "role_new" } } });
    renderRoute(<RoleNewPage />, {
      path: "/console/roles/new",
      routes: [{ path: "/console/roles/:id", element: <p>detail page</p> }],
    });
    await userEvent.type(screen.getByLabelText("Name"), "No handle");
    await userEvent.click(screen.getByRole("button", { name: "Create role" }));
    await screen.findByText("detail page");
    expect(api.calls("POST /api/v1/roles")[0]?.body).toEqual({ data: { foreign_id: null, name: "No handle", labels: {} } });
  });

  it("shows server validation errors without leaving the form", async () => {
    mockApi({
      "POST /api/v1/roles": apiResponse(422, {
        error: { message: "validation failed", details: { foreign_id: ["has already been taken"] } },
      }),
    });
    renderRoute(<RoleNewPage />, { path: "/console/roles/new" });
    await userEvent.type(screen.getByLabelText("Foreign ID"), "dup");
    await userEvent.click(screen.getByRole("button", { name: "Create role" }));

    expect(await screen.findByText("Role could not be saved.")).toBeInTheDocument();
    expect(screen.getAllByText(/has already been taken/).length).toBeGreaterThanOrEqual(2);
  });

  it("cancel returns to the list", () => {
    mockApi({});
    renderRoute(<RoleNewPage />, { path: "/console/roles/new" });
    expect(screen.getByRole("link", { name: "Cancel" })).toHaveAttribute("href", "/console/roles");
  });
});

describe("edit role", () => {
  it("shows the foreign id read-only and saves only name and labels", async () => {
    const api = mockApi({
      "GET /api/v1/roles/role_infra": { data: role },
      "PATCH /api/v1/roles/role_infra": { data: { ...role, name: "Infrastructure" } },
    });
    const { router } = renderRoute(<RoleEditPage />, {
      path: "/console/roles/:id/edit",
      url: "/console/roles/role_infra/edit",
      routes: [{ path: "/console/roles/:id", element: <p>detail page</p> }],
    });

    expect(await screen.findByRole("heading", { name: "Edit Role" })).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "Foreign ID" })).not.toBeInTheDocument();
    expect(screen.getByText("acme-infra")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Cancel" })).toHaveAttribute("href", "/console/roles/role_infra");

    const name = screen.getByLabelText("Name");
    await userEvent.clear(name);
    await userEvent.type(name, "Infrastructure");
    await userEvent.click(screen.getByRole("button", { name: "Save role" }));

    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/roles/role_infra");
    expect(api.calls("PATCH /api/v1/roles/role_infra")[0]?.body).toEqual({
      data: { name: "Infrastructure", labels: { kind: "shared" } },
    });
  });

  it("shows a dash for a role without a foreign id", async () => {
    mockApi({ "GET /api/v1/roles/role_infra": { data: { ...role, foreign_id: null } } });
    renderRoute(<RoleEditPage />, { path: "/console/roles/:id/edit", url: "/console/roles/role_infra/edit" });
    expect(await screen.findByText("—")).toBeInTheDocument();
  });
});

describe("role detail", () => {
  it("renders identity, labels, Slack permissions, and secret grants", async () => {
    detailApi();
    renderDetail();

    expect(await screen.findByRole("heading", { name: "Infra" })).toBeInTheDocument();
    expect(screen.getByText("role_infra")).toBeInTheDocument();
    expect(screen.getByText("acme-infra")).toBeInTheDocument();
    expect(screen.getByText("kind=shared")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Edit" })).toHaveAttribute("href", "/console/roles/role_infra/edit");

    expect(await screen.findByText("#general")).toBeInTheDocument();
    // Existing channels are immutable: shown as text, never as an editable channel input.
    expect(screen.queryByDisplayValue("C0123456789")).not.toBeInTheDocument();
    expect(screen.queryByText("Inherited From Roles")).not.toBeInTheDocument();
    expect(screen.queryByText("API-managed")).not.toBeInTheDocument();

    const grants = await screen.findByRole("table", { name: "Secret grants" });
    expect(within(grants).getByText("Static")).toBeInTheDocument();
    expect(within(grants).getByRole("link", { name: "ssr_prod" })).toHaveAttribute(
      "href",
      "/console/secrets/static/ssr_prod",
    );
    expect(within(grants).getByText("prod-key")).toBeInTheDocument();
    expect(within(grants).getByText("Prod API key")).toBeInTheDocument();
  });

  it("falls back to the foreign id and then the oid for the title", async () => {
    detailApi({ "GET /api/v1/roles/role_infra": { data: { ...role, name: null, foreign_id: null } } });
    renderDetail();
    expect(await screen.findByRole("heading", { name: "role_infra" })).toBeInTheDocument();
  });

  it("shows the empty grants state", async () => {
    detailApi({ "GET /api/v1/roles/role_infra/grants": { data: [], meta: meta(0) } });
    renderDetail();
    expect(await screen.findByText("No secrets granted.")).toBeInTheDocument();
  });

  it("grants a secret not already granted", async () => {
    const api = detailApi({ "POST /api/v1/grants": { data: { id: "grant_2" } } });
    renderDetail();
    await screen.findByRole("table", { name: "Secret grants" });

    await userEvent.click(await screen.findByRole("combobox", { name: "Secret to grant" }));
    expect(await screen.findByRole("option", { name: /BigQuery/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /Prod API key/ })).not.toBeInTheDocument();
    const pickerCall = api.calls("GET /api/v1/secrets").find((call) => call.url.searchParams.get("limit") === "200");
    expect(pickerCall?.url.searchParams.get("exclude_grantee")).toBe("role:role_infra");
    await userEvent.click(screen.getByRole("option", { name: /BigQuery/ }));
    await userEvent.click(screen.getByRole("button", { name: "Grant secret" }));

    await waitFor(() => expect(api.calls("POST /api/v1/grants")).toHaveLength(1));
    expect(api.calls("POST /api/v1/grants")[0]?.body).toEqual({ data: { role_id: "role_infra", gcp_auth_secret_id: "gas_bq" } });
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Granted BigQuery."));
  });

  it("excludes secrets granted on later grant pages from the picker", async () => {
    // Page 1 of the grants only holds ssr_prod; gas_bq is granted on page 2.
    detailApi({
      "GET /api/v1/roles/role_infra/grants": {
        data: [{ id: "grant_1", role_id: "role_infra", grantable: prodKey, created_at: "" }],
        meta: meta(250),
      },
      "GET /api/v1/secrets": secretsHandler(
        [prodKey, bigQuery, { kind: "hmac", kind_label: "HMAC", id: "hms_1", foreign_id: null, name: "Webhook" }],
        { "role:role_infra": ["ssr_prod", "gas_bq"] },
      ),
    });
    renderDetail();
    await userEvent.click(await screen.findByRole("combobox", { name: "Secret to grant" }));
    expect(await screen.findByRole("option", { name: /Webhook/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /BigQuery/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /Prod API key/ })).not.toBeInTheDocument();
  });

  it("hides the grant form when every secret is already granted", async () => {
    const api = detailApi({ "GET /api/v1/secrets": secretsHandler([prodKey], { "role:role_infra": ["ssr_prod"] }) });
    renderDetail();
    expect(await screen.findByRole("table", { name: "Secret grants" })).toBeInTheDocument();
    // Let the assignable-count request settle before checking the form stays hidden.
    await waitFor(() => expect(api.calls("GET /api/v1/secrets")).toHaveLength(1));
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(screen.queryByRole("button", { name: "Grant secret" })).not.toBeInTheDocument();
    expect(screen.queryByRole("combobox", { name: "Secret to grant" })).not.toBeInTheDocument();
  });

  it("shows a grants load error instead of the empty state", async () => {
    detailApi({
      "GET /api/v1/roles/role_infra/grants": apiResponse(500, { error: { message: "database unavailable" } }),
    });
    renderDetail();
    expect(
      await screen.findByText("Could not load the role's secret grants: database unavailable"),
    ).toBeInTheDocument();
    expect(screen.queryByText("No secrets granted.")).not.toBeInTheDocument();
  });

  it("reports a failed Slack channel-name request", async () => {
    detailApi({ "GET /api/v1/slack_channels": apiResponse(500, { error: { message: "database unavailable" } }) });
    renderDetail();
    expect(await screen.findByText("Could not load Slack channel names: database unavailable")).toBeInTheDocument();
  });

  it("asks for a secret before granting", async () => {
    const api = detailApi();
    renderDetail();
    await userEvent.click(await screen.findByRole("button", { name: "Grant secret" }));
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith("Pick a secret to grant."));
    expect(api.calls("POST /api/v1/grants")).toHaveLength(0);
  });

  it("revokes a grant after confirmation", async () => {
    const api = detailApi({ "DELETE /api/v1/grants/grant_1": undefined });
    renderDetail();
    const grants = await screen.findByRole("table", { name: "Secret grants" });
    await userEvent.click(await within(grants).findByRole("button", { name: "Revoke" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Revoke this grant?");
    await userEvent.click(within(dialog).getByRole("button", { name: "Revoke" }));
    await waitFor(() => expect(api.calls("DELETE /api/v1/grants/grant_1")).toHaveLength(1));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Revoked grant."));
  });

  it("replaces Slack channel permissions", async () => {
    const api = detailApi({ "PUT /api/v1/roles/role_infra/slack_channel_permissions": { data: [] } });
    renderDetail();
    await userEvent.click(await screen.findByRole("checkbox", { name: "Download for C0123456789" }));
    await userEvent.click(screen.getByRole("button", { name: "Save Slack channel permissions" }));

    await waitFor(() => expect(api.calls("PUT /api/v1/roles/role_infra/slack_channel_permissions")).toHaveLength(1));
    expect(api.calls("PUT /api/v1/roles/role_infra/slack_channel_permissions")[0]?.body).toEqual({
      data: {
        slack_channel_permissions: [
          { channel_id: "C0123456789", upload_enabled: true, download_enabled: true, history_enabled: true },
        ],
      },
    });
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Updated Slack channel permissions."));
  });

  it("shows server validation errors for Slack channel permissions", async () => {
    detailApi({
      "PUT /api/v1/roles/role_infra/slack_channel_permissions": apiResponse(422, {
        error: { message: "Select at least one Slack permission", details: {} },
      }),
    });
    renderDetail();
    await userEvent.click(await screen.findByRole("checkbox", { name: "Upload for C0123456789" }));
    await userEvent.click(screen.getByRole("checkbox", { name: "History for C0123456789" }));
    await userEvent.click(screen.getByRole("button", { name: "Save Slack channel permissions" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Select at least one Slack permission");
  });

  it("deletes one Slack channel permission", async () => {
    const api = detailApi({ "DELETE /api/v1/roles/role_infra/slack_channel_permissions/C0123456789": undefined });
    renderDetail();
    await userEvent.click(await screen.findByRole("button", { name: "Delete C0123456789" }));
    const dialog = await screen.findByRole("alertdialog");
    await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await waitFor(() =>
      expect(api.calls("DELETE /api/v1/roles/role_infra/slack_channel_permissions/C0123456789")).toHaveLength(1),
    );
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Deleted Slack channel permission."));
  });

  it("searches channel options excluding the role's channels", async () => {
    const api = detailApi();
    renderDetail();
    await userEvent.type(await screen.findByRole("combobox", { name: /channel/i }), "eng");
    await waitFor(() =>
      expect(
        api.calls("GET /api/v1/slack_channels").some((call) => call.url.searchParams.get("exclude_owner") === "role:role_infra"),
      ).toBe(true),
    );
  });
});
