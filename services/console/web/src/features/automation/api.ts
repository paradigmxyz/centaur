import { keepPreviousData, useQuery, useSuspenseQuery, type QueryClient } from "@tanstack/react-query";

import type { SlackChannelLoader, SlackChannelOption } from "@/components/slack-channel-picker";
import { api, type DataEnvelope, type ListEnvelope } from "@/lib/api";
import { createQueryKeys } from "@/lib/query-keys";

export const scheduledTaskKeys = createQueryKeys("scheduled_tasks");
export const workflowKeys = createQueryKeys("workflows");

export type DeliveryMode = "dm" | "channel";
export type SchedulePreset = "manual" | "daily" | "weekdays" | "mondays" | "fridays" | "custom";

export interface ScheduledTask {
  id: string | null;
  name: string | null;
  prompt: string | null;
  author_id: string | null;
  delivery_channel: string | null;
  delivery_mode: DeliveryMode;
  delivery_name: string | null;
  direct_message_user_id: string | null;
  cron_expression: string | null;
  timezone: string;
  schedule_preset: SchedulePreset;
  schedule_label: string;
  custom_days: string[];
  custom_time: string;
  enabled: boolean;
  next_run_at: string | null;
  last_run_at: string | null;
  last_error: string | null;
  created_at: string | null;
  updated_at: string | null;
}

export interface ScheduledTaskInput {
  name: string;
  prompt: string;
  delivery_mode: DeliveryMode;
  delivery_channel: string;
  schedule_preset: SchedulePreset;
  custom_days: string[];
  custom_time: string;
  enabled: boolean;
}

const tasksBase = "/api/v1/scheduled_tasks";

export function useScheduledTasks() {
  return useQuery({
    queryKey: scheduledTaskKeys.list(),
    queryFn: ({ signal }) => api.get<DataEnvelope<ScheduledTask[]>>(tasksBase, undefined, signal),
    select: (body) => body.data,
  });
}

export function useNewScheduledTask() {
  return useSuspenseQuery({
    queryKey: scheduledTaskKeys.detail("new"),
    queryFn: ({ signal }) => api.get<DataEnvelope<ScheduledTask>>(`${tasksBase}/new`, undefined, signal),
    select: (body) => body.data,
  });
}

export function useScheduledTask(id: string) {
  return useSuspenseQuery({
    queryKey: scheduledTaskKeys.detail(id),
    queryFn: ({ signal }) =>
      api.get<DataEnvelope<ScheduledTask>>(`${tasksBase}/${encodeURIComponent(id)}/edit`, undefined, signal),
    select: (body) => body.data,
  });
}

const refreshTaskLists = (queryClient: QueryClient) =>
  queryClient.invalidateQueries({ queryKey: scheduledTaskKeys.lists() });

export async function createScheduledTask(queryClient: QueryClient, data: ScheduledTaskInput) {
  const body = await api.post<DataEnvelope<ScheduledTask>>(tasksBase, { data });
  await refreshTaskLists(queryClient);
  return body;
}

export async function updateScheduledTask(queryClient: QueryClient, id: string, data: ScheduledTaskInput) {
  const body = await api.patch<DataEnvelope<ScheduledTask>>(`${tasksBase}/${encodeURIComponent(id)}`, { data });
  void refreshTaskLists(queryClient);
  void queryClient.invalidateQueries({ queryKey: scheduledTaskKeys.detail(id) });
  return body;
}

export async function deleteScheduledTask(queryClient: QueryClient, id: string) {
  await api.delete(`${tasksBase}/${encodeURIComponent(id)}`);
  await refreshTaskLists(queryClient);
}

export async function runScheduledTask(queryClient: QueryClient, id: string) {
  await api.post(`${tasksBase}/${encodeURIComponent(id)}/run`);
  await refreshTaskLists(queryClient);
}

interface ChannelOptionsResponse {
  data: SlackChannelOption[];
  meta: { catalog_error: string | null };
}

/**
 * Delivery channel search under the task author's SlackDeliveryPolicy (public
 * channels plus private channels shared by the author and the bot).
 */
export function deliveryChannelLoader(taskId?: string | null): SlackChannelLoader {
  return async (query, signal) => {
    const body = await api.get<ChannelOptionsResponse>(
      `${tasksBase}/slack_channel_options`,
      { q: query, task_id: taskId ?? undefined },
      signal,
    );
    return { options: body.data, error: body.meta.catalog_error };
  };
}

export interface WorkflowTiming {
  status: string;
  queue_name: string | null;
  queue_label: string | null;
  attempts: number;
  max_attempts: number | null;
  started_at: string | null;
  created_at: string | null;
  finished_at: string | null;
}

export interface WorkflowQueueSummary {
  queue_name: string | null;
  queue_label: string | null;
  status: string;
  last_activity_at: string | null;
  run_count: number;
}

export interface WorkflowRow extends WorkflowTiming {
  workflow_key: string | null;
  label: string;
  task_name: string | null;
  queues: WorkflowQueueSummary[];
}

export interface WorkflowRun extends WorkflowTiming {
  run_id: string;
  task_id: string | null;
}

export interface WorkflowSchedule {
  label: string | null;
  summary: string | null;
  timezone: string | null;
  enabled: boolean;
  source_path: string | null;
  source_url: string | null;
}

export interface WorkflowDebugRow {
  label: string;
  value: unknown;
  run_id: string | null;
}

export interface WorkflowDetail extends WorkflowTiming {
  workflow_key: string;
  label: string;
  task_name: string | null;
  harness_type: string | null;
  engine_label: string | null;
  duration_label: string;
  run_count: number;
  status_counts: { status: string; count: number }[];
  queues: { queue_name: string; queue_label: string }[];
  schedule: WorkflowSchedule | null;
  debug: WorkflowDebugRow[];
}

const workflowsBase = "/api/v1/workflows";
const workflowPath = (key: string) => `${workflowsBase}/${encodeURIComponent(key)}`;

export function useWorkflows(page: number) {
  return useQuery({
    queryKey: workflowKeys.list({ page }),
    queryFn: ({ signal }) => api.get<ListEnvelope<WorkflowRow>>(workflowsBase, { page }, signal),
    placeholderData: keepPreviousData,
  });
}

export function useWorkflow(key: string) {
  return useQuery({
    queryKey: workflowKeys.detail(key),
    queryFn: ({ signal }) => api.get<DataEnvelope<WorkflowDetail>>(workflowPath(key), undefined, signal),
    select: (body) => body.data,
  });
}

export function useWorkflowRuns(key: string, params: { status: string; queue: string; page: number }, enabled: boolean) {
  return useQuery({
    queryKey: workflowKeys.sub(key, "runs", params),
    queryFn: ({ signal }) =>
      api.get<ListEnvelope<WorkflowRun>>(
        `${workflowPath(key)}/runs`,
        { status: params.status, queue: params.queue, page: params.page },
        signal,
      ),
    placeholderData: keepPreviousData,
    enabled,
  });
}

export async function forceStartWorkflow(queryClient: QueryClient, key: string) {
  const body = await api.post<DataEnvelope<{ run_id: string; created: boolean }>>(`${workflowPath(key)}/runs`);
  await queryClient.invalidateQueries({ queryKey: workflowKeys.all });
  return body;
}
