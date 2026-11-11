import type { QueryClient } from "@tanstack/react-query";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { toast } from "sonner";
import { afterEach, describe, expect, it, vi } from "vitest";

import { apiResponse, mockApi, operatorMe, renderRoute } from "@/test/utils";

import type { BrokerCredential } from "./api";
import CredentialDetailPage from "./pages/CredentialDetail";
import CredentialEditPage from "./pages/CredentialEdit";
import CredentialNewPage from "./pages/CredentialNew";
import CredentialsListPage from "./pages/CredentialsList";
import { toPayload, type CredentialFormValues } from "./sections/CredentialForm";

vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

const meta = (total: number) => ({ page: 1, limit: 50, total, total_pages: Math.max(1, Math.ceil(total / 50)) });

const credential: BrokerCredential = {
  id: "bcr_gmail",
  foreign_id: "managed-gmail",
  name: "Managed Gmail",
  description: "Mailbox reader",
  labels: { team: "comms" },
  grant: "refresh_token",
  token_endpoint: "https://oauth2.googleapis.com/token",
  scopes: ["scope.a", "scope.b"],
  client_id: "cid-123",
  effective_client_id: "cid-123",
  token_endpoint_header_names: ["Authorization"],
  early_refresh_slack_seconds: 300,
  early_refresh_fraction: 0.2,
  max_refresh_interval_seconds: 86400,
  refresh_timeout_seconds: 30,
  status: "bootstrapping",
  expires_at: null,
  last_refresh: null,
  next_attempt_at: "2026-06-01T10:00:00Z",
  dead: false,
  dead_reason: null,
  failure_count: 0,
  has_client_secret: true,
  has_refresh_token: true,
  has_username: false,
  has_password: false,
  has_api_key: false,
  oauth_app_id: null,
  oauth_app: null,
  static_secret: { id: "ssr_gmail", foreign_id: "managed-gmail-secret", name: "Gmail token" },
  provider_subject: null,
  provider_email: null,
  external_user_key: null,
  created_at: "2026-06-01T09:00:00Z",
  updated_at: "2026-06-01T09:30:00Z",
};

const minted: BrokerCredential = {
  ...credential,
  id: "bcr_minted",
  foreign_id: null,
  name: null,
  client_id: null,
  effective_client_id: "app-client",
  token_endpoint_header_names: [],
  status: "dead",
  dead: true,
  dead_reason: "invalid_grant",
  oauth_app_id: "oap_google",
  oauth_app: { id: "oap_google", slug: "google-workspace", provider: "google" },
  static_secret: null,
  provider_subject: "sub-9",
  provider_email: "person@example.com",
  external_user_key: "user-9",
};

/** Everything TanStack still holds: mutation variables and every cached query. */
function cachedText(queryClient: QueryClient) {
  return JSON.stringify([
    queryClient.getMutationCache().getAll().map((mutation) => mutation.state),
    queryClient.getQueryCache().getAll().map((query) => [query.queryKey, query.state.data]),
  ]);
}

