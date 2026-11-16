// Secret plaintext in the SPA: request bodies are plain async calls, so after
// any submit (accepted or rejected) the plaintext must be gone from the inputs,
// the page, and both TanStack caches; an issued API client secret is shown
// once in route-scoped state and never cached.
import type { QueryClient } from "@tanstack/react-query";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { act } from "react";
import { Route, Routes } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { adminMe, apiResponse, mockApi, operatorMe, renderRoute } from "@/test/utils";

import ApiClientDetailPage from "./admin/pages/ApiClientDetail";
import ApiClientNewPage from "./admin/pages/ApiClientNew";
import ApiClientsListPage from "./admin/pages/ApiClientsList";
import { IssuedSecretScope } from "./admin/secret-handoff";
import CredentialNewPage from "./credentials/pages/CredentialNew";
import OauthAppNewPage from "./oauth-apps/pages/OauthAppNew";
import SecretNewPage from "./secrets/pages/SecretNew";
import SshKeyEditPage from "./skills/pages/SshKeyEdit";

vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

/** Everything TanStack holds: every query's key and state, every mutation's state. */
function cachedText(queryClient: QueryClient) {
  return JSON.stringify([
    queryClient.getQueryCache().getAll().map((query) => [query.queryKey, query.state]),
    queryClient.getMutationCache().getAll().map((mutation) => mutation.state),
  ]);
}

const REJECTED = apiResponse(422, { error: { message: "validation failed", details: { base: ["Rejected by the test server"] } } });

interface SecretForm {
  name: string;
  write: string;
  reads?: Record<string, unknown>;
  accepted: unknown;
  plaintexts: string[];
  render: () => ReturnType<typeof renderRoute>;
  fill: () => Promise<void>;
  submit: string;
  done: string;
  inputs: () => HTMLElement[];
}

const PRIVATE_KEY = "-----BEGIN OPENSSH PRIVATE KEY-----\nssh-private-body\n-----END OPENSSH PRIVATE KEY-----";

const forms: SecretForm[] = [
  {
    name: "secret inline value",
    write: "POST /api/v1/console/pg_dsn_secrets",
    accepted: { data: { id: "pgd_new" } },
    plaintexts: ["postgres://u:inline-pw@db.example/shop"],
    render: () =>
      renderRoute(<SecretNewPage />, {
        path: "/console/secrets/:kind/new",
        url: "/console/secrets/pg_dsn/new",
        routes: [{ path: "/console/secrets/:kind/:id", element: <p>saved</p> }],
      }),
    fill: async () => {
      await userEvent.type(screen.getByLabelText("Foreign ID *"), "ui-shop");
      await userEvent.type(screen.getByLabelText("Database *"), "shop");
      await userEvent.click(screen.getByRole("combobox", { name: "Backend" }));
      await userEvent.click(await screen.findByRole("option", { name: "Inline Value" }));
      await userEvent.type(screen.getByLabelText("Inline value"), "postgres://u:inline-pw@db.example/shop");
    },
    submit: "Create secret",
    done: "saved",
    inputs: () => [screen.getByLabelText("Inline value")],
  },
  {
    name: "credential secrets and header values",
    write: "POST /api/v1/console/broker_credentials",
    accepted: { data: { id: "bcr_new" } },
    plaintexts: ["cred-client-secret", "Basic cred-header", "cred-refresh-token"],
    render: () =>
      renderRoute(<CredentialNewPage />, {
        path: "/console/credentials/new",
        routes: [{ path: "/console/credentials/:id", element: <p>saved</p> }],
      }),
    fill: async () => {
      await userEvent.type(screen.getByLabelText("Client secret"), "cred-client-secret");
      await userEvent.click(screen.getByRole("button", { name: "Add header" }));
      await userEvent.type(screen.getByLabelText("Header 1 name"), "Authorization");
      await userEvent.type(screen.getByLabelText("Header 1 value"), "Basic cred-header");
      await userEvent.type(screen.getByRole("textbox", { name: "Refresh Token" }), "cred-refresh-token");
    },
    submit: "Create credential",
    done: "saved",
    inputs: () => [
      screen.getByLabelText("Client secret"),
      screen.getByLabelText("Header 1 value"),
      screen.getByRole("textbox", { name: "Refresh Token" }),
    ],
  },
  {
    name: "OAuth app client_secret",
    write: "POST /api/v1/console/oauth_apps",
    reads: {
      "GET /api/v1/console/oauth_providers": {
        data: [{ key: "google", display_name: "Google", presettable: false, config_keys: [], optional_config_keys: [] }],
      },
    },
    accepted: { data: { id: "oap_new" } },
    plaintexts: ["oauth-client-secret"],
    render: () =>
      renderRoute(<OauthAppNewPage />, {
        path: "/console/oauth_apps/new",
        routes: [{ path: "/console/oauth_apps/:id", element: <p>saved</p> }],
      }),
    fill: async () => {
      await userEvent.type(await screen.findByLabelText("Slug *"), "new-google");
      await userEvent.type(screen.getByLabelText("Client secret *"), "oauth-client-secret");
      await userEvent.type(screen.getByLabelText("Allowed scopes *"), "openid");
    },
    submit: "Create app",
    done: "saved",
    inputs: () => [screen.getByLabelText("Client secret *")],
  },
  {
    name: "SSH private key",
    write: "PUT /api/v1/console/me/ssh_key",
    reads: {
      "GET /api/v1/console/me/ssh_key": {
        data: { id: null, saved: false, name: "My SSH key", has_private_key: false, fingerprint: null, public_key: null, created_at: null, updated_at: null, grants: [] },
      },
      "POST /api/v1/console/me/ssh_key/validate": { data: { valid: true, message: "Valid private key.", fingerprint: "SHA256:abc" } },
    },
    accepted: { data: { id: "ssk_1", saved: true, name: "My SSH key", has_private_key: true, fingerprint: "SHA256:abc", public_key: "ssh-ed25519 AAAA", created_at: null, updated_at: null, grants: [] } },
    plaintexts: ["ssh-private-body"],
    render: () =>
      renderRoute(<SshKeyEditPage />, {
        path: "/console/ssh_key/edit",
        me: operatorMe(),
        routes: [{ path: "/console/ssh_key", element: <p>saved</p> }],
      }),
    fill: async () => {
      fireEvent.change(await screen.findByLabelText("Your private SSH key"), { target: { value: PRIVATE_KEY } });
    },
    submit: "Save SSH key",
    done: "saved",
    inputs: () => [screen.getByLabelText("Your private SSH key")],
  },
];

