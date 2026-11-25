import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { mockApi, operatorMe, renderRoute } from "@/test/utils";

import type { Skill } from "./api";
import SkillDetailPage from "./pages/SkillDetail";
import SkillEditPage from "./pages/SkillEdit";

vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

const me = operatorMe({ nav: { ...operatorMe().nav, skills: true } });
const reader = { id: "usr_reader", name: "Rita", email: "reader@example.test" };
const editor = { id: "usr_editor", name: null, email: "editor@example.test" };

function skill(overrides: Partial<Skill> = {}): Skill {
  return {
    id: "skl_demo",
    name: "demo",
    description: "A demo skill",
    visibility: "private",
    author: "Owner",
    owner: { id: "usr_owner", name: "Owner", email: "owner@example.test" },
    updated_at: "2026-01-10T12:00:00Z",
    created_at: "2026-01-10T12:00:00Z",
    instructions: "# Docs",
    lock_version: 3,
    checksum: "abc",
    editors: [editor],
    readers: [reader],
    viewer: { owner: true, can_edit: true, can_manage: true, can_unshare: false },
    ...overrides,
  };
}

function renderEdit(current: Skill) {
  const api = mockApi({
    "GET /api/v1/console/skills/skl_demo": { data: current },
    "GET /api/v1/console/skills/editor_candidates": { data: [reader, editor] },
    "PATCH /api/v1/console/skills/skl_demo": { data: current },
  });
  renderRoute(<SkillEditPage />, {
    path: "/console/skills/:id/edit",
    url: "/console/skills/skl_demo/edit",
    me,
  });
  return api;
}

describe("skill access", () => {
  it("sends the owner's visibility, readers, and editors", async () => {
    const api = renderEdit(skill());
    const save = await screen.findByRole("button", { name: "Save skill" });
    expect(screen.getByText("Readers", { selector: "p" })).toBeTruthy();
    expect(screen.getByText("Visibility")).toBeTruthy();
    await userEvent.click(save);
    await waitFor(() => expect(api.calls("PATCH /api/v1/console/skills/skl_demo")).toHaveLength(1));
    expect(api.calls("PATCH /api/v1/console/skills/skl_demo")[0]?.body).toMatchObject({
      data: {
        visibility: "private",
        reader_ids: ["usr_reader"],
        editor_ids: ["usr_editor"],
        lock_version: 3,
      },
    });
  });

  it("keeps access fields out of an editor's save", async () => {
    const api = renderEdit(
      skill({ readers: undefined, viewer: { owner: false, can_edit: true, can_manage: false, can_unshare: false } }),
    );
    await userEvent.click(await screen.findByRole("button", { name: "Save skill" }));
    await waitFor(() => expect(api.calls("PATCH /api/v1/console/skills/skl_demo")).toHaveLength(1));
    const body = api.calls("PATCH /api/v1/console/skills/skl_demo")[0]?.body as { data: Record<string, unknown> };
    expect(body.data).not.toHaveProperty("visibility");
    expect(body.data).not.toHaveProperty("reader_ids");
    expect(body.data).not.toHaveProperty("editor_ids");
    expect(screen.queryByText("Visibility")).toBeNull();
  });

  it("lists readers on the owner's detail page only", async () => {
    mockApi({ "GET /api/v1/console/skills/skl_demo": { data: skill() } });
    const owner = renderRoute(<SkillDetailPage />, { path: "/console/skills/:id", url: "/console/skills/skl_demo", me });
    expect(await screen.findByRole("heading", { name: "Readers" })).toBeTruthy();
    expect(screen.getByText("Rita (reader@example.test)")).toBeTruthy();
    owner.unmount();

    mockApi({
      "GET /api/v1/console/skills/skl_demo": {
        data: skill({ readers: undefined, viewer: { owner: false, can_edit: false, can_manage: false, can_unshare: false } }),
      },
    });
    renderRoute(<SkillDetailPage />, { path: "/console/skills/:id", url: "/console/skills/skl_demo", me });
    expect(await screen.findByRole("heading", { name: "demo" })).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Readers" })).toBeNull();
    expect(screen.queryByRole("link", { name: "Edit" })).toBeNull();
  });
});