describe("credentials list", () => {
  it("lists credentials with combined id, provenance, status, and dead reason", async () => {
    mockApi({ "GET /api/v1/broker_credentials": { data: [credential, minted], meta: meta(2) } });
    renderRoute(<CredentialsListPage />, { path: "/console/credentials" });

    const link = await screen.findByRole("link", { name: "managed-gmail" });
    expect(link).toHaveAttribute("href", "/console/credentials/bcr_gmail");
    expect(link).toHaveAttribute("title", "managed-gmail");
    expect(screen.getByText("bcr_gmail")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "bcr_minted" })).toHaveAttribute("href", "/console/credentials/bcr_minted");
    expect(screen.getByText("unnamed")).toBeInTheDocument();
    expect(screen.getByText("(via Google OAuth)")).toBeInTheDocument();
    expect(screen.getByText("bootstrapping")).toBeInTheDocument();
    expect(screen.getByText("dead")).toBeInTheDocument();
    expect(screen.getByText("invalid_grant")).toBeInTheDocument();
    expect(screen.getByText("2 broker credentials. Token material is never shown.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "+ Add Credential" })).toHaveAttribute("href", "/console/credentials/new");
  });

  it("shows the empty state", async () => {
    mockApi({ "GET /api/v1/broker_credentials": { data: [], meta: meta(0) } });
    renderRoute(<CredentialsListPage />, { path: "/console/credentials" });
    expect(await screen.findByText("No managed credentials.")).toBeInTheDocument();
    expect(screen.getByText("0 broker credentials. Token material is never shown.")).toBeInTheDocument();
  });

  it("shows an error, not the empty state, when the list fails to load", async () => {
    mockApi({ "GET /api/v1/broker_credentials": apiResponse(500, { error: { message: "database unavailable" } }) });
    renderRoute(<CredentialsListPage />, { path: "/console/credentials" });
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not load credentials: database unavailable");
    expect(screen.queryByText("No managed credentials.")).not.toBeInTheDocument();
  });

  it("shows an error but keeps the loaded rows when a refetch fails", async () => {
    let fail = false;
    mockApi({
      "GET /api/v1/broker_credentials": () =>
        fail
          ? apiResponse(503, { error: { message: "upstream timeout" } })
          : { data: [credential], meta: meta(1) },
    });
    const { queryClient } = renderRoute(<CredentialsListPage />, { path: "/console/credentials" });
    expect(await screen.findByRole("link", { name: "managed-gmail" })).toBeInTheDocument();

    fail = true;
    await queryClient.refetchQueries({ queryKey: ["credentials"] });
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not load credentials: upstream timeout");
    expect(screen.getByRole("link", { name: "managed-gmail" })).toBeInTheDocument();
    expect(screen.queryByText("No managed credentials.")).not.toBeInTheDocument();
  });

  it("sends non-admins to their landing page without fetching", async () => {
    const api = mockApi({});
    renderRoute(<CredentialsListPage />, {
      path: "/console/credentials",
      me: operatorMe({ nav: { ...operatorMe().nav, integrations: true } }),
    });
    expect(await screen.findByText("other page")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/broker_credentials")).toHaveLength(0);
  });
});

describe("credential detail", () => {
  it("renders refresh, token client, policy, grantable-as, and metadata without token material", async () => {
    mockApi({ "GET /api/v1/broker_credentials/bcr_gmail": { data: credential } });
    renderRoute(<CredentialDetailPage />, { path: "/console/credentials/:id", url: "/console/credentials/bcr_gmail" });

    expect(await screen.findByRole("heading", { name: "Managed Gmail" })).toBeInTheDocument();
    expect(screen.getByText("bootstrapping")).toBeInTheDocument();
    expect(screen.getByText("team=comms")).toBeInTheDocument();
    expect(screen.getByText("Mailbox reader")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Edit" })).toHaveAttribute("href", "/console/credentials/bcr_gmail/edit");

    const refresh = screen.getByRole("region", { name: "Refresh" });
    expect(within(refresh).getByText("Next attempt")).toBeInTheDocument();
    expect(within(refresh).queryByText("Dead reason")).not.toBeInTheDocument();

    const client = screen.getByRole("region", { name: "Token Client" });
    expect(within(client).getByText("cid-123")).toBeInTheDocument();
    expect(within(client).getByText("https://oauth2.googleapis.com/token")).toBeInTheDocument();
    expect(within(client).getByText("Authorization: [redacted]")).toBeInTheDocument();
    expect(within(client).queryByText("Username")).not.toBeInTheDocument();

    const policy = screen.getByRole("region", { name: "Refresh Policy" });
    expect(within(policy).getByText("300s")).toBeInTheDocument();
    expect(within(policy).getByText("0.2")).toBeInTheDocument();

    const grantable = screen.getByRole("region", { name: "Grantable As" });
    expect(within(grantable).getByRole("link", { name: "Gmail token" })).toHaveAttribute(
      "href",
      "/console/secrets/static/ssr_gmail",
    );
    expect(screen.queryByRole("region", { name: "Provider Identity" })).not.toBeInTheDocument();
    expect(screen.getByText("Token material (access token, refresh token, client secret) is never shown.")).toBeInTheDocument();
  });

  it("shows the provider identity and inherited client id for a flow-minted credential", async () => {
    mockApi({ "GET /api/v1/broker_credentials/bcr_minted": { data: minted } });
    renderRoute(<CredentialDetailPage />, { path: "/console/credentials/:id", url: "/console/credentials/bcr_minted" });

    expect(await screen.findByRole("heading", { name: "bcr_minted" })).toBeInTheDocument();
    const identity = screen.getByRole("region", { name: "Provider Identity" });
    expect(within(identity).getByRole("link", { name: "google-workspace" })).toHaveAttribute(
      "href",
      "/console/oauth_apps/oap_google",
    );
    expect(within(identity).getByText("person@example.com")).toBeInTheDocument();
    expect(within(identity).getByText("sub-9")).toBeInTheDocument();
    expect(within(identity).getByText("user-9")).toBeInTheDocument();
    expect(screen.getByText(/inherited from the OAuth app/)).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Token Client" })).getByText("app-client")).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Refresh" })).getByText("invalid_grant")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Grantable As" })).not.toBeInTheDocument();
  });

  it("keeps the loaded record and shows an error when a refetch fails", async () => {
    let fail = false;
    mockApi({
      "GET /api/v1/broker_credentials/bcr_gmail": () =>
        fail ? apiResponse(503, { error: { message: "upstream timeout" } }) : { data: credential },
    });
    const { queryClient } = renderRoute(<CredentialDetailPage />, {
      path: "/console/credentials/:id",
      url: "/console/credentials/bcr_gmail",
    });
    expect(await screen.findByRole("heading", { name: "Managed Gmail" })).toBeInTheDocument();
    fail = true;
    await queryClient.refetchQueries({ queryKey: ["credentials"] });
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not refresh this credential: upstream timeout");
    expect(screen.getByRole("heading", { name: "Managed Gmail" })).toBeInTheDocument();
  });

  it("redacts stored password and preqin values", async () => {
    mockApi({
      "GET /api/v1/broker_credentials/bcr_gmail": {
        data: { ...credential, grant: "preqin", has_username: true, has_api_key: true },
      },
    });
    renderRoute(<CredentialDetailPage />, { path: "/console/credentials/:id", url: "/console/credentials/bcr_gmail" });
    const client = await screen.findByRole("region", { name: "Token Client" });
    expect(within(client).getByText("Username")).toBeInTheDocument();
    expect(within(client).getByText("API key")).toBeInTheDocument();
    expect(within(client).getAllByText("[redacted]")).toHaveLength(2);
  });

  it("deletes after confirmation and returns to the list", async () => {
    const api = mockApi({
      "GET /api/v1/broker_credentials/bcr_gmail": { data: credential },
      "DELETE /api/v1/broker_credentials/bcr_gmail": undefined,
    });
    const { router } = renderRoute(<CredentialDetailPage />, {
      path: "/console/credentials/:id",
      url: "/console/credentials/bcr_gmail",
      routes: [{ path: "/console/credentials", element: <p>list page</p> }],
    });
    await userEvent.click(await screen.findByRole("button", { name: "Delete" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Delete this credential?");
    expect(dialog).toHaveTextContent("This cannot be undone.");
    await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    expect(await screen.findByText("list page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/credentials");
    expect(api.calls("DELETE /api/v1/broker_credentials/bcr_gmail")).toHaveLength(1);
    expect(toast.success).toHaveBeenCalledWith("Credential deleted.");
  });

  it("reports a blocked delete and stays on the page", async () => {
    mockApi({
      "GET /api/v1/broker_credentials/bcr_gmail": { data: credential },
      "DELETE /api/v1/broker_credentials/bcr_gmail": apiResponse(409, {
        error: { message: "is referenced by one or more token_broker secret sources; remove those references first" },
      }),
    });
    renderRoute(<CredentialDetailPage />, { path: "/console/credentials/:id", url: "/console/credentials/bcr_gmail" });
    await userEvent.click(await screen.findByRole("button", { name: "Delete" }));
    await userEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith(expect.stringContaining("referenced by")));
    expect(screen.getByRole("heading", { name: "Managed Gmail" })).toBeInTheDocument();
  });
});

describe("new credential", () => {
  it("creates a credential with scopes, headers, labels, and a seed", async () => {
    const api = mockApi({ "POST /api/v1/broker_credentials": { data: { ...credential, id: "bcr_new" } } });
    const { router, queryClient, unmount } = renderRoute(<CredentialNewPage />, {
      path: "/console/credentials/new",
      routes: [{ path: "/console/credentials/:id", element: <p>detail page</p> }],
    });

    expect(screen.getByRole("heading", { name: "New credential" })).toBeInTheDocument();
    expect(screen.getByText("Broker")).toBeInTheDocument();
    expect(screen.getByLabelText("Foreign ID")).toBeEnabled();
    await userEvent.type(screen.getByLabelText("Foreign ID"), "new-managed");
    await userEvent.type(screen.getByLabelText("Name"), "New managed");
    await userEvent.type(screen.getByLabelText("Token endpoint *"), "https://idp.example/token");
    await userEvent.type(screen.getByLabelText("Client ID *"), "cid");
    await userEvent.type(screen.getByLabelText("Client secret"), "shhh");
    await userEvent.type(screen.getByLabelText("Scopes"), "scope.a\nscope.b\n");
    await userEvent.click(screen.getByRole("button", { name: "Add header" }));
    await userEvent.type(screen.getByLabelText("Header 1 name"), "Authorization");
    await userEvent.type(screen.getByLabelText("Header 1 value"), "Basic abc");
    await userEvent.type(screen.getByRole("textbox", { name: "Refresh Token" }), "seed-token");
    await userEvent.click(screen.getByRole("button", { name: "Add label" }));
    await userEvent.type(screen.getByLabelText("Labels 1 key"), "team");
    await userEvent.type(screen.getByLabelText("Labels 1 value"), "comms");
    await userEvent.click(screen.getByRole("button", { name: "Create credential" }));

    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/credentials/bcr_new");
    // The plaintext left with the request: nothing in the mutation or query cache.
    unmount();
    for (const plaintext of ["shhh", "seed-token", "Basic abc"]) expect(cachedText(queryClient)).not.toContain(plaintext);
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
    expect(toast.success).toHaveBeenCalledWith("Credential created.");
    expect(api.calls("POST /api/v1/broker_credentials")[0]?.body).toEqual({
      data: {
        foreign_id: "new-managed",
        name: "New managed",
        description: "",
        grant: "refresh_token",
        token_endpoint: "https://idp.example/token",
        client_id: "cid",
        client_secret: "shhh",
        scopes: ["scope.a", "scope.b"],
        token_endpoint_headers: { Authorization: "Basic abc" },
        early_refresh_fraction: "0.2",
        early_refresh_slack_seconds: "300",
        max_refresh_interval_seconds: "86400",
        refresh_timeout_seconds: "30",
        refresh_token: "seed-token",
        labels: { team: "comms" },
      },
    });
  });

  it("switches bootstrap panels by grant and sends preqin's username as username", async () => {
    const api = mockApi({ "POST /api/v1/broker_credentials": { data: { ...credential, id: "bcr_preqin" } } });
    renderRoute(<CredentialNewPage />, {
      path: "/console/credentials/new",
      routes: [{ path: "/console/credentials/:id", element: <p>detail page</p> }],
    });

    await userEvent.click(screen.getByRole("radio", { name: "Client Credentials" }));
    expect(screen.getByText(/No refresh token is required/)).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "Refresh Token" })).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("radio", { name: "Password" }));
    await userEvent.type(screen.getByLabelText("Username *"), "password-panel-user");

    await userEvent.click(screen.getByRole("radio", { name: "Preqin" }));
    expect(screen.getByLabelText("Client ID")).toBeInTheDocument();
    expect(screen.getByLabelText("Token endpoint *")).toHaveAttribute("placeholder", "https://api.preqin.com/connect/token");
    await userEvent.type(screen.getByLabelText("Username *"), "preqin-user");
    await userEvent.type(screen.getByLabelText("API key *"), "preqin-api-key");
    await userEvent.click(screen.getByRole("button", { name: "Create credential" }));

    await waitFor(() => expect(api.calls("POST /api/v1/broker_credentials")).toHaveLength(1));
    const body = api.calls("POST /api/v1/broker_credentials")[0]?.body as { data: Record<string, unknown> };
    expect(body.data).toMatchObject({ grant: "preqin", username: "preqin-user", api_key: "preqin-api-key" });
    expect(body.data).not.toHaveProperty("password");
    expect(body.data).not.toHaveProperty("refresh_token");
  });

  it("shows server validation errors on the field and in the summary", async () => {
    mockApi({
      "POST /api/v1/broker_credentials": apiResponse(422, {
        error: {
          message: "validation failed",
          details: { token_endpoint: ["can't be blank"], client_id: ["can't be blank"] },
        },
      }),
    });
    const { queryClient, unmount } = renderRoute(<CredentialNewPage />, { path: "/console/credentials/new" });
    await userEvent.type(screen.getByLabelText("Client secret"), "rejected-secret");
    await userEvent.click(screen.getByRole("button", { name: "Add header" }));
    await userEvent.type(screen.getByLabelText("Header 1 name"), "Authorization");
    await userEvent.type(screen.getByLabelText("Header 1 value"), "Basic rejected-header");
    await userEvent.type(screen.getByRole("textbox", { name: "Refresh Token" }), "rejected-seed");
    await userEvent.click(screen.getByRole("button", { name: "Create credential" }));

    expect(await screen.findByText("Credential could not be saved.")).toBeInTheDocument();
    expect(screen.getByText("Token endpoint can't be blank")).toBeInTheDocument();
    expect(screen.getAllByText("can't be blank").length).toBeGreaterThanOrEqual(2);
    // A failed submit still drops the plaintext from the inputs and the caches.
    expect(screen.getByLabelText("Client secret")).toHaveValue("");
    expect(screen.getByLabelText("Header 1 value")).toHaveValue("");
    expect(screen.getByRole("textbox", { name: "Refresh Token" })).toHaveValue("");
    const plaintexts = ["rejected-secret", "Basic rejected-header", "rejected-seed"];
    for (const plaintext of plaintexts) expect(cachedText(queryClient)).not.toContain(plaintext);
    unmount();
    for (const plaintext of plaintexts) expect(cachedText(queryClient)).not.toContain(plaintext);
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
  });
});

