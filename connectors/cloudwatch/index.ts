import type { Ctx, Response } from "../centaur";

// Read-only CloudWatch Logs and CloudWatch (metrics/alarms) calls, sent unsigned:
// iron-proxy's aws_auth transform re-signs each request with the real read-only
// IAM keys, so no credentials are needed here. The region is non-secret config
// from AWS_REGION (default us-east-1).
//
// Responses are passed through as AWS sends them. CloudWatch Logs answers in AWS
// JSON 1.1, so its member names are camelCase (`logGroups`, `events`); the
// monitoring API uses the query-compatible JSON protocol, so its member names are
// the API reference's PascalCase (`Metrics`, `MetricAlarms`). Timestamps are
// encoded the way each protocol encodes them: Logs uses epoch milliseconds, the
// monitoring API epoch seconds or ISO-8601 strings.

const DEFAULT_REGION = "us-east-1";
const LOGS_TARGET = "Logs_20140328";
const MONITORING_TARGET = "GraniteServiceVersion20100801";

/** A CloudWatch Logs log group. */
export interface LogGroup {
  logGroupName: string;
  arn?: string;
  /** Epoch milliseconds. */
  creationTime?: number;
  retentionInDays?: number;
  metricFilterCount?: number;
  storedBytes?: number;
  logGroupClass?: string;
  dataProtectionStatus?: string;
  [field: string]: unknown;
}

/** A log stream of a group. */
export interface LogStream {
  logStreamName: string;
  /** Epoch milliseconds. */
  creationTime?: number;
  firstEventTimestamp?: number;
  lastEventTimestamp?: number;
  lastIngestionTime?: number;
  arn?: string;
  [field: string]: unknown;
}

/** One log event returned by `filter_log_events`. */
export interface FilteredLogEvent {
  eventId?: string;
  logStreamName?: string;
  /** Match time, epoch milliseconds. */
  timestamp?: number;
  /** Ingestion time, epoch milliseconds. */
  ingestionTime?: number;
  message?: string;
  [field: string]: unknown;
}

/** One Logs Insights result row: a `{ field, value }` entry per selected field. */
export interface QueryResultRow {
  field: string;
  value: string;
  [field: string]: unknown;
}

export interface QueryStatistics {
  recordsMatched?: number;
  recordsScanned?: number;
  bytesScanned?: number;
  [field: string]: unknown;
}

/** A Logs Insights query's status and results. */
export interface QueryResults {
  queryId?: string;
  /** Scheduled, Running, Complete, Failed, Cancelled or Timeout. */
  status?: string;
  /** Rows, each an array of `{ field, value }` entries. */
  results?: QueryResultRow[][];
  statistics?: QueryStatistics;
  [field: string]: unknown;
}

export interface MetricDimension {
  Name: string;
  Value: string;
  [field: string]: unknown;
}

/** A metric as `list_metrics` reports it. */
export interface Metric {
  Namespace?: string;
  MetricName?: string;
  Dimensions?: MetricDimension[];
  [field: string]: unknown;
}

/** One metric's time series from `get_metric_data`. */
export interface MetricDataResult {
  Id?: string;
  Label?: string;
  /** Epoch seconds (or the API's ISO-8601 string form). */
  Timestamps?: (number | string)[];
  Values?: number[];
  /** PartialData, InternalError, Forbidden or Complete. */
  StatusCode?: string;
  Messages?: { Code?: string; Value?: string; [field: string]: unknown }[];
  [field: string]: unknown;
}

/** A metric (or metric-math) alarm as `describe_alarms` reports it. */
export interface MetricAlarm {
  AlarmName?: string;
  AlarmArn?: string;
  AlarmDescription?: string;
  Namespace?: string;
  MetricName?: string;
  Statistic?: string;
  Dimensions?: MetricDimension[];
  Period?: number;
  EvaluationPeriods?: number;
  DatapointsToAlarm?: number;
  Threshold?: number;
  ComparisonOperator?: string;
  TreatMissingData?: string;
  /** OK, ALARM or INSUFFICIENT_DATA. */
  StateValue?: string;
  StateReason?: string;
  StateUpdatedTimestamp?: number | string;
  ActionsEnabled?: boolean;
  ConfigurationUpdatedTimestamp?: number | string;
  AlarmActions?: string[];
  OKActions?: string[];
  InsufficientDataActions?: string[];
  Metrics?: Record<string, unknown>[];
  [field: string]: unknown;
}

