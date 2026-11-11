import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { adminMe, apiResponse, mockApi, operatorMe, renderRoute } from "@/test/utils";

import ApiClientDetailPage from "./pages/ApiClientDetail";
import ApiClientEditPage from "./pages/ApiClientEdit";
import ApiClientNewPage from "./pages/ApiClientNew";
import ApiClientsListPage from "./pages/ApiClientsList";
import SystemSettingsPage from "./pages/SystemSettingsPage";
import UsersPage from "./pages/UsersPage";

const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }));
vi.mock("sonner", () => ({ toast }));

beforeEach(() => {
  toast.success.mockReset();
  toast.error.mockReset();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

const apiClientsMe = () => adminMe({ features: { api_clients: true, password_login: true }, nav: { ...adminMe().nav, api_clients: true } });

function consoleUser(overrides: Record<string, unknown>) {
  return {
    id: "usr_x",
    email: "x@example.test",
    name: null,
    status: "active",
    admin: false,
    current: false,
    identity_providers: [],
    approved_at: null,
    approved_by: null,
    created_at: "2026-06-01T10:00:00Z",
    updated_at: "2026-06-01T10:00:00Z",
    ...overrides,
  };
}

const me = consoleUser({ id: "usr_admin", email: "admin@example.test", admin: true, current: true, identity_providers: ["google"] });
const otherAdmin = consoleUser({ id: "usr_other", email: "other@example.test", admin: true });
const operator = consoleUser({ id: "usr_op", email: "op@example.test", name: "Op", identity_providers: ["google", "okta"] });
const disabled = consoleUser({ id: "usr_off", email: "off@example.test", status: "disabled" });
const pending = consoleUser({ id: "usr_new", email: "new@example.test", status: "pending", name: "Newbie" });

describe("users page", () => {
  const users = { data: [me, otherAdmin, operator, disabled, pending], meta: { total: 5 } };

  it("lists pending users separately and offers role changes without self-demotion", async () => {
    mockApi({ "GET /api/v1/users": users });
    renderRoute(<UsersPage />, { path: "/console/users" });

    const pendingSection = await screen.findByRole("region", { name: "1 pending approval" });
    expect(within(pendingSection).getByText("new@example.test")).toBeInTheDocument();
    expect(within(pendingSection).getByText("Newbie")).toBeInTheDocument();
    expect(within(pendingSection).getByText("Password")).toBeInTheDocument();
    expect(within(pendingSection).getByText(/requested/)).toBeInTheDocument();

    const table = screen.getByRole("table", { name: "Users" });
    expect(within(table).queryByText("new@example.test")).not.toBeInTheDocument();
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(4);

    const [self, other, op, off] = rows as [HTMLElement, HTMLElement, HTMLElement, HTMLElement];
    expect(within(self).getByText("(you)")).toBeInTheDocument();
    expect(within(self).getByText("Google")).toBeInTheDocument();
    expect(within(self).queryByRole("button")).not.toBeInTheDocument();
    expect(within(other).getByRole("button", { name: "Make user" })).toBeInTheDocument();
    expect(within(other).getByRole("button", { name: "Disable" })).toBeInTheDocument();
    expect(within(op).getByText("operator")).toBeInTheDocument();
    expect(within(op).getByText("Okta")).toBeInTheDocument();
    expect(within(op).getByRole("button", { name: "Make admin" })).toBeInTheDocument();
    expect(within(off).getByText("disabled")).toBeInTheDocument();
    expect(within(off).getByRole("button", { name: "Make admin" })).toBeInTheDocument();
    expect(within(off).queryByRole("button", { name: "Disable" })).not.toBeInTheDocument();
  });

  it("approves, promotes, demotes, and disables with the legacy notices", async () => {
    const api = mockApi({
      "GET /api/v1/users": users,
      "POST /api/v1/users/usr_new/approve": { data: { ...pending, status: "active" } },
      "POST /api/v1/users/usr_op/promote": { data: { ...operator, admin: true } },
      "POST /api/v1/users/usr_other/demote": { data: { ...otherAdmin, admin: false } },
      "POST /api/v1/users/usr_other/disable": { data: { ...otherAdmin, status: "disabled" } },
    });
    renderRoute(<UsersPage />, { path: "/console/users" });

    await userEvent.click(await screen.findByRole("button", { name: "Approve" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Approved new@example.test."));

    await userEvent.click(within(screen.getByRole("row", { name: /op@example\.test/ })).getByRole("button", { name: "Make admin" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("op@example.test is now an admin."));

    await userEvent.click(screen.getByRole("button", { name: "Make user" }));
    let dialog = await screen.findByRole("alertdialog", { name: "Make other@example.test a user?" });
    await userEvent.click(within(dialog).getByRole("button", { name: "Make user" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("other@example.test is now a user."));

    await userEvent.click(within(screen.getByRole("row", { name: /other@example\.test/ })).getByRole("button", { name: "Disable" }));
    dialog = await screen.findByRole("alertdialog", { name: "Disable other@example.test?" });
    await userEvent.click(within(dialog).getByRole("button", { name: "Disable" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Disabled other@example.test."));

    expect(api.calls("POST /api/v1/users/usr_other/disable")).toHaveLength(1);
    expect(api.calls("GET /api/v1/users").length).toBeGreaterThan(1);
  });

  it("reports server refusals", async () => {
    mockApi({
      "GET /api/v1/users": { data: [me, operator], meta: { total: 2 } },
      "POST /api/v1/users/usr_op/promote": apiResponse(422, { error: { message: "You can't demote your own account." } }),
    });
    renderRoute(<UsersPage />, { path: "/console/users" });
    await userEvent.click(await screen.findByRole("button", { name: "Make admin" }));
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith("You can't demote your own account."));
  });

  it("shows the empty state", async () => {
    mockApi({ "GET /api/v1/users": { data: [pending], meta: { total: 1 } } });
    renderRoute(<UsersPage />, { path: "/console/users" });
    expect(await screen.findByText("No approved users yet.")).toBeInTheDocument();
  });

  it("sends non-admins away without fetching", async () => {
    const api = mockApi({});
    renderRoute(<UsersPage />, { path: "/console/users", me: operatorMe({ nav: { ...operatorMe().nav, integrations: true } }) });
    expect(await screen.findByText("other page")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/users")).toHaveLength(0);
  });
});

const link = (id: string, isDefault: boolean, principal: Record<string, unknown>) => ({
  id,
  default: isDefault,
  principal: { id: "prn_a", foreign_id: null, name: null, kind: "slack_channel", ...principal },
});

const client = {
  id: "apc_eval",
  name: "eval-runner",
  scopes: ["sessions:read", "sessions:write"],
  disabled: false,
  disabled_at: null,
  principals_count: 2,
  principals: [
    link("acp_1", true, { id: "prn_a", name: "Evals", foreign_id: "C0EVALS" }),
    link("acp_2", false, { id: "prn_b", kind: "workflow" }),
  ],
  created_at: "2026-06-01T10:00:00Z",
  updated_at: "2026-06-01T10:00:00Z",
};

describe("API clients list", () => {
  it("lists clients with scopes, principal counts, and status", async () => {
    mockApi({ "GET /api/v1/api_clients": { data: [client, { ...client, id: "apc_off", name: "old", disabled: true }], meta: { total: 2 } } });
    renderRoute(<ApiClientsListPage />, { path: "/console/api_clients", me: apiClientsMe() });

    expect(await screen.findByRole("link", { name: "apc_eval" })).toHaveAttribute("href", "/console/api_clients/apc_eval");
    expect(screen.getByText("eval-runner")).toBeInTheDocument();
    expect(screen.getAllByText("sessions:write")).toHaveLength(2);
    expect(screen.getByText("active")).toBeInTheDocument();
    expect(screen.getByText("disabled")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Add API Client" })).toHaveAttribute("href", "/console/api_clients/new");
  });

  it("shows the empty state", async () => {
    mockApi({ "GET /api/v1/api_clients": { data: [], meta: { total: 0 } } });
    renderRoute(<ApiClientsListPage />, { path: "/console/api_clients", me: apiClientsMe() });
    expect(await screen.findByText("No API clients yet.")).toBeInTheDocument();
  });

  it("is unreachable while the feature is off", async () => {
    const api = mockApi({});
    renderRoute(<ApiClientsListPage />, { path: "/console/api_clients", me: adminMe() });
    expect(await screen.findByText("other page")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/api_clients")).toHaveLength(0);
  });
});

describe("new API client", () => {
  it("creates a client and shows its secret once on the detail page", async () => {
    const api = mockApi({
      "POST /api/v1/api_clients": { data: { ...client, secret: "plain-secret-value" } },
      "GET /api/v1/api_clients/apc_eval": { data: client },
    });
    const { router } = renderRoute(<ApiClientNewPage />, {
      path: "/console/api_clients/new",
      me: apiClientsMe(),
      routes: [{ path: "/console/api_clients/:id", element: <ApiClientDetailPage /> }],
    });

    expect(screen.getByLabelText("sessions:read")).toBeChecked();
    expect(screen.getByLabelText("sessions:write")).toBeChecked();
    await userEvent.type(screen.getByLabelText("Name *"), "eval-runner");
    await userEvent.type(screen.getByLabelText("Default principal"), "C0EVALS");
    await userEvent.click(screen.getByLabelText("sessions:read"));
    await userEvent.click(screen.getByRole("button", { name: "Create API client" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/console/api_clients/apc_eval"));
    expect(api.calls("POST /api/v1/api_clients")[0]?.body).toEqual({
      data: { name: "eval-runner", scopes: ["sessions:write"], principal: "C0EVALS" },
    });
    expect(toast.success).toHaveBeenCalledWith("API client created.");
    expect(await screen.findByRole("textbox", { name: "Client secret" })).toHaveValue("plain-secret-value");
    expect(screen.getByText(/It is shown only once/)).toBeInTheDocument();

    // A later visit cannot show it again.
    await router.navigate("/console/api_clients/new");
    expect(await screen.findByRole("heading", { name: "New API Client" })).toBeInTheDocument();
    await router.navigate("/console/api_clients/apc_eval");
    expect(await screen.findByRole("heading", { name: "eval-runner" })).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "Client secret" })).not.toBeInTheDocument();
  });

  it("shows an unknown principal error", async () => {
    mockApi({
      "POST /api/v1/api_clients": apiResponse(422, {
        error: { message: "validation failed", details: { base: ['No principal matches "nope".'] } },
      }),
    });
    renderRoute(<ApiClientNewPage />, { path: "/console/api_clients/new", me: apiClientsMe() });
    await userEvent.type(screen.getByLabelText("Name *"), "x");
    await userEvent.type(screen.getByLabelText("Default principal"), "nope");
    await userEvent.click(screen.getByRole("button", { name: "Create API client" }));
    expect(await screen.findByText("API client could not be saved.")).toBeInTheDocument();
    expect(screen.getByText('No principal matches "nope".')).toBeInTheDocument();
  });

  it("requires a name", async () => {
    const api = mockApi({});
    renderRoute(<ApiClientNewPage />, { path: "/console/api_clients/new", me: apiClientsMe() });
    await userEvent.click(screen.getByRole("button", { name: "Create API client" }));
    expect(await screen.findByText("can't be blank")).toBeInTheDocument();
    expect(api.calls("POST /api/v1/api_clients")).toHaveLength(0);
  });
});

describe("edit API client", () => {
  it("updates name and scopes without a principal field", async () => {
    const api = mockApi({
      "GET /api/v1/api_clients/apc_eval": { data: client },
      "PATCH /api/v1/api_clients/apc_eval": { data: { ...client, name: "renamed", scopes: ["sessions:read"] } },
    });
    const { router } = renderRoute(<ApiClientEditPage />, {
      path: "/console/api_clients/:id/edit",
      url: "/console/api_clients/apc_eval/edit",
      me: apiClientsMe(),
    });

    const name = await screen.findByLabelText("Name *");
    expect(screen.queryByLabelText("Default principal")).not.toBeInTheDocument();
    await userEvent.clear(name);
    await userEvent.type(name, "renamed");
    await userEvent.click(screen.getByLabelText("sessions:write"));
    await userEvent.click(screen.getByRole("button", { name: "Save API client" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/console/api_clients/apc_eval"));
    expect(api.calls("PATCH /api/v1/api_clients/apc_eval")[0]?.body).toEqual({
      data: { name: "renamed", scopes: ["sessions:read"] },
    });
    expect(toast.success).toHaveBeenCalledWith("API client updated.");
  });
});

describe("API client detail", () => {
  const renderDetail = (overrides: Record<string, unknown> = {}) => {
    const api = mockApi({ "GET /api/v1/api_clients/apc_eval": { data: client }, ...overrides });
    const result = renderRoute(<ApiClientDetailPage />, {
      path: "/console/api_clients/:id",
      url: "/console/api_clients/apc_eval",
      me: apiClientsMe(),
    });
    return { api, ...result };
  };

  it("shows the client, its scopes, and principals with default and actions", async () => {
    renderDetail();
    expect(await screen.findByRole("heading", { name: "eval-runner" })).toBeInTheDocument();
    expect(screen.getByText("apc_eval")).toBeInTheDocument();
    expect(screen.getByText("active")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Edit" })).toHaveAttribute("href", "/console/api_clients/apc_eval/edit");
    expect(screen.queryByRole("textbox", { name: "Client secret" })).not.toBeInTheDocument();

    const table = screen.getByRole("table", { name: "Attached principals" });
    const [first, second] = within(table).getAllByRole("row").slice(1) as [HTMLElement, HTMLElement];
    expect(within(first).getByRole("link", { name: "prn_a" })).toHaveAttribute("href", "/console/principals/prn_a");
    expect(within(first).getByText("C0EVALS")).toBeInTheDocument();
    expect(within(first).getByText("Evals")).toBeInTheDocument();
    expect(within(first).getByText("default")).toBeInTheDocument();
    expect(within(first).queryByRole("button", { name: "Make default" })).not.toBeInTheDocument();
    expect(within(second).getByText("unnamed")).toBeInTheDocument();
    expect(within(second).getByText("workflow")).toBeInTheDocument();
    expect(within(second).getByRole("button", { name: "Make default" })).toBeInTheDocument();
  });

  it("shows the empty principals state", async () => {
    mockApi({ "GET /api/v1/api_clients/apc_eval": { data: { ...client, principals: [], principals_count: 0 } } });
    renderRoute(<ApiClientDetailPage />, { path: "/console/api_clients/:id", url: "/console/api_clients/apc_eval", me: apiClientsMe() });
    expect(
      await screen.findByText("No principals attached. The client cannot create sessions until one is attached."),
    ).toBeInTheDocument();
  });

  it("rotates the secret after confirmation and shows it once", async () => {
    const { api } = renderDetail({ "POST /api/v1/api_clients/apc_eval/rotate_secret": { data: { ...client, secret: "rotated-secret" } } });
    await userEvent.click(await screen.findByRole("button", { name: "Rotate secret" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Rotate the secret?" });
    expect(within(dialog).getByText("The current secret and its access tokens stop working immediately.")).toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole("button", { name: "Rotate secret" }));

    expect(await screen.findByRole("textbox", { name: "Client secret" })).toHaveValue("rotated-secret");
    expect(toast.success).toHaveBeenCalledWith("Secret rotated. The previous secret no longer works.");
    expect(api.calls("POST /api/v1/api_clients/apc_eval/rotate_secret")).toHaveLength(1);
  });

  it("disables after confirmation and enables a disabled client", async () => {
    const { api } = renderDetail({ "POST /api/v1/api_clients/apc_eval/disable": { data: { ...client, disabled: true } } });
    await userEvent.click(await screen.findByRole("button", { name: "Disable" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Disable this client?" });
    await userEvent.click(within(dialog).getByRole("button", { name: "Disable" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("API client disabled."));
    expect(api.calls("POST /api/v1/api_clients/apc_eval/disable")).toHaveLength(1);
  });

  it("enables a disabled client", async () => {
    const api = mockApi({
      "GET /api/v1/api_clients/apc_eval": { data: { ...client, disabled: true } },
      "POST /api/v1/api_clients/apc_eval/enable": { data: client },
    });
    renderRoute(<ApiClientDetailPage />, { path: "/console/api_clients/:id", url: "/console/api_clients/apc_eval", me: apiClientsMe() });
    expect(await screen.findByText("disabled")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Enable" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("API client enabled."));
    expect(api.calls("POST /api/v1/api_clients/apc_eval/enable")).toHaveLength(1);
  });

  it("deletes after confirmation", async () => {
    const { router } = renderDetail({ "DELETE /api/v1/api_clients/apc_eval": undefined });
    await userEvent.click(await screen.findByRole("button", { name: "Delete" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Delete eval-runner?" });
    await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/console/api_clients"));
    expect(toast.success).toHaveBeenCalledWith("Deleted API client eval-runner.");
  });

  it("attaches, makes default, and detaches principals", async () => {
    const { api } = renderDetail({
      "POST /api/v1/api_clients/apc_eval/principals": { data: link("acp_3", false, { id: "prn_c", foreign_id: "C0NEW" }) },
      "POST /api/v1/api_clients/apc_eval/principals/acp_2/default": { data: link("acp_2", true, { id: "prn_b" }) },
      "DELETE /api/v1/api_clients/apc_eval/principals/acp_1": undefined,
    });

    await userEvent.type(await screen.findByLabelText("Principal OID or foreign ID"), " C0NEW ");
    await userEvent.click(screen.getByRole("button", { name: "Attach principal" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Attached C0NEW."));
    expect(api.calls("POST /api/v1/api_clients/apc_eval/principals")[0]?.body).toEqual({ data: { principal: "C0NEW" } });
    expect(screen.getByLabelText("Principal OID or foreign ID")).toHaveValue("");

    await userEvent.click(screen.getByRole("button", { name: "Make default" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("prn_b is now the default principal."));

    await userEvent.click(within(screen.getByRole("row", { name: /Evals/ })).getByRole("button", { name: "Detach" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Detach this principal?" });
    expect(within(dialog).getByText("Sessions already bound to it stop accepting turns.")).toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole("button", { name: "Detach" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Detached Evals."));
  });

  it("reports an unknown principal", async () => {
    renderDetail({
      "POST /api/v1/api_clients/apc_eval/principals": apiResponse(422, {
        error: { message: "No principal matches that OID or foreign ID.", details: { base: ["No principal matches that OID or foreign ID."] } },
      }),
    });
    await userEvent.type(await screen.findByLabelText("Principal OID or foreign ID"), "nope");
    await userEvent.click(screen.getByRole("button", { name: "Attach principal" }));
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith("No principal matches that OID or foreign ID."));
  });
});

const settings = {
  default_sandbox_repo_cache: "public",
  default_sandbox_observability_enabled: true,
  default_sandbox_sessions_read_enabled: false,
  default_sandbox_workflows_read_enabled: false,
  default_sandbox_workflows_write_enabled: false,
  default_role_ids: ["role_b"],
  roles: [
    { id: "role_a", foreign_id: "infra", name: "Infra", assign_by_default: false },
    { id: "role_b", foreign_id: null, name: null, assign_by_default: true },
  ],
  updated_at: "2026-06-01T10:00:00Z",
};

describe("system settings", () => {
  const teams = { data: { slack_external_team_ids: ["T01", "T02"] } };

  it("shows defaults, default roles, and the Slack Connect allowlist", async () => {
    mockApi({ "GET /api/v1/system_settings": { data: settings }, "GET /api/v1/system_settings/slack_external_teams": teams });
    renderRoute(<SystemSettingsPage />, { path: "/console/settings/edit" });

    expect(await screen.findByRole("heading", { name: "Settings" })).toBeInTheDocument();
    expect(screen.getByText("Deployment-wide defaults for new control-plane records.")).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Repo cache" })).toHaveTextContent("Public");
    expect(screen.getByLabelText("Observability")).toBeChecked();
    expect(screen.getByLabelText("Read sessions")).not.toBeChecked();
    expect(screen.getByLabelText(/Infra/)).not.toBeChecked();
    expect(screen.getByLabelText(/role_b/)).toBeChecked();
    expect(await screen.findByLabelText("Allowed external workspaces")).toHaveValue("T01\nT02");
    expect(screen.getByRole("link", { name: "Cancel" })).toHaveAttribute("href", "/console/principals");
  });

  it("saves defaults with the full default role list", async () => {
    const api = mockApi({
      "GET /api/v1/system_settings": { data: settings },
      "GET /api/v1/system_settings/slack_external_teams": teams,
      "PATCH /api/v1/system_settings": ({ body }: { body: { data: Record<string, unknown> } }) => ({ data: { ...settings, ...body.data } }),
    });
    renderRoute(<SystemSettingsPage />, { path: "/console/settings/edit" });

    await userEvent.click(await screen.findByLabelText("Read sessions"));
    await userEvent.click(screen.getByLabelText(/Infra/));
    await userEvent.click(screen.getByLabelText(/role_b/));
    await userEvent.click(screen.getByRole("button", { name: "Save settings" }));

    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("System settings updated."));
    expect(api.calls("PATCH /api/v1/system_settings")[0]?.body).toEqual({
      data: {
        default_sandbox_repo_cache: "public",
        default_sandbox_observability_enabled: true,
        default_sandbox_sessions_read_enabled: true,
        default_sandbox_workflows_read_enabled: false,
        default_sandbox_workflows_write_enabled: false,
        default_role_ids: ["role_a"],
      },
    });
  });

  it("shows validation errors", async () => {
    mockApi({
      "GET /api/v1/system_settings": { data: settings },
      "GET /api/v1/system_settings/slack_external_teams": teams,
      "PATCH /api/v1/system_settings": apiResponse(422, {
        error: { message: "validation failed", details: { default_sandbox_repo_cache: ["is not included in the list"] } },
      }),
    });
    renderRoute(<SystemSettingsPage />, { path: "/console/settings/edit" });
    await userEvent.click(await screen.findByRole("button", { name: "Save settings" }));
    expect(await screen.findByText("Settings could not be saved.")).toBeInTheDocument();
  });

  it("explains that a role is needed before choosing default roles", async () => {
    mockApi({
      "GET /api/v1/system_settings": { data: { ...settings, roles: [], default_role_ids: [] } },
      "GET /api/v1/system_settings/slack_external_teams": teams,
    });
    renderRoute(<SystemSettingsPage />, { path: "/console/settings/edit" });
    expect(await screen.findByText("Create a role before configuring default role assignments.")).toBeInTheDocument();
  });

  it("hides the Slack Connect form when the API is unavailable", async () => {
    mockApi({
      "GET /api/v1/system_settings": { data: settings },
      "GET /api/v1/system_settings/slack_external_teams": apiResponse(503, { error: { message: "unavailable" } }),
    });
    renderRoute(<SystemSettingsPage />, { path: "/console/settings/edit" });
    expect(
      await screen.findByText(
        "The allowed external workspaces could not be loaded from the Centaur API. Reload the page to try again.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Save Slack Connect workspaces" })).not.toBeInTheDocument();
  });

  it("replaces the Slack Connect workspaces and shows API errors", async () => {
    let fail = true;
    const api = mockApi({
      "GET /api/v1/system_settings": { data: settings },
      "GET /api/v1/system_settings/slack_external_teams": teams,
      "PUT /api/v1/system_settings/slack_external_teams": () =>
        fail
          ? apiResponse(422, { error: { message: "invalid Slack team id: bad" } })
          : { data: { slack_external_team_ids: ["T03", "T04"] } },
    });
    renderRoute(<SystemSettingsPage />, { path: "/console/settings/edit" });

    const textarea = await screen.findByLabelText("Allowed external workspaces");
    await userEvent.clear(textarea);
    await userEvent.type(textarea, "bad");
    await userEvent.click(screen.getByRole("button", { name: "Save Slack Connect workspaces" }));
    expect(await screen.findByText("invalid Slack team id: bad")).toBeInTheDocument();
    expect(textarea).toHaveValue("bad");

    fail = false;
    await userEvent.clear(textarea);
    await userEvent.type(textarea, "T03, T04");
    await userEvent.click(screen.getByRole("button", { name: "Save Slack Connect workspaces" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Slack Connect workspaces updated."));
    expect(api.calls("PUT /api/v1/system_settings/slack_external_teams")[1]?.body).toEqual({
      data: { slack_external_team_ids: "T03, T04" },
    });
    expect(textarea).toHaveValue("T03\nT04");
    expect(screen.queryByText("invalid Slack team id: bad")).not.toBeInTheDocument();
  });
});