describe("edit credential", () => {
  it("never prefills write-only values and keeps stored ones when left blank", async () => {
    const api = mockApi({
      "GET /api/v1/broker_credentials/bcr_gmail": { data: credential },
      "PATCH /api/v1/broker_credentials/bcr_gmail": { data: credential },
    });
    const { router } = renderRoute(<CredentialEditPage />, {
      path: "/console/credentials/:id/edit",
      url: "/console/credentials/bcr_gmail/edit",
      routes: [{ path: "/console/credentials/:id", element: <p>detail page</p> }],
    });

    expect(await screen.findByRole("heading", { name: "Edit Managed Gmail" })).toBeInTheDocument();
    expect(screen.getByLabelText("Foreign ID")).toBeDisabled();
    expect(screen.getByText(/It cannot be changed once set\./)).toBeInTheDocument();
    expect(screen.getByLabelText("Client secret")).toHaveValue("");
    expect(screen.getByLabelText("Client secret")).toHaveAttribute("placeholder", "•••••••• (unchanged)");
    expect(screen.getByRole("textbox", { name: "Refresh Token" })).toHaveValue("");
    expect(screen.getByRole("textbox", { name: "Refresh Token" })).toHaveAttribute("placeholder", "•••••••• (unchanged)");
    expect(screen.getByLabelText("Header 1 name")).toHaveValue("Authorization");
    expect(screen.getByLabelText("Header 1 value")).toHaveValue("");
    expect(screen.getByLabelText("Header 1 value")).toHaveAttribute("placeholder", "•••••••• (unchanged)");
    expect(screen.getByLabelText("Scopes")).toHaveValue("scope.a\nscope.b");

    await userEvent.clear(screen.getByLabelText("Name"));
    await userEvent.type(screen.getByLabelText("Name"), "Renamed");
    await userEvent.click(screen.getByRole("button", { name: "Save changes" }));

    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/credentials/bcr_gmail");
    expect(toast.success).toHaveBeenCalledWith("Credential updated.");
    const body = api.calls("PATCH /api/v1/broker_credentials/bcr_gmail")[0]?.body as { data: Record<string, unknown> };
    expect(body.data).toMatchObject({ name: "Renamed", token_endpoint_headers: { Authorization: null } });
    for (const key of ["foreign_id", "client_secret", "refresh_token", "username", "password", "api_key"]) {
      expect(body.data).not.toHaveProperty(key);
    }
  });

  it("does not keep a rotated secret in the mutation or query cache", async () => {
    const api = mockApi({
      "GET /api/v1/broker_credentials/bcr_gmail": { data: credential },
      "PATCH /api/v1/broker_credentials/bcr_gmail": { data: credential },
    });
    const { queryClient, unmount } = renderRoute(<CredentialEditPage />, {
      path: "/console/credentials/:id/edit",
      url: "/console/credentials/bcr_gmail/edit",
      routes: [{ path: "/console/credentials/:id", element: <p>detail page</p> }],
    });
    await userEvent.type(await screen.findByLabelText("Client secret"), "rotated-secret");
    await userEvent.type(screen.getByLabelText("Header 1 value"), "Basic rotated-header");
    await userEvent.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByText("detail page")).toBeInTheDocument();
    expect(api.calls("PATCH /api/v1/broker_credentials/bcr_gmail")[0]?.body).toMatchObject({
      data: { client_secret: "rotated-secret", token_endpoint_headers: { Authorization: "Basic rotated-header" } },
    });
    unmount();
    for (const plaintext of ["rotated-secret", "Basic rotated-header"]) {
      expect(cachedText(queryClient)).not.toContain(plaintext);
    }
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
  });

  it("marks the refresh token and client secret unchanged only when one is stored", async () => {
    mockApi({
      "GET /api/v1/broker_credentials/bcr_gmail": {
        data: { ...credential, has_refresh_token: false, has_client_secret: false },
      },
    });
    renderRoute(<CredentialEditPage />, {
      path: "/console/credentials/:id/edit",
      url: "/console/credentials/bcr_gmail/edit",
    });
    const token = await screen.findByRole("textbox", { name: "Refresh Token" });
    expect(token).toHaveValue("");
    expect(token).toHaveAttribute("placeholder", "initial refresh token for the loop");
    expect(screen.getByLabelText("Client secret")).not.toHaveAttribute("placeholder", "•••••••• (unchanged)");
    expect(screen.queryByText(/Leave blank to keep the current secret/)).not.toBeInTheDocument();
  });

  it("requires a value for a renamed stored header", async () => {
    const api = mockApi({ "GET /api/v1/broker_credentials/bcr_gmail": { data: credential } });
    renderRoute(<CredentialEditPage />, {
      path: "/console/credentials/:id/edit",
      url: "/console/credentials/bcr_gmail/edit",
    });
    const name = await screen.findByLabelText("Header 1 name");
    await userEvent.clear(name);
    await userEvent.type(name, "X-Renamed");
    expect(screen.getByLabelText("Header 1 value")).toHaveAttribute("placeholder", "Basic …");
    await userEvent.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByText("Enter a value for the renamed header.")).toBeInTheDocument();
    expect(api.calls("PATCH /api/v1/broker_credentials/bcr_gmail")).toHaveLength(0);
  });
});

