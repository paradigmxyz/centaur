// Parity for the legacy descope ("view as operator") toggle: the account menu
// offers it only to acting admins on a session, the banner restores it.
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AccountMenu } from "@/app/shell/AccountMenu";
import { DescopeBanner } from "@/app/shell/DescopeBanner";
import { adminMe, mockApi, operatorMe, renderRoute } from "@/test/utils";

const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }));
vi.mock("sonner", () => ({ toast }));

afterEach(() => {
  vi.unstubAllGlobals();
});

async function openMenu(email: string) {
  await userEvent.click(screen.getByRole("button", { name: new RegExp(email.replace(/\./g, "\\.")) }));
}

describe("descope toggle", () => {
  it("offers View as operator to acting admins and lands them as an operator", async () => {
    const descoped = adminMe({ acting_admin: false, descoped: true, nav: { ...operatorMe().nav, integrations: true } });
    const api = mockApi({ "POST /api/v1/descope": { data: descoped }, "GET /api/v1/me": { data: descoped } });
    const me = adminMe();
    const { router } = renderRoute(<AccountMenu me={me} />, { path: "/console/users" });

    await openMenu(me.user.email);
    await userEvent.click(await screen.findByRole("menuitem", { name: /View as operator/ }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/console/integrations"));
    expect(api.calls("POST /api/v1/descope")).toHaveLength(1);
  });

  it.each([
    ["operators", operatorMe()],
    ["descoped admins", adminMe({ acting_admin: false, descoped: true })],
    ["bearer-token callers", adminMe({ auth_method: "api_key" })],
  ])("is not offered to %s", async (_label, me) => {
    mockApi({});
    renderRoute(<AccountMenu me={me} />);
    await openMenu(me.user.email);
    expect(await screen.findByRole("menuitem", { name: /Sign out/ })).toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: /View as operator/ })).not.toBeInTheDocument();
  });

  it("restores admin permissions from the banner", async () => {
    const api = mockApi({ "DELETE /api/v1/descope": { data: adminMe() }, "GET /api/v1/me": { data: adminMe() } });
    const { router } = renderRoute(<DescopeBanner />, { path: "/console/integrations" });

    expect(screen.getByText("Admin permissions paused — viewing the console as an operator")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Restore admin" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/console/principals"));
    expect(toast.success).toHaveBeenCalledWith("Admin permissions restored.");
    expect(api.calls("DELETE /api/v1/descope")).toHaveLength(1);
  });
});
