import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { adminMe, apiResponse, mockApi, operatorMe, renderRoute } from "@/test/utils";

import MySkillsListPage from "./pages/MySkillsList";
import SkillDetailPage from "./pages/SkillDetail";
import SkillEditPage from "./pages/SkillEdit";
import SkillNewPage from "./pages/SkillNew";
import SkillsListPage from "./pages/SkillsList";

const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }));
vi.mock("sonner", () => ({ toast }));

beforeEach(() => {
  toast.success.mockReset();
  toast.error.mockReset();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

// Skills are open to every active user (nav.skills), not only admins.
const member = () => operatorMe({ nav: { ...operatorMe().nav, skills: true, integrations: true } });
const me = member();

const owner = { id: me.user.id, name: null, email: me.user.email };
const alice = { id: "usr_alice", name: "Alice", email: "alice@example.test" };
const bob = { id: "usr_bob", name: null, email: "bob@example.test" };

const summary = {
  id: "skl_triage",
  name: "incident-triage",
  description: "Triage production incidents.",
  visibility: "shared" as const,
  author: "Alice",
  owner: alice,
  updated_at: "2026-06-01T10:00:00Z",
  viewer: { owner: false },
};

function skill(overrides: Record<string, unknown> = {}) {
  return {
    ...summary,
    instructions: "# Incident Triage\n\nSteps.",
    content_html: "<h1>Incident Triage</h1><p>Steps.</p>",
    lock_version: 3,
    checksum: "abc",
    created_at: "2026-05-01T10:00:00Z",
    editors: [],
    viewer: { owner: false, can_edit: false, can_manage: false, can_unshare: false },
    ...overrides,
  };
}

const ownSkill = (overrides: Record<string, unknown> = {}) =>
  skill({
    owner,
    author: owner.email,
    visibility: "private",
    viewer: { owner: true, can_edit: true, can_manage: true, can_unshare: false },
    ...overrides,
  });

describe("skills list", () => {
  it("shows public skills with authors, You for the caller, and the tab strip", async () => {
    const api = mockApi({
      "GET /api/v1/skills": {
        data: [summary, { ...summary, id: "skl_mine", name: "my-skill", owner, author: owner.email, viewer: { owner: true } }],
      },
    });
    renderRoute(<SkillsListPage />, { path: "/console/skills", me });

    expect(await screen.findByRole("link", { name: "incident-triage" })).toHaveAttribute("href", "/console/skills/skl_triage");
    expect(screen.getByRole("heading", { name: "Public Skills" })).toBeInTheDocument();
    expect(screen.getByText("Browse skills available to every agent.")).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Author" })).toBeInTheDocument();
    expect(screen.getByText("Alice")).toBeInTheDocument();
    expect(screen.getByText("You")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "New Skill" })).toHaveAttribute("href", "/console/skills/new");
    expect(screen.getByPlaceholderText("Search public skills")).toBeInTheDocument();
    const tabs = screen.getByRole("navigation", { name: "Skill sections" });
    expect(within(tabs).getByRole("link", { name: "Public Skills" })).toHaveAttribute("aria-current", "page");
    expect(api.calls("GET /api/v1/skills")[0]?.url.searchParams.get("tab")).toBe("directory");
  });

  it("searches within the tab and carries the query to the other tab", async () => {
    const api = mockApi({ "GET /api/v1/skills": { data: [] } });
    renderRoute(<SkillsListPage />, { path: "/console/skills", url: "/console/skills?q=deploy", me });

    expect(await screen.findByText("No skills match “deploy”.")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/skills")[0]?.url.searchParams.get("q")).toBe("deploy");
    const tabs = screen.getByRole("navigation", { name: "Skill sections" });
    expect(within(tabs).getByRole("link", { name: "My Skills" })).toHaveAttribute("href", "/console/skills/mine?q=deploy");
  });

  it("has an empty state for each tab", async () => {
    mockApi({ "GET /api/v1/skills": { data: [] } });
    renderRoute(<SkillsListPage />, { path: "/console/skills", me });
    expect(await screen.findByText("No public skills yet.")).toBeInTheDocument();
  });

  it("shows my skills with visibility badges", async () => {
    const api = mockApi({
      "GET /api/v1/skills": {
        data: [
          { ...summary, owner, viewer: { owner: true }, visibility: "private" },
          { ...summary, id: "skl_pub", name: "public-one", owner, viewer: { owner: true } },
        ],
      },
    });
    renderRoute(<MySkillsListPage />, { path: "/console/skills/mine", me });

    expect(await screen.findByText("PRIVATE")).toBeInTheDocument();
    expect(screen.getByText("PUBLIC")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "My Skills" })).toBeInTheDocument();
    expect(screen.getByText("Create and manage your private and public skills.")).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Visibility" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Search my skills")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/skills")[0]?.url.searchParams.get("tab")).toBe("mine");
  });

  it("explains an empty my skills tab", async () => {
    mockApi({ "GET /api/v1/skills": { data: [] } });
    renderRoute(<MySkillsListPage />, { path: "/console/skills/mine", me });
    expect(await screen.findByText("You have not created any skills yet.")).toBeInTheDocument();
  });

  it("reports a failed load instead of an empty state", async () => {
    mockApi({ "GET /api/v1/skills": apiResponse(500, { error: { message: "database unavailable" } }) });
    renderRoute(<SkillsListPage />, { path: "/console/skills", me });

    expect(await screen.findByRole("alert")).toHaveTextContent("Could not load skills: database unavailable");
    expect(screen.queryByText("No public skills yet.")).not.toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("reports a failed search instead of no matches", async () => {
    mockApi({
      "GET /api/v1/skills": ({ url }: { url: URL }) =>
        url.searchParams.get("q") ? apiResponse(500, { error: { message: "search failed" } }) : { data: [summary] },
    });
    renderRoute(<MySkillsListPage />, { path: "/console/skills/mine", url: "/console/skills/mine?q=deploy", me });

    expect(await screen.findByRole("alert")).toHaveTextContent("Could not load skills: search failed");
    expect(screen.queryByText("No skills match “deploy”.")).not.toBeInTheDocument();
    expect(screen.queryByText("You have not created any skills yet.")).not.toBeInTheDocument();
  });

  it("is hidden from users without skills access", async () => {
    mockApi({ "GET /api/v1/skills": { data: [] } });
    renderRoute(<SkillsListPage />, { path: "/console/skills", me: operatorMe() });
    await waitFor(() => expect(screen.queryByRole("heading", { name: "Public Skills" })).not.toBeInTheDocument());
  });
});

