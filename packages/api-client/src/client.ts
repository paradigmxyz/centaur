import axios, { type AxiosInstance } from "axios";

export interface WorkflowRunOptions {
  workflowName: string;
  triggerKey?: string;
  input?: Record<string, unknown>;
  eagerStart?: boolean;
  timeoutMs?: number;
}

export interface WorkflowRunAccepted {
  ok: boolean;
  run_id: string;
  workflow_name: string;
  workflow_version?: string;
  workflow_source_path?: string | null;
  parent_run_id?: string | null;
  root_run_id?: string | null;
  status: string;
  thread_key?: string | null;
  execution_id?: string | null;
  output_json?: Record<string, unknown> | null;
  error_text?: string | null;
  latest_checkpoint_name?: string | null;
  latest_step_kind?: string | null;
  waiting_on?: Record<string, unknown> | null;
  child_runs_count?: number;
  created_at?: string | null;
  started_at?: string | null;
  completed_at?: string | null;
  idempotent?: boolean;
}

export class CentaurClient {
  readonly http: AxiosInstance;

  constructor(opts: {
    apiUrl: string;
    apiKey: string;
    timeoutMs?: number;
  }) {
    this.http = axios.create({
      baseURL: opts.apiUrl,
      headers: { Authorization: `Bearer ${opts.apiKey}` },
      timeout: opts.timeoutMs ?? 30_000,
    });
  }

  async startWorkflowRun(opts: WorkflowRunOptions): Promise<WorkflowRunAccepted> {
    const { data } = await this.http.post(
      "/api/workflows/runs",
      {
        workflow_name: opts.workflowName,
        trigger_key: opts.triggerKey,
        input: opts.input ?? {},
        eager_start: opts.eagerStart ?? false,
      },
      {
        timeout: opts.timeoutMs,
      },
    );
    return data as WorkflowRunAccepted;
  }

  async getWorkflowRun(runId: string): Promise<WorkflowRunAccepted> {
    const { data } = await this.http.get(`/api/workflows/runs/${encodeURIComponent(runId)}`);
    return data as WorkflowRunAccepted;
  }

  async listWorkflowRuns(opts?: {
    workflowName?: string;
    threadKey?: string;
    status?: string;
    parentRunId?: string;
    limit?: number;
  }): Promise<{ ok: boolean; items: WorkflowRunAccepted[] }> {
    const { data } = await this.http.get("/api/workflows/runs", {
      params: {
        workflow_name: opts?.workflowName,
        thread_key: opts?.threadKey,
        status: opts?.status,
        parent_run_id: opts?.parentRunId,
        limit: opts?.limit,
      },
    });
    return data as { ok: boolean; items: WorkflowRunAccepted[] };
  }

  async getWorkflowChildren(
    runId: string,
    limit = 200,
  ): Promise<{ ok: boolean; items: WorkflowRunAccepted[] }> {
    const { data } = await this.http.get(
      `/api/workflows/runs/${encodeURIComponent(runId)}/children`,
      {
        params: { limit },
      },
    );
    return data as { ok: boolean; items: WorkflowRunAccepted[] };
  }

  async cancelWorkflowRun(runId: string): Promise<WorkflowRunAccepted> {
    const { data } = await this.http.post(`/api/workflows/runs/${encodeURIComponent(runId)}/cancel`);
    return data as WorkflowRunAccepted;
  }

  async sendWorkflowEvent(opts: {
    eventType: string;
    correlationId: string;
    payload?: Record<string, unknown>;
  }): Promise<Record<string, unknown>> {
    const { data } = await this.http.post("/api/workflows/events", {
      event_type: opts.eventType,
      correlation_id: opts.correlationId,
      payload: opts.payload ?? {},
    });
    return data as Record<string, unknown>;
  }
}
