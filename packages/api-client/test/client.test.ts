import { afterEach, describe, expect, it, vi } from "vitest";

import { CentaurClient } from "../src/client";

describe("CentaurClient", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("starts workflow runs through the workflow API", async () => {
    const client = new CentaurClient({
      apiUrl: "http://api.local",
      apiKey: "test-key",
    });
    const postMock = vi.spyOn(client.http, "post").mockResolvedValue({
      data: { ok: true, run_id: "run-123", workflow_name: "nightly", status: "queued" },
    });

    await expect(
      client.startWorkflowRun({
        workflowName: "nightly",
        triggerKey: "trigger-1",
        input: { topic: "incidents" },
        eagerStart: true,
        timeoutMs: 5000,
      }),
    ).resolves.toMatchObject({ run_id: "run-123" });

    expect(postMock).toHaveBeenCalledWith(
      "/api/workflows/runs",
      {
        workflow_name: "nightly",
        trigger_key: "trigger-1",
        input: { topic: "incidents" },
        eager_start: true,
      },
      { timeout: 5000 },
    );
  });

  it("reads and mutates workflow runs through workflow endpoints", async () => {
    const client = new CentaurClient({
      apiUrl: "http://api.local",
      apiKey: "test-key",
    });
    const getMock = vi.spyOn(client.http, "get").mockResolvedValue({
      data: { ok: true, run_id: "run:123", workflow_name: "nightly", status: "completed" },
    });
    const postMock = vi.spyOn(client.http, "post").mockResolvedValue({
      data: { ok: true, run_id: "run:123", workflow_name: "nightly", status: "cancelled" },
    });

    await client.getWorkflowRun("run:123");
    await client.listWorkflowRuns({
      workflowName: "nightly",
      threadKey: "slack:C:1",
      status: "running",
      parentRunId: "root",
      limit: 5,
    });
    await client.getWorkflowChildren("run:123", 10);
    await client.cancelWorkflowRun("run:123");

    expect(getMock).toHaveBeenNthCalledWith(1, "/api/workflows/runs/run%3A123");
    expect(getMock).toHaveBeenNthCalledWith(2, "/api/workflows/runs", {
      params: {
        workflow_name: "nightly",
        thread_key: "slack:C:1",
        status: "running",
        parent_run_id: "root",
        limit: 5,
      },
    });
    expect(getMock).toHaveBeenNthCalledWith(3, "/api/workflows/runs/run%3A123/children", {
      params: { limit: 10 },
    });
    expect(postMock).toHaveBeenCalledWith("/api/workflows/runs/run%3A123/cancel");
  });

  it("sends workflow events", async () => {
    const client = new CentaurClient({
      apiUrl: "http://api.local",
      apiKey: "test-key",
    });
    const postMock = vi.spyOn(client.http, "post").mockResolvedValue({ data: { ok: true } });

    await client.sendWorkflowEvent({
      eventType: "approval.received",
      correlationId: "corr-1",
      payload: { approved: true },
    });

    expect(postMock).toHaveBeenCalledWith("/api/workflows/events", {
      event_type: "approval.received",
      correlation_id: "corr-1",
      payload: { approved: true },
    });
  });
});