/** One state-change record from `get_alarm_history`. */
export interface AlarmHistoryItem {
  AlarmName?: string;
  AlarmType?: string;
  Timestamp?: number | string;
  /** ConfigurationUpdate, StateUpdate or Action. */
  HistoryItemType?: string;
  HistorySummary?: string;
  /** JSON text with the change details. */
  HistoryData?: string;
  [field: string]: unknown;
}

function region(ctx: Ctx): string {
  return (ctx.env.AWS_REGION || DEFAULT_REGION).trim();
}

/** The vendor's own error code and message, from either AWS JSON error shape. */
function awsError(status: number, body: string): Error {
  let code: string | undefined;
  let message: string | undefined;
  try {
    const parsed = JSON.parse(body) as Record<string, unknown>;
    code = (parsed.__type ?? parsed.code ?? parsed.Code) as string | undefined;
    message = (parsed.message ?? parsed.Message) as string | undefined;
  } catch {
    // Not JSON (for example a plain-text 403 from the edge): keep the body.
  }
  const text = [code, message].filter(Boolean).join(": ") || body.slice(0, 1000);
  return new Error(`CloudWatch API error (HTTP ${status}): ${text}`);
}

async function read(res: Response): Promise<any> {
  const body = await res.text();
  if (!res.ok) throw awsError(res.status, body);
  return body ? JSON.parse(body) : {};
}

/** CloudWatch Logs: AWS JSON 1.1, POST with X-Amz-Target. */
async function logs(ctx: Ctx, operation: string, payload: Record<string, unknown>): Promise<any> {
  const res = await ctx.fetch(`https://logs.${region(ctx)}.amazonaws.com/`, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-amz-json-1.1",
      "x-amz-target": `${LOGS_TARGET}.${operation}`,
    },
    body: JSON.stringify(payload),
  });
  return read(res);
}

/** CloudWatch metrics and alarms: the query-compatible JSON protocol. */
async function monitoring(
  ctx: Ctx,
  operation: string,
  payload: Record<string, unknown>,
): Promise<any> {
  const res = await ctx.fetch(`https://monitoring.${region(ctx)}.amazonaws.com/`, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-amz-json-1.0",
      "x-amz-target": `${MONITORING_TARGET}.${operation}`,
      "x-amzn-query-mode": "true",
    },
    body: JSON.stringify(payload),
  });
  return read(res);
}

/** ISO-8601 (UTC when no offset is given), epoch seconds or epoch milliseconds to epoch millis. */
function toEpochMs(value: string | number | undefined): number | undefined {
  if (value === undefined) return undefined;
  if (typeof value === "number") return Math.trunc(value > 1_000_000_000_000 ? value : value * 1000);
  const raw = value.trim();
  if (!raw) return undefined;
  if (/^\d+(\.\d+)?$/.test(raw)) {
    const seconds = Number(raw);
    return Math.trunc(seconds > 1_000_000_000_000 ? seconds : seconds * 1000);
  }
  const iso = /(?:Z|[+-]\d{2}:?\d{2})$/.test(raw) ? raw : `${raw}Z`;
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) {
    throw new Error(`timestamp ${JSON.stringify(value)} is not ISO-8601 or epoch seconds/millis`);
  }
  return ms;
}

/** `end` defaults to now and `start` to one hour before `end`, as epoch millis. */
function window(
  start: string | number | undefined,
  end: string | number | undefined,
): { startMs: number; endMs: number } {
  const endMs = toEpochMs(end) ?? Date.now();
  return { startMs: toEpochMs(start) ?? endMs - 3_600_000, endMs };
}

/**
 * Log group names, for `filter_log_events` and `start_query`. `name_prefix` filters server-side.
 * `limit` is 1-50 per page; pass the returned `next_token` as `next_token` for the next page.
 */
