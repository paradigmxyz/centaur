import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { adminMe, apiResponse, mockApi, operatorMe, renderRoute } from "@/test/utils";

import ScheduledTaskEditPage from "./pages/ScheduledTaskEdit";
import ScheduledTaskNewPage from "./pages/ScheduledTaskNew";
import ScheduledTasksListPage from "./pages/ScheduledTasksList";
import WorkflowDetailPage from "./pages/WorkflowDetail";
import WorkflowsListPage from "./pages/WorkflowsList";

const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }));
vi.mock("sonner", () => ({ toast }));

afterEach(() => {
  vi.unstubAllGlobals();
  toast.success.mockReset();
  toast.error.mockReset();
});

const member = operatorMe({
  user: { id: "usr_member", email: "member@example.test", name: null, status: "active", admin: false },
  nav: { ...operatorMe().nav, integrations: true, skills: true, scheduled_tasks: true },
});

const task = {
  id: "tsk_1",
  name: "Weekday summary",
  prompt: "Summarize open incidents.",
  author_id: "usr_member",
  author_email: "member@example.test",
  principal_id: null,
  delivery_channel: "C0123456789",
  delivery_mode: "channel",
  delivery_name: "#general",
  direct_message_user_id: "U0123456789",
  cron_expression: "0 9 * * 1-5",
  timezone: "America/Los_Angeles",
  schedule_preset: "weekdays",
  schedule_label: "Weekdays at 9:00 PT",
  custom_days: ["1", "2", "3", "4", "5"],
  custom_time: "09:00",
  enabled: true,
  next_run_at: null,
  last_enqueued_at: null,
  last_run_id: null,
  last_run_at: null,
  last_error: null,
  created_at: "2026-06-01T10:00:00Z",
  updated_at: "2026-06-01T10:00:00Z",
};

const newTask = {
  ...task,
  id: null,
  name: null,
  prompt: null,
  delivery_channel: null,
  delivery_mode: "dm",
  delivery_name: null,
  cron_expression: "0 9 * * *",
  schedule_preset: "daily",
  schedule_label: "Daily at 9:00 PT",
};