describe.each(forms)("$name", (form) => {
  it("is sent, then gone from the page and the TanStack caches after a successful save", async () => {
    const api = mockApi({ ...form.reads, [form.write]: form.accepted });
    const { queryClient } = form.render();
    await form.fill();
    await userEvent.click(screen.getByRole("button", { name: form.submit }));

    expect(await screen.findByText(form.done)).toBeInTheDocument();
    const sent = JSON.stringify(api.calls(form.write)[0]?.body);
    for (const plaintext of form.plaintexts) {
      expect(sent).toContain(plaintext.replace(/\n/g, "\\n"));
      expect(document.body.innerHTML).not.toContain(plaintext);
      expect(cachedText(queryClient)).not.toContain(plaintext);
    }
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
  });

  it("is cleared from the inputs and absent from the TanStack caches after a 422", async () => {
    mockApi({ ...form.reads, [form.write]: REJECTED });
    const { queryClient } = form.render();
    await form.fill();
    await userEvent.click(screen.getByRole("button", { name: form.submit }));

    expect((await screen.findAllByText("Rejected by the test server")).length).toBeGreaterThan(0);
    await waitFor(() => form.inputs().forEach((input) => expect(input).toHaveValue("")));
    for (const plaintext of form.plaintexts) {
      expect(document.body.innerHTML).not.toContain(plaintext);
      expect(cachedText(queryClient)).not.toContain(plaintext);
    }
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
  });
});

describe("issued API client secret", () => {
  const client = {
    id: "apc_eval",
    name: "eval-runner",
    scopes: ["sessions:read"],
    disabled: false,
    disabled_at: null,
    principals_count: 0,
    principals: [],
    created_at: "2026-06-01T10:00:00Z",
    updated_at: "2026-06-01T10:00:00Z",
  };

  function renderApiClients(url: string) {
    mockApi({
      "GET /api/v1/console/api_clients": { data: [client], meta: { total: 1 } },
      "GET /api/v1/console/api_clients/apc_eval": { data: client },
      "POST /api/v1/console/api_clients": { data: { ...client, secret: "issued-on-create" } },
      "POST /api/v1/console/api_clients/apc_eval/rotate_secret": { data: { ...client, secret: "issued-on-rotate" } },
    });
    return renderRoute(
      <IssuedSecretScope>
        <Routes>
          <Route index element={<ApiClientsListPage />} />
          <Route path="new" element={<ApiClientNewPage />} />
          <Route path=":id" element={<ApiClientDetailPage />} />
        </Routes>
      </IssuedSecretScope>,
      {
        path: "/console/api_clients/*",
        url,
        me: adminMe({ features: { api_clients: true, password_login: true }, nav: { ...adminMe().nav, api_clients: true } }),
      },
    );
  }

  async function expectGoneAfterLeavingAndReturning(router: ReturnType<typeof renderApiClients>["router"], secret: string) {
    await act(() => router.navigate("/console/api_clients"));
    expect(await screen.findByRole("link", { name: "apc_eval" })).toBeInTheDocument();
    await act(() => router.navigate("/console/api_clients/apc_eval"));
    expect(await screen.findByRole("heading", { name: "eval-runner" })).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "Client secret" })).not.toBeInTheDocument();
    expect(document.body.innerHTML).not.toContain(secret);
  }

  it("is shown once after create and gone after navigating away and back", async () => {
    const { router, queryClient } = renderApiClients("/console/api_clients/new");
    await userEvent.type(screen.getByLabelText("Name *"), "eval-runner");
    await userEvent.click(screen.getByRole("button", { name: "Create API client" }));

    expect(await screen.findByRole("textbox", { name: "Client secret" })).toHaveValue("issued-on-create");
    expect(cachedText(queryClient)).not.toContain("issued-on-create");
    await expectGoneAfterLeavingAndReturning(router, "issued-on-create");
    expect(cachedText(queryClient)).not.toContain("issued-on-create");
  });

  it("is shown once after rotate and gone after navigating away and back", async () => {
    const { router, queryClient } = renderApiClients("/console/api_clients/apc_eval");
    await userEvent.click(await screen.findByRole("button", { name: "Rotate secret" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Rotate the secret?" });
    await userEvent.click(within(dialog).getByRole("button", { name: "Rotate secret" }));

    expect(await screen.findByRole("textbox", { name: "Client secret" })).toHaveValue("issued-on-rotate");
    expect(cachedText(queryClient)).not.toContain("issued-on-rotate");
    await expectGoneAfterLeavingAndReturning(router, "issued-on-rotate");
    expect(cachedText(queryClient)).not.toContain("issued-on-rotate");
  });
});