export async function list_log_groups(
  args: { name_prefix?: string; limit?: number; next_token?: string },
  ctx: Ctx,
): Promise<{ log_groups: LogGroup[]; next_token: string | null }> {
  const body = await logs(ctx, "DescribeLogGroups", {
    logGroupNamePrefix: args.name_prefix,
    limit: Math.max(1, Math.min(args.limit ?? 50, 50)),
    nextToken: args.next_token,
  });
  return { log_groups: body.logGroups ?? [], next_token: body.nextToken ?? null };
}

/**
 * Search log events in one group within a time window: the workhorse for grepping logs.
 * `log_group_name` is an exact group name (see `list_log_groups`); `filter_pattern` uses
 * CloudWatch Logs filter syntax (`ERROR`, `"timeout"`, `{ $.level = "error" }`), omit it for all
 * events. `start`/`end` are ISO-8601 (UTC when no offset) or epoch seconds/millis and default to
 * the last hour. `limit` is 1-10000 per page; pass `next_token` for the next page.
 */
export async function filter_log_events(
  args: {
    log_group_name: string;
    filter_pattern?: string;
    start?: string | number;
    end?: string | number;
    limit?: number;
    next_token?: string;
  },
  ctx: Ctx,
): Promise<{ events: FilteredLogEvent[]; searched_log_streams: LogStream[]; next_token: string | null }> {
  const { startMs, endMs } = window(args.start, args.end);
  const body = await logs(ctx, "FilterLogEvents", {
    logGroupName: args.log_group_name,
    filterPattern: args.filter_pattern,
    startTime: startMs,
    endTime: endMs,
    limit: Math.max(1, Math.min(args.limit ?? 100, 10000)),
    nextToken: args.next_token,
  });
  return {
    events: body.events ?? [],
    searched_log_streams: body.searchedLogStreams ?? [],
    next_token: body.nextToken ?? null,
  };
}

/**
 * Start a CloudWatch Logs Insights query. `log_group_names` is one name or a list.
 * `query_string` is Logs Insights syntax, for example
 * `fields @timestamp, @message | filter @message like /ERROR/ | sort @timestamp desc`.
 * `start`/`end` are ISO-8601 or epoch seconds/millis and default to the last hour; `limit` is the
 * maximum rows (1-10000). Returns the `query_id` to poll with `get_query_results`.
 */
export async function start_query(
  args: {
    log_group_names: string | string[];
    query_string: string;
    start?: string | number;
    end?: string | number;
    limit?: number;
  },
  ctx: Ctx,
): Promise<{ query_id: string }> {
  const names = typeof args.log_group_names === "string" ? [args.log_group_names] : args.log_group_names;
  if (!names.length) throw new Error("start_query needs at least one log group name");
  const { startMs, endMs } = window(args.start, args.end);
  const body = await logs(ctx, "StartQuery", {
    logGroupNames: names,
    queryString: args.query_string,
    // Logs Insights takes epoch seconds.
    startTime: Math.floor(startMs / 1000),
    endTime: Math.floor(endMs / 1000),
    limit: Math.max(1, Math.min(args.limit ?? 100, 10000)),
  });
  return { query_id: body.queryId };
}

/**
 * Status and rows of a Logs Insights query from `start_query`. `status` is Scheduled, Running,
 * Complete, Failed, Cancelled or Timeout; poll again (a few seconds apart) until it is Complete.
 */
export async function get_query_results(
  args: { query_id: string },
  ctx: Ctx,
): Promise<QueryResults> {
  return logs(ctx, "GetQueryResults", { queryId: args.query_id });
}

/** Stop a running Logs Insights query; `success` is false when it had already finished. */
export async function stop_query(
  args: { query_id: string },
  ctx: Ctx,
): Promise<{ success: boolean }> {
  const body = await logs(ctx, "StopQuery", { queryId: args.query_id });
  return { success: Boolean(body.success) };
}

/**
 * Metrics available in the account, optionally filtered by `namespace` (for example AWS/Lambda)
 * and `metric_name` (for example Errors). Returns namespace, name and dimensions to pass to
 * `get_metric_data`; `limit` caps the page (AWS returns up to 500), `next_token` pages on.
 */