describe("toPayload", () => {
  const values = (overrides: Partial<CredentialFormValues>): CredentialFormValues => ({
    foreign_id: "",
    name: "",
    description: "",
    token_endpoint: "https://idp.example/token",
    client_id: "cid",
    client_secret: "",
    scopes: "",
    headers: [],
    early_refresh_fraction: "0.2",
    early_refresh_slack_seconds: "300",
    max_refresh_interval_seconds: "86400",
    refresh_timeout_seconds: "30",
    grant: "refresh_token",
    refresh_token: "",
    username: "",
    password: "",
    preqin_username: "",
    api_key: "",
    labels: [],
    ...overrides,
  });

  it("clears headers when every row is removed and sends a new value for a stored header", () => {
    expect(toPayload(values({}), credential).token_endpoint_headers).toBeNull();
    expect(
      toPayload(
        values({
          headers: [
            { key: "Authorization", value: "Basic new", stored: "Authorization" },
            { key: "", value: "dropped" },
          ],
        }),
        credential,
      ).token_endpoint_headers,
    ).toEqual({ Authorization: "Basic new" });
  });

  it("sends only the selected grant's write-only fields", () => {
    const payload = toPayload(
      values({ grant: "password", username: "u", password: "p", refresh_token: "ignored", preqin_username: "x", api_key: "k" }),
    );
    expect(payload).toMatchObject({ username: "u", password: "p" });
    expect(payload).not.toHaveProperty("refresh_token");
    expect(payload).not.toHaveProperty("api_key");
  });
});