describe("skill detail", () => {
  it("renders a reader's view: markdown, editors, back to the directory, no actions", async () => {
    mockApi({ "GET /api/v1/skills/skl_triage": { data: skill({ editors: [alice, bob] }) } });
    renderRoute(<SkillDetailPage />, { path: "/console/skills/:id", url: "/console/skills/skl_triage", me });

    expect(await screen.findByRole("heading", { level: 1, name: "incident-triage" })).toBeInTheDocument();
    expect(screen.getByText("PUBLIC")).toBeInTheDocument();
    expect(screen.getByText(/by Alice · updated/)).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Description" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Instructions" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 1, name: "Incident Triage" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Editors" })).toBeInTheDocument();
    expect(screen.getByText("Alice (alice@example.test)")).toBeInTheDocument();
    expect(screen.getByText("bob@example.test")).toBeInTheDocument();
    const breadcrumb = screen.getByRole("navigation", { name: "Breadcrumb" });
    expect(within(breadcrumb).getByRole("link", { name: /Back to Skills/ })).toHaveAttribute("href", "/console/skills");
    expect(screen.queryByRole("link", { name: "Edit" })).not.toBeInTheDocument();
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Archive" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Make Private" })).not.toBeInTheDocument();
  });

  it("lets an editor edit but not manage the skill", async () => {
    mockApi({
      "GET /api/v1/skills/skl_triage": {
        data: skill({ visibility: "private", viewer: { owner: false, can_edit: true, can_manage: false, can_unshare: false } }),
      },
    });
    renderRoute(<SkillDetailPage />, { path: "/console/skills/:id", url: "/console/skills/skl_triage", me });

    expect(await screen.findByRole("link", { name: "Edit" })).toHaveAttribute("href", "/console/skills/skl_triage/edit");
    const breadcrumb = screen.getByRole("navigation", { name: "Breadcrumb" });
    expect(within(breadcrumb).getByRole("link", { name: /Back to Skills/ })).toHaveAttribute("href", "/console/skills/mine");
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Archive" })).not.toBeInTheDocument();
    expect(screen.queryByText("Editors")).not.toBeInTheDocument();
  });

  it("lets the owner share, unshare, and archive", async () => {
    const user = userEvent.setup();
    const api = mockApi({
      "GET /api/v1/skills/skl_triage": { data: ownSkill() },
      "POST /api/v1/skills/skl_triage/share": { data: ownSkill({ visibility: "shared", viewer: { owner: true, can_edit: true, can_manage: true, can_unshare: true } }) },
      "POST /api/v1/skills/skl_triage/unshare": { data: ownSkill() },
      "DELETE /api/v1/skills/skl_triage": undefined,
    });
    const { router } = renderRoute(<SkillDetailPage />, {
      path: "/console/skills/:id",
      url: "/console/skills/skl_triage",
      me,
      routes: [{ path: "/console/skills/mine", element: <p>my skills page</p> }],
    });

    const toggle = await screen.findByRole("switch");
    expect(toggle).toHaveAttribute("aria-checked", "false");
    expect(within(toggle).getByText("Private")).toBeInTheDocument();
    expect(within(toggle).getByText("Public")).toBeInTheDocument();

    await user.click(toggle);
    await waitFor(() => expect(screen.getByRole("switch")).toHaveAttribute("aria-checked", "true"));
    expect(toast.success).toHaveBeenCalledWith("Skill is now shared.");
    expect(screen.getByText("PUBLIC")).toBeInTheDocument();

    await user.click(screen.getByRole("switch"));
    await waitFor(() => expect(screen.getByRole("switch")).toHaveAttribute("aria-checked", "false"));
    expect(toast.success).toHaveBeenCalledWith("Skill is now private.");

    await user.click(screen.getByRole("button", { name: "Archive" }));
    expect(await screen.findByText("my skills page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/skills/mine");
    expect(toast.success).toHaveBeenCalledWith("Skill archived.");
    expect(api.calls("DELETE /api/v1/skills/skl_triage")).toHaveLength(1);
  });

  it("reports why a skill could not be shared", async () => {
    const user = userEvent.setup();
    mockApi({
      "GET /api/v1/skills/skl_triage": { data: ownSkill() },
      "POST /api/v1/skills/skl_triage/share": apiResponse(422, {
        error: { message: "validation failed", details: { name: ["is already used by another skill"], description: ["can't be blank"] } },
      }),
    });
    renderRoute(<SkillDetailPage />, { path: "/console/skills/:id", url: "/console/skills/skl_triage", me });

    await user.click(await screen.findByRole("switch"));
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith("Name is already used by another skill and Description can't be blank"),
    );
  });

  it("lets an acting admin make someone else's public skill private", async () => {
    const user = userEvent.setup();
    const api = mockApi({
      "GET /api/v1/skills/skl_triage": {
        data: skill({ viewer: { owner: false, can_edit: false, can_manage: false, can_unshare: true } }),
      },
      "POST /api/v1/skills/skl_triage/unshare": { data: skill({ visibility: "private" }) },
    });
    renderRoute(<SkillDetailPage />, { path: "/console/skills/:id", url: "/console/skills/skl_triage", me: adminMe() });

    await user.click(await screen.findByRole("button", { name: "Make Private" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Skill is now private."));
    expect(api.calls("POST /api/v1/skills/skl_triage/unshare")).toHaveLength(1);
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Archive" })).not.toBeInTheDocument();
  });
});

describe("skill form", () => {
  it("creates a skill with editors picked from active users", async () => {
    const user = userEvent.setup();
    const api = mockApi({
      "GET /api/v1/skills/editor_candidates": { data: [alice, bob] },
      "POST /api/v1/skills": { data: ownSkill({ id: "skl_new", name: "release-helper" }) },
    });
    renderRoute(<SkillNewPage />, {
      path: "/console/skills/new",
      me,
      routes: [{ path: "/console/skills/:id", element: <p>skill page</p> }],
    });

    expect(await screen.findByRole("heading", { name: "New Skill" })).toBeInTheDocument();
    expect(screen.getByText("Create a private skill. You can share it after it validates.")).toBeInTheDocument();
    expect(screen.getByLabelText("Instructions")).toHaveValue("# Instructions");
    expect(screen.getByText("No additional editors.")).toBeInTheDocument();
    expect(
      screen.getByText(/Their names and emails are visible to anyone who can view the skill./),
    ).toBeInTheDocument();

    await user.type(screen.getByLabelText("Name"), "release-helper");
    await user.type(screen.getByLabelText("Description"), "Help with releases.");
    await user.clear(screen.getByLabelText("Instructions"));
    await user.type(screen.getByLabelText("Instructions"), "# Release");
    await user.click(screen.getByRole("combobox", { name: "Editors" }));
    await user.click(await screen.findByRole("option", { name: "Alice (alice@example.test)" }));
    expect(screen.getByText("Alice (alice@example.test)")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Create skill" }));

    expect(await screen.findByText("skill page")).toBeInTheDocument();
    expect(api.calls("POST /api/v1/skills")[0]?.body).toEqual({
      data: { name: "release-helper", description: "Help with releases.", instructions: "# Release", editor_ids: ["usr_alice"] },
    });
    expect(toast.success).toHaveBeenCalledWith("Skill created.");
  });

  it("reports when the editor candidates cannot be loaded", async () => {
    mockApi({ "GET /api/v1/skills/editor_candidates": apiResponse(500, { error: { message: "users unavailable" } }) });
    renderRoute(<SkillNewPage />, { path: "/console/skills/new", me });

    expect(await screen.findByRole("alert")).toHaveTextContent("Could not load console users: users unavailable");
    expect(screen.getByRole("button", { name: "Create skill" })).toBeInTheDocument();
  });

  it("keeps the fields and shows every server error when saving fails", async () => {
    const user = userEvent.setup();
    mockApi({
      "GET /api/v1/skills/editor_candidates": { data: [bob] },
      "POST /api/v1/skills": apiResponse(422, {
        error: {
          message: "validation failed",
          details: { name: ["is already used by another skill"], editors: ["include an unavailable user"] },
        },
      }),
    });
    renderRoute(<SkillNewPage />, { path: "/console/skills/new", me });

    await user.type(await screen.findByLabelText("Name"), "incident-triage");
    await user.type(screen.getByLabelText("Description"), "Dupe.");
    await user.click(screen.getByRole("button", { name: "Create skill" }));

    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText("Could not save this skill:")).toBeInTheDocument();
    expect(within(alert).getByText("Name is already used by another skill")).toBeInTheDocument();
    expect(within(alert).getByText("Editors include an unavailable user")).toBeInTheDocument();
    expect(screen.getByLabelText("Name")).toHaveValue("incident-triage");
    expect(screen.getByLabelText("Description")).toHaveValue("Dupe.");
    expect(toast.success).not.toHaveBeenCalled();
  });

  it("requires a name, description, and instructions before submitting", async () => {
    const user = userEvent.setup();
    const api = mockApi({ "GET /api/v1/skills/editor_candidates": { data: [] } });
    renderRoute(<SkillNewPage />, { path: "/console/skills/new", me });

    await user.clear(await screen.findByLabelText("Instructions"));
    await user.click(screen.getByRole("button", { name: "Create skill" }));
    expect(await screen.findAllByText("can't be blank")).toHaveLength(3);
    expect(api.calls("POST /api/v1/skills")).toHaveLength(0);
  });

  it("lets the owner replace editors on edit and sends the lock version", async () => {
    const user = userEvent.setup();
    const api = mockApi({
      "GET /api/v1/skills/skl_triage": { data: ownSkill({ visibility: "shared", editors: [alice] }) },
      "GET /api/v1/skills/editor_candidates": { data: [alice, bob] },
      "PATCH /api/v1/skills/skl_triage": { data: ownSkill({ lock_version: 4 }) },
    });
    renderRoute(<SkillEditPage />, {
      path: "/console/skills/:id/edit",
      url: "/console/skills/skl_triage/edit",
      me,
      routes: [{ path: "/console/skills/:id", element: <p>skill page</p> }],
    });

    expect(await screen.findByRole("heading", { name: "Edit incident-triage" })).toBeInTheDocument();
    expect(screen.getByText("This skill is shared. Saving changes makes them live immediately.")).toBeInTheDocument();
    expect(screen.getByLabelText("Name")).toHaveValue("incident-triage");
    await user.click(screen.getByRole("button", { name: "Remove" }));
    expect(screen.getByText("No additional editors.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save skill" }));

    expect(await screen.findByText("skill page")).toBeInTheDocument();
    expect(api.calls("PATCH /api/v1/skills/skl_triage")[0]?.body).toEqual({
      data: {
        name: "incident-triage",
        description: "Triage production incidents.",
        instructions: "# Incident Triage\n\nSteps.",
        lock_version: 3,
        editor_ids: [],
      },
    });
    expect(toast.success).toHaveBeenCalledWith("Skill saved.");
  });

  it("shows editors read-only to a non-owner editor and never sends editor_ids", async () => {
    const user = userEvent.setup();
    const api = mockApi({
      "GET /api/v1/skills/skl_triage": {
        data: skill({ visibility: "private", editors: [owner], viewer: { owner: false, can_edit: true, can_manage: false, can_unshare: false } }),
      },
      "PATCH /api/v1/skills/skl_triage": { data: skill() },
    });
    renderRoute(<SkillEditPage />, {
      path: "/console/skills/:id/edit",
      url: "/console/skills/skl_triage/edit",
      me,
      routes: [{ path: "/console/skills/:id", element: <p>skill page</p> }],
    });

    expect(await screen.findByText("Only the skill owner can manage editors.")).toBeInTheDocument();
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
    expect(screen.queryByText(/This skill is shared/)).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save skill" }));
    await screen.findByText("skill page");
    expect(api.calls("PATCH /api/v1/skills/skl_triage")[0]?.body).not.toHaveProperty("data.editor_ids");
    expect(api.calls("GET /api/v1/skills/editor_candidates")).toHaveLength(0);
  });

  it("reloads the latest content when the skill changed since it was opened", async () => {
    const user = userEvent.setup();
    let version = 3;
    const api = mockApi({
      "GET /api/v1/skills/skl_triage": () => ({ data: ownSkill({ lock_version: version, description: `Version ${version}` }) }),
      "GET /api/v1/skills/editor_candidates": { data: [] },
      "PATCH /api/v1/skills/skl_triage": () => {
        version = 4;
        return apiResponse(409, {
          error: {
            message: "This skill changed since you opened it. Review the latest content before saving.",
            details: { reason: "stale" },
          },
        });
      },
    });
    renderRoute(<SkillEditPage />, { path: "/console/skills/:id/edit", url: "/console/skills/skl_triage/edit", me });

    await user.type(await screen.findByLabelText("Description"), " mine");
    await user.click(screen.getByRole("button", { name: "Save skill" }));

    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith(
        "This skill changed since you opened it. Review the latest content before saving.",
      ),
    );
    await waitFor(() => expect(screen.getByLabelText("Description")).toHaveValue("Version 4"));
    expect(api.calls("PATCH /api/v1/skills/skl_triage")).toHaveLength(1);
    expect(toast.success).not.toHaveBeenCalled();
  });

  it("is not found for readers who cannot edit", async () => {
    mockApi({ "GET /api/v1/skills/skl_triage": { data: skill() } });
    renderRoute(<SkillEditPage />, { path: "/console/skills/:id/edit", url: "/console/skills/skl_triage/edit", me });
    expect(await screen.findByRole("heading", { name: "Not found" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Save skill" })).not.toBeInTheDocument();
  });
});