export async function list_metrics(
  args: { namespace?: string; metric_name?: string; limit?: number; next_token?: string },
  ctx: Ctx,
): Promise<{ metrics: Metric[]; next_token: string | null }> {
  const body = await monitoring(ctx, "ListMetrics", {
    Namespace: args.namespace,
    MetricName: args.metric_name,
    NextToken: args.next_token,
  });
  const metrics: Metric[] = body.Metrics ?? [];
  return { metrics: metrics.slice(0, Math.max(1, args.limit ?? 100)), next_token: body.NextToken ?? null };
}

/**
 * Time series of one metric. `dimensions` maps dimension name to value (for example
 * `{ FunctionName: "fn" }`). `stat` is Average, Sum, Minimum, Maximum, SampleCount or a
 * percentile such as p99; `period` is the granularity in seconds (a multiple of 60).
 * `start`/`end` are ISO-8601 or epoch seconds/millis and default to the last hour. `Timestamps`
 * and `Values` are aligned arrays, oldest first.
 */
export async function get_metric_data(
  args: {
    namespace: string;
    metric_name: string;
    dimensions?: Record<string, string>;
    stat?: string;
    period?: number;
    start?: string | number;
    end?: string | number;
  },
  ctx: Ctx,
): Promise<{ metric_data_results: MetricDataResult[]; messages: Record<string, unknown>[] }> {
  const { startMs, endMs } = window(args.start, args.end);
  const dimensions = Object.entries(args.dimensions ?? {}).map(([Name, Value]) => ({ Name, Value }));
  const body = await monitoring(ctx, "GetMetricData", {
    MetricDataQueries: [
      {
        Id: "m1",
        MetricStat: {
          Metric: { Namespace: args.namespace, MetricName: args.metric_name, Dimensions: dimensions },
          Period: args.period ?? 300,
          Stat: args.stat ?? "Average",
        },
        ReturnData: true,
      },
    ],
    // The monitoring API takes epoch seconds.
    StartTime: Math.floor(startMs / 1000),
    EndTime: Math.floor(endMs / 1000),
  });
  return { metric_data_results: body.MetricDataResults ?? [], messages: body.Messages ?? [] };
}

/**
 * Metric and composite alarms, optionally filtered by `state_value` (OK, ALARM or
 * INSUFFICIENT_DATA) and `alarm_name_prefix`; use ALARM for currently-firing alarms. `limit` is
 * 1-100 per page; pass `next_token` for the next page.
 */
export async function describe_alarms(
  args: {
    state_value?: "OK" | "ALARM" | "INSUFFICIENT_DATA";
    alarm_name_prefix?: string;
    limit?: number;
    next_token?: string;
  },
  ctx: Ctx,
): Promise<{
  metric_alarms: MetricAlarm[];
  composite_alarms: Record<string, unknown>[];
  next_token: string | null;
}> {
  const body = await monitoring(ctx, "DescribeAlarms", {
    StateValue: args.state_value,
    AlarmNamePrefix: args.alarm_name_prefix,
    MaxRecords: Math.max(1, Math.min(args.limit ?? 50, 100)),
    NextToken: args.next_token,
  });
  return {
    metric_alarms: body.MetricAlarms ?? [],
    composite_alarms: body.CompositeAlarms ?? [],
    next_token: body.NextToken ?? null,
  };
}

/**
 * State-change history for one alarm (`alarm_name`) or across all alarms. `limit` is 1-100 per
 * page, `next_token` pages on; `history_item_type` restricts to ConfigurationUpdate, StateUpdate
 * or Action. `HistoryData` is JSON text with the change details.
 */
export async function get_alarm_history(
  args: {
    alarm_name?: string;
    history_item_type?: "ConfigurationUpdate" | "StateUpdate" | "Action";
    limit?: number;
    next_token?: string;
  },
  ctx: Ctx,
): Promise<{ items: AlarmHistoryItem[]; next_token: string | null }> {
  const body = await monitoring(ctx, "DescribeAlarmHistory", {
    AlarmName: args.alarm_name,
    HistoryItemType: args.history_item_type,
    MaxRecords: Math.max(1, Math.min(args.limit ?? 50, 100)),
    NextToken: args.next_token,
  });
  return { items: body.AlarmHistoryItems ?? [], next_token: body.NextToken ?? null };
}
