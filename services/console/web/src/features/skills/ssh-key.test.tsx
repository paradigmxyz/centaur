import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { apiResponse, mockApi, operatorMe, renderRoute } from "@/test/utils";

import SshKeyEditPage from "./pages/SshKeyEdit";
import SshKeyShowPage from "./pages/SshKeyShow";

const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }));
vi.mock("sonner", () => ({ toast }));

beforeEach(() => {
  toast.success.mockReset();
  toast.error.mockReset();
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

// The SSH key pages serve every active user; nothing is gated on admin nav.
const me = operatorMe();
const PRIVATE_KEY = "-----BEGIN OPENSSH PRIVATE KEY-----\nsynthetic-test-body\n-----END OPENSSH PRIVATE KEY-----";

const unsaved = {
  id: null,
  saved: false,
  name: "My SSH key",
  has_private_key: false,
  fingerprint: null,
  public_key: null,
  created_at: null,
  updated_at: null,
  grants: [],
};

const saved = {
  id: "ssk_1",
  saved: true,
  name: "Laptop key",
  has_private_key: true,
  fingerprint: "SHA256:abc123",
  public_key: "ssh-ed25519 AAAAC3Nza",
  created_at: "2026-06-01T10:00:00Z",
  updated_at: "2026-06-02T08:05:59Z",
  grants: [
    { id: "grt_1", grantee_type: "principal", grantee_id: "prn_1", label: "console-me", principal_kind: "console_user" },
    { id: "grt_2", grantee_type: "role", grantee_id: "role_1", label: "Infra", principal_kind: null },
  ],
};

describe("ssh key show", () => {
  it("shows the saved key, its grants, and the public key", async () => {
    mockApi({ "GET /api/v1/me/ssh_key": { data: saved } });
    renderRoute(<SshKeyShowPage />, { path: "/console/ssh_key", me });

    expect(await screen.findByRole("heading", { name: "Laptop key" })).toBeInTheDocument();
    expect(screen.getByText("Your personal SSH key")).toBeInTheDocument();
    expect(screen.getByText(/Saved 2026-06-02 08:05 UTC\./)).toBeInTheDocument();
    expect(screen.getByText("SHA256:abc123")).toBeInTheDocument();
    expect(screen.getByText(/Principal: console-me/)).toHaveTextContent("Principal: console-me (console_user)");
    expect(screen.getByText("Role: Infra")).toBeInTheDocument();
    expect(screen.getByLabelText("Public key")).toHaveValue("ssh-ed25519 AAAAC3Nza");
    expect(screen.getByLabelText("Public key")).toHaveAttribute("readonly");
    expect(screen.getByRole("link", { name: "Edit" })).toHaveAttribute("href", "/console/ssh_key/edit");
    expect(screen.getByRole("link", { name: "Back to integrations" })).toHaveAttribute("href", "/console/integrations");
  });

  it("explains a key without grants", async () => {
    mockApi({ "GET /api/v1/me/ssh_key": { data: { ...saved, grants: [] } } });
    renderRoute(<SshKeyShowPage />, { path: "/console/ssh_key", me });
    expect(await screen.findByText(/Not granted yet\./)).toBeInTheDocument();
  });

  it("sends a user without a key to the edit page", async () => {
    mockApi({ "GET /api/v1/me/ssh_key": { data: unsaved } });
    const { router } = renderRoute(<SshKeyShowPage />, {
      path: "/console/ssh_key",
      me,
      routes: [{ path: "/console/ssh_key/edit", element: <p>edit page</p> }],
    });
    expect(await screen.findByText("edit page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/ssh_key/edit");
  });

  it("deletes the key after confirmation", async () => {
    const user = userEvent.setup();
    const api = mockApi({ "GET /api/v1/me/ssh_key": { data: saved }, "DELETE /api/v1/me/ssh_key": undefined });
    renderRoute(<SshKeyShowPage />, {
      path: "/console/ssh_key",
      me,
      routes: [{ path: "/console/integrations", element: <p>integrations page</p> }],
    });

    await user.click(await screen.findByRole("button", { name: "Delete" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText("Delete this SSH key?")).toBeInTheDocument();
    expect(within(dialog).getByText("Access ends within seconds.")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Delete" }));

    expect(await screen.findByText("integrations page")).toBeInTheDocument();
    expect(api.calls("DELETE /api/v1/me/ssh_key")).toHaveLength(1);
    expect(toast.success).toHaveBeenCalledWith(
      "SSH key deleted. Remove its public key from your environments if it is no longer needed.",
    );
  });
});

describe("ssh key edit", () => {
  it("connects a new key and never prefills the private key", async () => {
    const user = userEvent.setup();
    const api = mockApi({
      "GET /api/v1/me/ssh_key": { data: unsaved },
      "POST /api/v1/me/ssh_key/validate": { data: { valid: true, message: "Valid private key. Ready to save.", fingerprint: "SHA256:abc123" } },
      "PUT /api/v1/me/ssh_key": { data: saved },
    });
    renderRoute(<SshKeyEditPage />, {
      path: "/console/ssh_key/edit",
      me,
      routes: [{ path: "/console/ssh_key", element: <p>key page</p> }],
    });

    expect(await screen.findByRole("heading", { name: "Connect an SSH key" })).toBeInTheDocument();
    expect(screen.getByLabelText("Name")).toHaveValue("My SSH key");
    const keyField = screen.getByLabelText("Your private SSH key");
    expect(keyField).toHaveValue("");
    expect(keyField).toHaveAttribute("placeholder", "Paste your dedicated private SSH key");
    expect(keyField).toHaveAttribute("autocomplete", "off");
    expect(screen.getByRole("status")).toHaveTextContent("Validation does not save it.");
    expect(screen.getByRole("link", { name: "Cancel" })).toHaveAttribute("href", "/console/integrations");

    await user.click(screen.getByRole("button", { name: "Save SSH key" }));
    expect(await screen.findByText("can't be blank")).toBeInTheDocument();
    expect(api.calls("PUT /api/v1/me/ssh_key")).toHaveLength(0);

    fireEvent.change(keyField, { target: { value: PRIVATE_KEY } });
    expect(await screen.findByText(/Not saved yet—click Save SSH key\./, undefined, { timeout: 2000 })).toHaveTextContent(
      "Valid private key. Ready to save. Fingerprint: SHA256:abc123. Not saved yet—click Save SSH key.",
    );
    expect(api.calls("POST /api/v1/me/ssh_key/validate")[0]?.body).toEqual({ data: { private_key: PRIVATE_KEY } });

    await user.click(screen.getByRole("button", { name: "Save SSH key" }));
    expect(await screen.findByText("key page")).toBeInTheDocument();
    expect(api.calls("PUT /api/v1/me/ssh_key")[0]?.body).toEqual({ data: { name: "My SSH key", private_key: PRIVATE_KEY } });
    expect(toast.success).toHaveBeenCalledWith("SSH key saved. Register the public key in your environments.");
  });

  it("keeps the saved key when the field is left blank", async () => {
    const user = userEvent.setup();
    const api = mockApi({ "GET /api/v1/me/ssh_key": { data: saved }, "PUT /api/v1/me/ssh_key": { data: saved } });
    renderRoute(<SshKeyEditPage />, {
      path: "/console/ssh_key/edit",
      me,
      routes: [{ path: "/console/ssh_key", element: <p>key page</p> }],
    });

    expect(await screen.findByLabelText("Your private SSH key")).toHaveAttribute(
      "placeholder",
      "Leave blank to keep your saved key",
    );
    await user.clear(screen.getByLabelText("Name"));
    await user.type(screen.getByLabelText("Name"), "Renamed");
    await user.click(screen.getByRole("button", { name: "Save SSH key" }));

    expect(await screen.findByText("key page")).toBeInTheDocument();
    expect(api.calls("PUT /api/v1/me/ssh_key")[0]?.body).toEqual({ data: { name: "Renamed" } });
    expect(api.calls("POST /api/v1/me/ssh_key/validate")).toHaveLength(0);
  });

  it("marks an invalid key and debounces checks", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const api = mockApi({
      "GET /api/v1/me/ssh_key": { data: saved },
      "POST /api/v1/me/ssh_key/validate": apiResponse(422, {
        error: { message: "That looks like a public key. Paste the private key.", details: { valid: false } },
      }),
    });
    renderRoute(<SshKeyEditPage />, { path: "/console/ssh_key/edit", me });

    const keyField = await screen.findByLabelText("Your private SSH key");
    fireEvent.change(keyField, { target: { value: "ssh-ed25519 AAAA" } });
    expect(screen.getByRole("status")).toHaveTextContent("Checking key…");
    fireEvent.change(keyField, { target: { value: "ssh-ed25519 AAAAB" } });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(600);
    });

    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent("That looks like a public key. Paste the private key."),
    );
    expect(keyField).toHaveAttribute("aria-invalid", "true");
    expect(api.calls("POST /api/v1/me/ssh_key/validate")).toHaveLength(1);

    fireEvent.change(keyField, { target: { value: "" } });
    expect(screen.getByRole("status")).toHaveTextContent("Leave blank to keep your saved key.");
    expect(keyField).not.toHaveAttribute("aria-invalid");
  });

  it("says when the key cannot be checked", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    mockApi({
      "GET /api/v1/me/ssh_key": { data: unsaved },
      "POST /api/v1/me/ssh_key/validate": apiResponse(500, { error: { message: "boom" } }),
    });
    renderRoute(<SshKeyEditPage />, { path: "/console/ssh_key/edit", me });

    fireEvent.change(await screen.findByLabelText("Your private SSH key"), { target: { value: PRIVATE_KEY } });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(600);
    });
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent("Could not check the key. Save to validate it, or try again."),
    );
  });

  it("lists server validation errors without echoing the key", async () => {
    const user = userEvent.setup();
    mockApi({
      "GET /api/v1/me/ssh_key": { data: saved },
      "PUT /api/v1/me/ssh_key": apiResponse(422, {
        error: { message: "validation failed", details: { name: ["can't be blank"], private_key: ["is not a valid private key"] } },
      }),
    });
    renderRoute(<SshKeyEditPage />, { path: "/console/ssh_key/edit", me });

    await user.clear(await screen.findByLabelText("Name"));
    await user.type(screen.getByLabelText("Name"), "x");
    await user.click(screen.getByRole("button", { name: "Save SSH key" }));

    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText("Name can't be blank")).toBeInTheDocument();
    expect(within(alert).getByText("Private key is not a valid private key")).toBeInTheDocument();
    expect(toast.success).not.toHaveBeenCalled();
  });
});