describe("scheduled tasks list", () => {
  it("lists tasks with schedule, delivery, and status lines", async () => {
    mockApi({
      "GET /api/v1/scheduled_tasks": {
        data: [
          task,
          {
            ...task,
            id: "tsk_2",
            name: "Raw cron",
            cron_expression: "*/7 * * * *",
            schedule_label: "*/7 * * * *",
            enabled: false,
            delivery_channel: "U0123456789",
            delivery_name: "Direct message to you",
            last_error: "boom",
          },
          { ...task, id: "tsk_3", name: "Unknown channel", delivery_channel: "C9999999999", delivery_name: null },
        ],
      },
    });
    renderRoute(<ScheduledTasksListPage />, { path: "/console/scheduled_tasks", me: member });

    expect(await screen.findByRole("link", { name: "Weekday summary" })).toHaveAttribute(
      "href",
      "/console/scheduled_tasks/tsk_1/edit",
    );
    expect(screen.getByRole("heading", { name: "Scheduled Tasks" })).toBeInTheDocument();
    expect(screen.getByText("Schedule single-turn prompts and deliver their results to Slack.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "New Task" })).toHaveAttribute("href", "/console/scheduled_tasks/new");
    expect(screen.getAllByText("Weekdays at 9:00 PT")[0]).not.toHaveClass("font-mono");
    expect(screen.getAllByText("*/7 * * * *")[0]).toHaveClass("font-mono");
    expect(screen.getByText("Disabled")).toBeInTheDocument();
    expect(screen.getByText("#general")).toBeInTheDocument();
    expect(screen.getByText("Direct message to you")).toBeInTheDocument();
    expect(screen.getByText("U0123456789")).toBeInTheDocument();
    expect(screen.getByText("C9999999999")).toBeInTheDocument();
    expect(screen.getByText("Last dispatch failed")).toHaveAttribute("title", "boom");

    const [, first, second] = screen.getAllByRole("row");
    expect(within(second as HTMLElement).getByRole("button", { name: "Run now" })).toBeDisabled();
    expect(within(first as HTMLElement).getByRole("button", { name: "Run now" })).toBeEnabled();
    expect(document.title).toBe("Scheduled · Centaur Console");
  });

  it("shows the empty state", async () => {
    mockApi({ "GET /api/v1/scheduled_tasks": { data: [] } });
    renderRoute(<ScheduledTasksListPage />, { path: "/console/scheduled_tasks", me: member });
    expect(await screen.findByText("No scheduled tasks yet.")).toBeInTheDocument();
  });

  it("queues a run and reports a disabled task", async () => {
    let runs = 0;
    const api = mockApi({
      "GET /api/v1/scheduled_tasks": { data: [task] },
      "POST /api/v1/scheduled_tasks/tsk_1/run": () =>
        ++runs === 1
          ? { data: { ...task, queued: true } }
          : apiResponse(422, { error: { message: "Enable the task before running it.", details: { reason: "task_disabled" } } }),
    });
    renderRoute(<ScheduledTasksListPage />, { path: "/console/scheduled_tasks", me: member });

    await userEvent.click(await screen.findByRole("button", { name: "Run now" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Task queued."));
    expect(api.calls("POST /api/v1/scheduled_tasks/tsk_1/run")[0]?.headers["X-CSRF-Token"]).toBe("csrf-test-token");

    await userEvent.click(screen.getByRole("button", { name: "Run now" }));
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith("Enable the task before running it."));
  });

  it("deletes a task after confirmation", async () => {
    const api = mockApi({
      "GET /api/v1/scheduled_tasks": { data: [task] },
      "DELETE /api/v1/scheduled_tasks/tsk_1": undefined,
    });
    renderRoute(<ScheduledTasksListPage />, { path: "/console/scheduled_tasks", me: member });

    await userEvent.click(await screen.findByRole("button", { name: "Delete" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Delete Weekday summary?");
    await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Task deleted."));
    expect(api.calls("DELETE /api/v1/scheduled_tasks/tsk_1")).toHaveLength(1);
  });
});

describe("new scheduled task", () => {
  it("renders the single-turn form with DM delivery by default", async () => {
    mockApi({ "GET /api/v1/scheduled_tasks/new": { data: newTask } });
    renderRoute(<ScheduledTaskNewPage />, { path: "/console/scheduled_tasks/new", me: member });

    expect(await screen.findByRole("heading", { name: "New Scheduled Task" })).toBeInTheDocument();
    expect(screen.getByLabelText("Name")).toHaveAttribute("placeholder", "Daily support summary");
    expect(screen.getByLabelText("Agent prompt")).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Deliver to my DM" })).toBeChecked();
    expect(screen.getByRole("radio", { name: "Deliver to a channel" })).not.toBeChecked();
    expect(screen.queryByRole("combobox", { name: "Delivery channel" })).not.toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Schedule" })).toHaveTextContent("Every day");
    expect(screen.getByText("Choose Manual only for an ad-hoc task, or run a preset at 9:00 AM Pacific Time.")).toBeInTheDocument();
    expect(screen.queryByText("Custom schedule")).not.toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Task enabled" })).toBeChecked();
    expect(screen.getByRole("button", { name: "Create task" })).toBeEnabled();
    expect(screen.getByRole("link", { name: "Cancel" })).toHaveAttribute("href", "/console/scheduled_tasks");
  });

  it("offers every schedule preset", async () => {
    mockApi({ "GET /api/v1/scheduled_tasks/new": { data: newTask } });
    renderRoute(<ScheduledTaskNewPage />, { path: "/console/scheduled_tasks/new", me: member });

    await userEvent.click(await screen.findByRole("combobox", { name: "Schedule" }));
    const options = await screen.findAllByRole("option");
    expect(options.map((option) => option.textContent)).toEqual([
      "Manual only",
      "Every day",
      "Every weekday",
      "Mondays",
      "Fridays",
      "Custom",
    ]);
  });

  it("disables DM delivery without a linked Slack identity", async () => {
    mockApi({
      "GET /api/v1/scheduled_tasks/new": { data: { ...newTask, delivery_mode: "channel", direct_message_user_id: null } },
    });
    renderRoute(<ScheduledTaskNewPage />, { path: "/console/scheduled_tasks/new", me: member });

    expect(await screen.findByRole("radio", { name: "Deliver to my DM" })).toBeDisabled();
    expect(screen.getByRole("radio", { name: "Deliver to a channel" })).toBeChecked();
    expect(screen.getByText("Connect your Slack account to deliver to your DM.")).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Delivery channel" })).toBeInTheDocument();
    expect(
      screen.getByText("Public channels are always available. Private channels require both you and the Centaur bot to be members."),
    ).toBeInTheDocument();
  });

  it("creates a channel task from a picked delivery channel", async () => {
    const api = mockApi({
      "GET /api/v1/scheduled_tasks/new": { data: newTask },
      "GET /api/v1/scheduled_tasks/slack_channel_options": {
        data: [{ channel_id: "C0123456789", name: "general", private: false, label: "#general", description: "C0123456789 · Public" }],
        meta: { catalog_error: null },
      },
      "POST /api/v1/scheduled_tasks": { data: task },
    });
    const { router } = renderRoute(<ScheduledTaskNewPage />, {
      path: "/console/scheduled_tasks/new",
      me: member,
      routes: [{ path: "/console/scheduled_tasks", element: <p>list page</p> }],
    });

    await userEvent.type(await screen.findByLabelText("Name"), "Weekday summary");
    await userEvent.type(screen.getByLabelText("Agent prompt"), "Summarize open incidents.");
    await userEvent.click(screen.getByRole("radio", { name: "Deliver to a channel" }));
    await userEvent.type(screen.getByRole("combobox", { name: "Delivery channel" }), "gen");
    await waitFor(() =>
      expect(
        api.calls("GET /api/v1/scheduled_tasks/slack_channel_options").some((call) => call.url.searchParams.get("q") === "gen"),
      ).toBe(true),
    );
    await userEvent.click(await screen.findByRole("option", { name: /#general/ }));
    await userEvent.click(screen.getByRole("button", { name: "Create task" }));

    expect(await screen.findByText("list page")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/console/scheduled_tasks");
    expect(toast.success).toHaveBeenCalledWith("Task created.");
    expect(api.calls("POST /api/v1/scheduled_tasks")[0]?.body).toEqual({
      data: {
        name: "Weekday summary",
        prompt: "Summarize open incidents.",
        delivery_mode: "channel",
        delivery_channel: "C0123456789",
        schedule_preset: "daily",
        custom_days: [],
        custom_time: "09:00",
        enabled: true,
      },
    });
  });

  it("disables submit while the channel input is not a channel id", async () => {
    mockApi({
      "GET /api/v1/scheduled_tasks/new": { data: { ...newTask, delivery_mode: "channel" } },
      "GET /api/v1/scheduled_tasks/slack_channel_options": { data: [], meta: { catalog_error: null } },
    });
    renderRoute(<ScheduledTaskNewPage />, { path: "/console/scheduled_tasks/new", me: member });

    const picker = await screen.findByRole("combobox", { name: "Delivery channel" });
    await userEvent.type(picker, "U0123456789");
    expect(screen.getByRole("button", { name: "Create task" })).toBeDisabled();
    await userEvent.clear(picker);
    await userEvent.type(picker, "c0123456789");
    expect(screen.getByRole("button", { name: "Create task" })).toBeEnabled();
  });

  it("shows server errors in the summary and the custom schedule hint", async () => {
    mockApi({
      "GET /api/v1/scheduled_tasks/new": { data: { ...newTask, schedule_preset: "custom", custom_days: [] } },
      "POST /api/v1/scheduled_tasks": apiResponse(422, {
        error: {
          message: "validation failed",
          details: { cron_expression: ["is not a valid cron schedule"], delivery_channel: ["is not available to the author"] },
        },
      }),
    });
    renderRoute(<ScheduledTaskNewPage />, { path: "/console/scheduled_tasks/new", me: member });

    expect(await screen.findByText("Custom schedule")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Name"), "x");
    await userEvent.type(screen.getByLabelText("Agent prompt"), "y");
    await userEvent.click(screen.getByRole("button", { name: "Create task" }));

    expect(await screen.findByText("Could not save this task:")).toBeInTheDocument();
    expect(screen.getByText("Delivery channel is not available to the author")).toBeInTheDocument();
    expect(screen.getByText("Cron expression is not a valid cron schedule")).toBeInTheDocument();
    expect(screen.getByText("Choose at least one day and a time.")).toBeInTheDocument();
  });

  it("requires a name and prompt before submitting", async () => {
    const api = mockApi({ "GET /api/v1/scheduled_tasks/new": { data: newTask } });
    renderRoute(<ScheduledTaskNewPage />, { path: "/console/scheduled_tasks/new", me: member });

    await userEvent.click(await screen.findByRole("button", { name: "Create task" }));
    expect(await screen.findAllByText("can't be blank")).toHaveLength(2);
    expect(api.calls("POST /api/v1/scheduled_tasks")).toHaveLength(0);
  });
});

describe("edit scheduled task", () => {
  it("preserves the delivery channel and maps custom days and time", async () => {
    const custom = {
      ...task,
      cron_expression: "15 6 * * 1,3,5",
      schedule_preset: "custom",
      schedule_label: "Mon, Wed, Fri at 6:15 AM PT",
      custom_days: ["1", "3", "5"],
      custom_time: "06:15",
    };
    const api = mockApi({
      "GET /api/v1/scheduled_tasks/tsk_1": { data: custom },
      "PATCH /api/v1/scheduled_tasks/tsk_1": { data: custom },
    });
    renderRoute(<ScheduledTaskEditPage />, {
      path: "/console/scheduled_tasks/:id/edit",
      url: "/console/scheduled_tasks/tsk_1/edit",
      me: member,
      routes: [{ path: "/console/scheduled_tasks", element: <p>list page</p> }],
    });

    expect(await screen.findByRole("heading", { name: "Edit Scheduled Task" })).toBeInTheDocument();
    expect(screen.getByText("Weekday summary")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/scheduled_tasks/tsk_1")[0]?.url.searchParams.get("view")).toBe("console");
    expect(screen.getByRole("radio", { name: "Deliver to a channel" })).toBeChecked();
    expect(screen.getByRole("combobox", { name: "Delivery channel" })).toHaveValue("C0123456789");
    expect(screen.getByRole("combobox", { name: "Schedule" })).toHaveTextContent("Custom");
    expect(screen.getByRole("checkbox", { name: "Mon" })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: "Wed" })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: "Tue" })).not.toBeChecked();
    expect(screen.getByLabelText("Time")).toHaveValue("06:15");
    expect(screen.getByLabelText("Time")).toBeRequired();

    await userEvent.click(screen.getByRole("checkbox", { name: "Fri" }));
    await userEvent.click(screen.getByRole("checkbox", { name: "Sun" }));
    await userEvent.click(screen.getByRole("button", { name: "Save task" }));

    expect(await screen.findByText("list page")).toBeInTheDocument();
    expect(toast.success).toHaveBeenCalledWith("Task saved.");
    expect(api.calls("PATCH /api/v1/scheduled_tasks/tsk_1")[0]?.body).toMatchObject({
      data: { schedule_preset: "custom", custom_days: ["1", "3", "0"], custom_time: "06:15", delivery_channel: "C0123456789" },
    });
  });

  it("switches to DM delivery without sending the channel", async () => {
    const api = mockApi({
      "GET /api/v1/scheduled_tasks/tsk_1": { data: task },
      "PATCH /api/v1/scheduled_tasks/tsk_1": { data: task },
    });
    renderRoute(<ScheduledTaskEditPage />, {
      path: "/console/scheduled_tasks/:id/edit",
      url: "/console/scheduled_tasks/tsk_1/edit",
      me: member,
      routes: [{ path: "/console/scheduled_tasks", element: <p>list page</p> }],
    });

    await userEvent.click(await screen.findByRole("radio", { name: "Deliver to my DM" }));
    expect(screen.queryByRole("combobox", { name: "Delivery channel" })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Save task" }));
    await screen.findByText("list page");
    expect(api.calls("PATCH /api/v1/scheduled_tasks/tsk_1")[0]?.body).toMatchObject({
      data: { delivery_mode: "dm", delivery_channel: "" },
    });
  });

  it("labels another user's task for an admin", async () => {
    mockApi({ "GET /api/v1/scheduled_tasks/tsk_1": { data: task } });
    renderRoute(<ScheduledTaskEditPage />, {
      path: "/console/scheduled_tasks/:id/edit",
      url: "/console/scheduled_tasks/tsk_1/edit",
      me: adminMe(),
    });

    expect(await screen.findByText("Weekday summary · member@example.test")).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Deliver to the author's DM" })).toBeInTheDocument();
  });
});

const workflowRow = {
  workflow_key: "slack_sync",
  label: "slack_sync",
  task_name: "centaur_workflow",
  status: "running",
  queue_name: "centaur_workflows",
  queue_label: "default",
  attempts: 1,
  max_attempts: 3,
  started_at: "2026-07-06T12:00:00Z",
  created_at: "2026-07-06T11:59:00Z",
  finished_at: null,
  queues: [],
};

const workflowDetail = {
  ...workflowRow,
  status: "failed",
  harness_type: "claudecode",
  engine_label: "Claude Code",
  duration_label: "2 minutes",
  run_count: 13,
  status_counts: [
    { status: "running", count: 1 },
    { status: "completed", count: 11 },
    { status: "failed", count: 1 },
  ],
  queues: [
    { queue_name: "centaur_workflows", queue_label: "default" },
    { queue_name: "centaur_workflows_backfill", queue_label: "backfill" },
  ],
  schedule: {
    label: "cron */5 * * * *",
    summary: "cron */5 * * * * · America/Los_Angeles",
    timezone: "America/Los_Angeles",
    enabled: true,
    source_path: "workflows/slack/sync.py",
    source_url: "https://example.test/blob/main/workflows/slack/sync.py",
  },
  debug: [
    { label: "Input", value: { mode: "incremental" }, run_id: null },
    { label: "Last failure", value: { message: "boom" }, run_id: "run-failed" },
  ],
};

const workflowRun = {
  run_id: "00000000-0000-0000-0000-000000000001",
  task_id: "00000000-0000-0000-0000-000000000002",
  status: "failed",
  queue_name: "centaur_workflows",
  queue_label: "default",
  attempts: 2,
  max_attempts: null,
  started_at: "2026-07-06T12:00:00Z",
  created_at: "2026-07-06T12:00:00Z",
  finished_at: "2026-07-06T12:02:00Z",
};

const meta = (total: number, page = 1) => ({ page, limit: 50, total, total_pages: Math.max(Math.ceil(total / 50), 1) });

describe("workflows index", () => {
  it("lists one row per workflow with the queue breakdown", async () => {
    mockApi({
      "GET /api/v1/workflows": {
        data: [
          workflowRow,
          {
            ...workflowRow,
            workflow_key: "backfill",
            label: "backfill",
            task_name: null,
            status: "completed",
            queues: [
              { queue_name: "a", queue_label: "backfill", status: "failed", last_activity_at: null, run_count: 4 },
              { queue_name: "b", queue_label: "default", status: "completed", last_activity_at: null, run_count: 9 },
            ],
          },
        ],
        meta: meta(2),
      },
    });
    renderRoute(<WorkflowsListPage />, { path: "/console/workflows" });

    expect(await screen.findByRole("link", { name: "slack_sync" })).toHaveAttribute("href", "/console/workflows/slack_sync");
    expect(screen.getByRole("heading", { name: "Workflow Runs" })).toBeInTheDocument();
    expect(screen.getByText("running")).toBeInTheDocument();
    expect(screen.getByText("centaur_workflow")).toBeInTheDocument();
    expect(screen.getByText("workflow")).toBeInTheDocument();
    expect(screen.getByText("├")).toBeInTheDocument();
    expect(screen.getByText("└")).toBeInTheDocument();
    expect(screen.getByText(/· 4 runs/)).toBeInTheDocument();
    expect(screen.getAllByText("/ 3")).toHaveLength(2);
    expect(screen.getAllByText(/queued/)).toHaveLength(2);
    expect(screen.getByText("2 workflows")).toBeInTheDocument();
    expect(document.title).toBe("Workflows · Centaur Console");
  });

  it("shows the empty state and the unavailable banner", async () => {
    mockApi({ "GET /api/v1/workflows": { data: [], meta: meta(0) } });
    const first = renderRoute(<WorkflowsListPage />, { path: "/console/workflows" });
    expect(await screen.findByText("No workflow runs yet.")).toBeInTheDocument();
    first.unmount();

    mockApi({
      "GET /api/v1/workflows": apiResponse(503, {
        error: { message: "Workflow database is unavailable.", details: { reason: "workflow_database_unavailable" } },
      }),
    });
    renderRoute(<WorkflowsListPage />, { path: "/console/workflows" });
    expect(
      await screen.findByText("Workflow database is unavailable. Console needs the API workflow read-only views to show runs."),
    ).toBeInTheDocument();
    expect(screen.getByText("No workflow runs available.")).toBeInTheDocument();
    expect(screen.queryByText(/workflows$/)).not.toBeInTheDocument();
  });

  it("sends non-admins away without fetching", async () => {
    const api = mockApi({});
    renderRoute(<WorkflowsListPage />, { path: "/console/workflows", me: member });
    expect(await screen.findByText("other page")).toBeInTheDocument();
    expect(api.calls("GET /api/v1/workflows")).toHaveLength(0);
  });
});

function workflowApi(overrides: Record<string, unknown> = {}) {
  return mockApi({
    "GET /api/v1/workflows/slack_sync": { data: workflowDetail },
    "GET /api/v1/workflows/slack_sync/runs": { data: [workflowRun], meta: meta(1) },
    ...overrides,
  });
}

describe("workflow detail", () => {
  it("renders the overview, debugging, and historical runs", async () => {
    workflowApi();
    renderRoute(<WorkflowDetailPage />, { path: "/console/workflows/:id", url: "/console/workflows/slack_sync" });

    expect(await screen.findByRole("heading", { name: "slack_sync" })).toBeInTheDocument();
    expect(screen.getAllByText("13 runs").length).toBeGreaterThanOrEqual(2);
    expect(screen.getByRole("link", { name: "workflows/slack/sync.py ↗" })).toHaveAttribute(
      "href",
      "https://example.test/blob/main/workflows/slack/sync.py",
    );
    expect(screen.getByText("default, backfill")).toBeInTheDocument();
    expect(screen.getByText("Claude Code")).toBeInTheDocument();
    expect(screen.getByText("cron */5 * * * * · America/Los_Angeles")).toBeInTheDocument();
    expect(screen.getByText("2 minutes")).toBeInTheDocument();
    expect(screen.getByText("Debugging")).toBeInTheDocument();
    expect(screen.getByText("Last failure")).toHaveClass("text-red-400");
    expect(screen.getByText("run run-failed")).toBeInTheDocument();
    expect(screen.getByText(/"mode": "incremental"/)).toBeInTheDocument();

    expect(screen.getByRole("link", { name: /all\s*13/ })).toHaveAttribute("aria-current", "true");
    expect(screen.getByRole("link", { name: /failed\s*1/ })).toHaveAttribute("href", "/console/workflows/slack_sync?status=failed");
    expect(screen.getByRole("link", { name: "backfill" })).toHaveAttribute(
      "href",
      "/console/workflows/slack_sync?queue=centaur_workflows_backfill",
    );
    const runs = await screen.findByRole("table", { name: "Historical runs" });
    expect(await within(runs).findByTitle("00000000-0000-0000-0000-000000000001")).toHaveTextContent("…");
    expect(within(runs).getByText("/ unlimited")).toBeInTheDocument();
  });

  it("marks the active status filter and passes it through", async () => {
    const api = workflowApi();
    renderRoute(<WorkflowDetailPage />, {
      path: "/console/workflows/:id",
      url: "/console/workflows/slack_sync?status=failed&queue=centaur_workflows&page=2",
    });

    expect(await screen.findByRole("link", { name: /failed\s*1/ })).toHaveAttribute("aria-current", "true");
    expect(screen.getByRole("link", { name: /running\s*1/ })).toHaveAttribute(
      "href",
      "/console/workflows/slack_sync?status=running&queue=centaur_workflows",
    );
    await waitFor(() => expect(api.calls("GET /api/v1/workflows/slack_sync/runs").length).toBeGreaterThan(0));
    const call = api.calls("GET /api/v1/workflows/slack_sync/runs")[0];
    expect(call?.url.searchParams.get("status")).toBe("failed");
    expect(call?.url.searchParams.get("queue")).toBe("centaur_workflows");
    expect(call?.url.searchParams.get("page")).toBe("2");
  });

  it("renders without api enrichment and shows the filtered empty state", async () => {
    workflowApi({
      "GET /api/v1/workflows/slack_sync": { data: { ...workflowDetail, schedule: null, debug: [], queues: [] } },
      "GET /api/v1/workflows/slack_sync/runs": { data: [], meta: meta(0) },
    });
    renderRoute(<WorkflowDetailPage />, { path: "/console/workflows/:id", url: "/console/workflows/slack_sync" });

    expect(await screen.findByRole("heading", { name: "slack_sync" })).toBeInTheDocument();
    expect(screen.queryByText("Debugging")).not.toBeInTheDocument();
    expect(screen.queryByText("Schedule")).not.toBeInTheDocument();
    expect(screen.getAllByText("Queue").length).toBeGreaterThan(0);
    expect(await screen.findByText("No runs match the selected filters.")).toBeInTheDocument();
  });

  it("force-starts a run after confirmation", async () => {
    let created = true;
    const api = workflowApi({
      "POST /api/v1/workflows/slack_sync/runs": () => ({ data: { run_id: created ? "run-new" : "run-old", created } }),
    });
    renderRoute(<WorkflowDetailPage />, { path: "/console/workflows/:id", url: "/console/workflows/slack_sync" });

    await userEvent.click(await screen.findByRole("button", { name: "Manually Trigger" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Manually trigger a slack_sync run?");
    expect(dialog).toHaveTextContent("It starts with the registered schedule input.");
    await userEvent.click(within(dialog).getByRole("button", { name: "Manually Trigger" }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith("Run queued (run-new)."));
    expect(api.calls("POST /api/v1/workflows/slack_sync/runs")[0]?.headers["X-CSRF-Token"]).toBe("csrf-test-token");

    created = false;
    await userEvent.click(screen.getByRole("button", { name: "Manually Trigger" }));
    await userEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Manually Trigger" }));
    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith("A run with this idempotency key is already queued (run-old)."),
    );
  });

  it("surfaces force-start errors", async () => {
    workflowApi({
      "GET /api/v1/workflows/slack_sync": { data: { ...workflowDetail, schedule: null } },
      "POST /api/v1/workflows/slack_sync/runs": apiResponse(502, {
        error: { message: "workflow runtime is not enabled", details: { reason: "workflow_api_error" } },
      }),
    });
    renderRoute(<WorkflowDetailPage />, { path: "/console/workflows/:id", url: "/console/workflows/slack_sync" });

    await userEvent.click(await screen.findByRole("button", { name: "Manually Trigger" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).not.toHaveTextContent("registered schedule input");
    await userEvent.click(within(dialog).getByRole("button", { name: "Manually Trigger" }));
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith("Could not start workflow: workflow runtime is not enabled"),
    );
  });

  it("shows not found and unavailable states", async () => {
    mockApi({
      "GET /api/v1/workflows/missing": apiResponse(404, { error: { message: "No workflow runs found for missing." } }),
    });
    const first = renderRoute(<WorkflowDetailPage />, { path: "/console/workflows/:id", url: "/console/workflows/missing" });
    expect(await screen.findByText(/No workflow runs found for/)).toHaveTextContent("No workflow runs found for missing.");
    expect(screen.getByRole("link", { name: "← back to workflows" })).toHaveAttribute("href", "/console/workflows");
    first.unmount();

    mockApi({
      "GET /api/v1/workflows/slack_sync": apiResponse(503, {
        error: { message: "unavailable", details: { reason: "workflow_database_unavailable" } },
      }),
    });
    renderRoute(<WorkflowDetailPage />, { path: "/console/workflows/:id", url: "/console/workflows/slack_sync" });
    expect(await screen.findByText(/Workflow database is unavailable/)).toBeInTheDocument();
  });

  it("encodes workflow keys in API paths", async () => {
    const api = mockApi({
      "GET /api/v1/workflows/team.sync%2Fx": apiResponse(404, { error: { message: "nope" } }),
    });
    renderRoute(<WorkflowDetailPage />, { path: "/console/workflows/:id", url: "/console/workflows/team.sync%2Fx" });
    expect(await screen.findByText(/No workflow runs found for/)).toBeInTheDocument();
    expect(api.calls()[0]?.url.pathname).toBe("/api/v1/workflows/team.sync%2Fx");
  });
});
