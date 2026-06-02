"""Push-based metrics module for VictoriaMetrics.

Lightweight in-memory metric types (Counter, Gauge, Histogram) with a
background asyncio task that periodically pushes to VictoriaMetrics.
"""

from __future__ import annotations

import asyncio
import datetime as dt
import os
import threading
from typing import Any

import httpx
import structlog
from asyncpg import Pool

log = structlog.get_logger().bind(service="api", component="vm_metrics")

_VM_URL = os.environ.get("VICTORIAMETRICS_URL", "http://victoriametrics:8428")
_PUSH_ENABLED = os.environ.get(
    "VICTORIAMETRICS_PUSH_ENABLED", "1"
).strip().lower() not in {
    "0",
    "false",
    "no",
}
_PUSH_INTERVAL_S = 15
_DEFAULT_ETL_SCHEDULE_INTERVALS_S = {
    "slack_sync": 60 * 60,
    "google_drive_sync": 4 * 60 * 60,
    "google_calendar_sync": 4 * 60 * 60,
    "linear_sync": 4 * 60 * 60,
}

# ---------------------------------------------------------------------------
# Metric primitives
# ---------------------------------------------------------------------------

_ALL_METRICS: list[_MetricBase] = []


def _escape_label_value(v: str) -> str:
    return v.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


class _MetricBase:
    def __init__(
        self, name: str, help_text: str, label_names: list[str] | None = None
    ) -> None:
        self.name = name
        self.help_text = help_text
        self.label_names: list[str] = list(label_names or [])
        self._lock = threading.Lock()
        _ALL_METRICS.append(self)

    def _label_key(self, labels: dict[str, str]) -> tuple[tuple[str, str], ...]:
        return tuple(sorted(labels.items()))

    def _format_labels(self, labels: dict[str, str]) -> str:
        if not labels:
            return ""
        parts = ",".join(
            f'{k}="{_escape_label_value(v)}"' for k, v in sorted(labels.items())
        )
        return "{" + parts + "}"


class _ChildProxy:
    """Returned by .labels() — holds a reference to the parent and label values."""

    def __init__(self, parent: Any, label_values: dict[str, str]) -> None:
        self._parent = parent
        self._labels = label_values
        self._key = parent._label_key(label_values)


class Counter(_MetricBase):
    def __init__(
        self, name: str, help_text: str, label_names: list[str] | None = None
    ) -> None:
        super().__init__(name, help_text, label_names)
        self._values: dict[tuple[tuple[str, str], ...], float] = {}

    def labels(self, **kwargs: str) -> _CounterChild:
        return _CounterChild(self, kwargs)

    def inc(self, amount: float = 1) -> None:
        if self.label_names:
            raise ValueError("Must call .labels() first on a labeled metric")
        key: tuple[tuple[str, str], ...] = ()
        with self._lock:
            self._values[key] = self._values.get(key, 0) + amount

    def _inc_keyed(self, key: tuple[tuple[str, str], ...], amount: float) -> None:
        with self._lock:
            self._values[key] = self._values.get(key, 0) + amount

    def render(self) -> str:
        lines = [f"# HELP {self.name} {self.help_text}", f"# TYPE {self.name} counter"]
        with self._lock:
            snapshot = list(self._values.items())
        for key, val in snapshot:
            labels = dict(key)
            lines.append(f"{self.name}{self._format_labels(labels)} {val}")
        return "\n".join(lines)


class _CounterChild:
    def __init__(self, parent: Counter, label_values: dict[str, str]) -> None:
        self._parent = parent
        self._key = parent._label_key(label_values)

    def inc(self, amount: float = 1) -> None:
        self._parent._inc_keyed(self._key, amount)


class Gauge(_MetricBase):
    def __init__(
        self, name: str, help_text: str, label_names: list[str] | None = None
    ) -> None:
        super().__init__(name, help_text, label_names)
        self._values: dict[tuple[tuple[str, str], ...], float] = {}

    def labels(self, **kwargs: str) -> _GaugeChild:
        return _GaugeChild(self, kwargs)

    def set(self, value: float) -> None:
        key: tuple[tuple[str, str], ...] = ()
        with self._lock:
            self._values[key] = value

    def inc(self, amount: float = 1) -> None:
        key: tuple[tuple[str, str], ...] = ()
        with self._lock:
            self._values[key] = self._values.get(key, 0) + amount

    def dec(self, amount: float = 1) -> None:
        key: tuple[tuple[str, str], ...] = ()
        with self._lock:
            self._values[key] = self._values.get(key, 0) - amount

    def _set_keyed(self, key: tuple[tuple[str, str], ...], value: float) -> None:
        with self._lock:
            self._values[key] = value

    def _inc_keyed(self, key: tuple[tuple[str, str], ...], amount: float) -> None:
        with self._lock:
            self._values[key] = self._values.get(key, 0) + amount

    def _dec_keyed(self, key: tuple[tuple[str, str], ...], amount: float) -> None:
        with self._lock:
            self._values[key] = self._values.get(key, 0) - amount

    def clear_children(self) -> None:
        """Remove all labelled child values (used before re-populating from DB)."""
        with self._lock:
            self._values.clear()

    def render(self) -> str:
        lines = [f"# HELP {self.name} {self.help_text}", f"# TYPE {self.name} gauge"]
        with self._lock:
            snapshot = list(self._values.items())
        for key, val in snapshot:
            labels = dict(key)
            lines.append(f"{self.name}{self._format_labels(labels)} {val}")
        return "\n".join(lines)


class _GaugeChild:
    def __init__(self, parent: Gauge, label_values: dict[str, str]) -> None:
        self._parent = parent
        self._key = parent._label_key(label_values)

    def set(self, value: float) -> None:
        self._parent._set_keyed(self._key, value)

    def inc(self, amount: float = 1) -> None:
        self._parent._inc_keyed(self._key, amount)

    def dec(self, amount: float = 1) -> None:
        self._parent._dec_keyed(self._key, amount)


class Histogram(_MetricBase):
    def __init__(
        self,
        name: str,
        help_text: str,
        label_names: list[str] | None = None,
        buckets: list[float] | None = None,
    ) -> None:
        super().__init__(name, help_text, label_names)
        self.buckets = sorted(buckets or _DEFAULT_BUCKETS)
        # keyed data: key -> {"buckets": [...counts...], "sum": float, "count": int}
        self._data: dict[tuple[tuple[str, str], ...], dict[str, Any]] = {}

    def _empty_data(self) -> dict[str, Any]:
        return {"buckets": [0] * len(self.buckets), "sum": 0.0, "count": 0}

    def labels(self, **kwargs: str) -> _HistogramChild:
        return _HistogramChild(self, kwargs)

    def observe(self, value: float) -> None:
        if self.label_names:
            raise ValueError("Must call .labels() first on a labeled metric")
        self._observe_keyed((), value)

    def _observe_keyed(self, key: tuple[tuple[str, str], ...], value: float) -> None:
        with self._lock:
            if key not in self._data:
                self._data[key] = self._empty_data()
            d = self._data[key]
            d["sum"] += value
            d["count"] += 1
            for i, bound in enumerate(self.buckets):
                if value <= bound:
                    d["buckets"][i] += 1

    def render(self) -> str:
        lines = [
            f"# HELP {self.name} {self.help_text}",
            f"# TYPE {self.name} histogram",
        ]
        with self._lock:
            snapshot = list(self._data.items())
        for key, d in snapshot:
            labels = dict(key)
            lbl_str = self._format_labels(labels)
            cum = 0
            for i, bound in enumerate(self.buckets):
                cum += d["buckets"][i]
                le_labels = dict(labels)
                le_labels["le"] = str(bound)
                lines.append(
                    f"{self.name}_bucket{self._format_labels(le_labels)} {cum}"
                )
            inf_labels = dict(labels)
            inf_labels["le"] = "+Inf"
            lines.append(
                f"{self.name}_bucket{self._format_labels(inf_labels)} {d['count']}"
            )
            lines.append(f"{self.name}_sum{lbl_str} {d['sum']}")
            lines.append(f"{self.name}_count{lbl_str} {d['count']}")
        return "\n".join(lines)


class _HistogramChild:
    def __init__(self, parent: Histogram, label_values: dict[str, str]) -> None:
        self._parent = parent
        self._key = parent._label_key(label_values)

    def observe(self, value: float) -> None:
        self._parent._observe_keyed(self._key, value)


_DEFAULT_BUCKETS = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10]

# ---------------------------------------------------------------------------
# Metric instances
# ---------------------------------------------------------------------------

HTTP_REQUESTS_TOTAL = Counter(
    "http_requests_total",
    "Total HTTP requests served by the API.",
    ["method", "path", "status"],
)
HTTP_REQUEST_DURATION_SECONDS = Histogram(
    "http_request_duration_seconds",
    "HTTP request latency in seconds.",
    ["method", "path"],
    buckets=[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10],
)
HTTP_REQUESTS_IN_PROGRESS = Gauge(
    "http_requests_in_progress",
    "Number of in-flight HTTP requests.",
)

AGENT_SESSIONS_ACTIVE = Gauge(
    "agent_sessions_active",
    "Number of running sandbox sessions.",
)
AGENT_EXECUTIONS_TOTAL = Counter(
    "agent_executions_total",
    "Total completed agent executions.",
    ["harness", "status"],
)
AGENT_EXECUTION_DURATION_SECONDS = Histogram(
    "agent_execution_duration_seconds",
    "Agent execution duration in seconds.",
    ["harness", "status"],
    buckets=[1, 5, 10, 30, 60, 120, 300, 600, 1800, 3600],
)

TOOL_CALLS_TOTAL = Counter(
    "agent_tool_calls_total",
    "Total tool calls by tool name and outcome.",
    ["tool_name", "tool_method", "success"],
)
TOOL_CALL_DURATION_SECONDS = Histogram(
    "agent_tool_call_duration_seconds",
    "Tool call latency in seconds.",
    ["tool_name", "tool_method"],
    buckets=[0.01, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60],
)

EXECUTIONS_ENQUEUED_TOTAL = Counter(
    "agent_executions_enqueued_total",
    "Total executions enqueued.",
    ["harness"],
)
EXECUTIONS_CLAIMED_TOTAL = Counter(
    "agent_executions_claimed_total",
    "Total executions claimed by a worker.",
    ["harness"],
)
EXECUTION_QUEUE_DELAY_SECONDS = Histogram(
    "agent_execution_queue_delay_seconds",
    "Time from enqueue to claim in seconds.",
    ["harness"],
    buckets=[0.01, 0.05, 0.1, 0.5, 1, 5, 10, 30, 60],
)
EXECUTION_WATCHDOG_TIMEOUTS_TOTAL = Counter(
    "agent_execution_watchdog_timeouts_total",
    "Execution watchdog timeouts.",
    ["harness", "reason"],
)
EXECUTION_REQUESTS_GAUGE = Gauge(
    "agent_execution_requests",
    "Current execution requests by status.",
    ["status"],
)

FINAL_DELIVERY_OUTBOX_GAUGE = Gauge(
    "agent_final_delivery_outbox",
    "Final delivery outbox items by state.",
    ["state"],
)

RUNTIME_ASSIGNMENTS_GAUGE = Gauge(
    "agent_runtime_assignments",
    "Runtime assignment rows by state.",
    ["state"],
)
RUNTIME_ASSIGNMENTS_STALE_ACTIVE = Gauge(
    "agent_runtime_assignments_stale_active",
    "Active runtime assignments older than the inactivity TTL.",
)

WARM_POOL_CONTAINERS = Gauge(
    "agent_warm_pool_containers",
    "Warm pool container counts.",
    ["state"],
)
WARM_POOL_CLAIMS_TOTAL = Counter(
    "agent_warm_pool_claims_total",
    "Warm pool claim outcomes.",
    ["outcome"],
)
EXECUTION_TERMINAL_TOTAL = Counter(
    "agent_execution_terminal_total",
    "Terminal execution outcomes by harness and reason.",
    ["harness", "status", "terminal_reason"],
)
MESSAGE_EVENTS_TOTAL = Counter(
    "agent_message_events_total",
    "Stored message events by role and whether attachments were present.",
    ["role", "has_attachments"],
)
MESSAGE_TEXT_CHARS = Histogram(
    "agent_message_text_chars",
    "Character count for stored message text.",
    ["role"],
    buckets=[10, 50, 100, 500, 1000, 5000, 10000, 50000],
)
MESSAGE_ATTACHMENTS_TOTAL = Counter(
    "agent_message_attachments_total",
    "Attachment references stored on messages.",
    ["role"],
)
RETENTION_SWEEP_ROWS_TOTAL = Counter(
    "agent_retention_sweep_rows_total",
    "Rows selected or redacted by the retention sweeper.",
    ["target", "action", "dry_run"],
)
USAGE_TOKENS_TOTAL = Counter(
    "agent_usage_tokens_total",
    "Observed model token usage by harness, model, and token category.",
    ["harness", "model", "token_type"],
)
USAGE_COST_USD_TOTAL = Counter(
    "agent_usage_cost_usd_total",
    "Observed model cost in USD by harness and model.",
    ["harness", "model"],
)
TTFT_SECONDS = Histogram(
    "agent_ttft_seconds",
    "Time to first token in seconds.",
    ["harness"],
    buckets=[0.5, 1, 2, 5, 10, 20, 30, 60],
)
ONESHOT_TOTAL = Counter(
    "agent_oneshot_total",
    "Execution outcomes for first execution on a thread (1-shot tracking).",
    ["harness", "success"],
)
TOOL_ERROR_CATEGORIES_TOTAL = Counter(
    "agent_tool_error_categories_total",
    "Tool errors by tool name and error category.",
    ["tool_name", "category"],
)
EXECUTION_BY_USER_TOTAL = Counter(
    "agent_execution_by_user_total",
    "Executions by user, harness, and terminal status.",
    ["user_id", "harness", "status"],
)
WORKFLOW_RUNS_TOTAL = Counter(
    "workflow_runs_total",
    "Total workflow runs by name and terminal status.",
    ["workflow_name", "status"],
)
WORKFLOW_RUN_DURATION_SECONDS = Histogram(
    "workflow_run_duration_seconds",
    "Workflow run duration in seconds (queued to completed).",
    ["workflow_name", "status"],
    buckets=[1, 5, 10, 30, 60, 120, 300, 600, 1800, 3600],
)
WORKFLOW_RUNS_ENQUEUED_TOTAL = Counter(
    "workflow_runs_enqueued_total",
    "Total workflow runs enqueued.",
    ["workflow_name"],
)
WORKFLOW_RUNS_CLAIMED_TOTAL = Counter(
    "workflow_runs_claimed_total",
    "Total workflow runs claimed by a worker.",
    ["workflow_name"],
)
WORKFLOW_RUN_QUEUE_DELAY_SECONDS = Histogram(
    "workflow_run_queue_delay_seconds",
    "Time from enqueue to claim in seconds.",
    ["workflow_name"],
    buckets=[0.01, 0.05, 0.1, 0.5, 1, 5, 10, 30, 60],
)
WORKFLOW_EVENTS_TOTAL = Counter(
    "workflow_events_total",
    "Total workflow events sent.",
    ["event_type"],
)
WORKFLOW_RUNS_GAUGE = Gauge(
    "workflow_runs",
    "Current workflow runs by status.",
    ["status"],
)
ETL_SOURCE_CURSOR_LAG_SECONDS = Gauge(
    "etl_source_cursor_lag_seconds",
    "Worst source checkpoint lag in seconds.",
    ["source", "source_type"],
)
ETL_SCOPE_SYNC_FRESHNESS_SECONDS = Gauge(
    "etl_scope_sync_freshness_seconds",
    "Worst schedule-adjusted overdue time for a syncable ETL scope.",
    ["source", "source_type"],
)
ETL_ACTIVE_SCOPES = Gauge(
    "etl_active_scopes",
    "Number of source scopes expected to sync.",
    ["source", "source_type"],
)
ETL_FAILED_SCOPES = Gauge(
    "etl_failed_scopes",
    "Number of source scopes with a recorded sync failure.",
    ["source", "source_type"],
)
ETL_BACKFILL_JOBS = Gauge(
    "etl_backfill_jobs",
    "Number of backfill jobs by source, job type, and status.",
    ["source", "job_type", "status"],
)
ETL_BACKFILL_JOB_AGE_SECONDS = Gauge(
    "etl_backfill_job_age_seconds",
    "Oldest age in seconds for backfill jobs by source, job type, and status.",
    ["source", "job_type", "status"],
)
ETL_ITEMS_ENQUEUED_TOTAL = Counter(
    "etl_items_enqueued_total",
    "Deferred source items enqueued by ETL workflows.",
    ["source", "source_type", "item_type"],
)
ETL_ITEMS_DELETED_TOTAL = Counter(
    "etl_items_deleted_total",
    "Source items hard-deleted by ETL workflows.",
    ["source", "source_type", "item_type"],
)
ETL_ITEMS_SEEN_TOTAL = Counter(
    "etl_items_seen_total",
    "Raw source items fetched or observed by ETL workflows.",
    ["source", "source_type", "item_type"],
)
ETL_ITEMS_UPSERTED_TOTAL = Counter(
    "etl_items_upserted_total",
    "Raw source items upserted by ETL workflows.",
    ["source", "source_type", "item_type"],
)
ETL_ITEMS_FAILED_TOTAL = Counter(
    "etl_items_failed_total",
    "Source items or scopes that failed ETL processing.",
    ["source", "source_type", "item_type", "reason"],
)
COMPANY_CONTEXT_DOCUMENTS_CHANGED_TOTAL = Counter(
    "company_context_documents_changed_total",
    "Company context documents changed by projection workflows.",
    ["source", "source_type", "action"],
)
COMPANY_CONTEXT_PROJECTION_LAG_SECONDS = Gauge(
    "company_context_projection_lag_seconds",
    "Lag between raw source data updates and projected company context documents.",
    ["source"],
)
COMPANY_CONTEXT_DOCUMENT_SIZE_CHARS = Histogram(
    "company_context_document_size_chars",
    "Generated company context document body size in characters.",
    ["source", "source_type"],
    buckets=[100, 500, 1_000, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000],
)

# ---------------------------------------------------------------------------
# Helper functions (same public interface as the old metrics.py)
# ---------------------------------------------------------------------------


def observe_http_request(
    method: str, path: str, status: int, duration_s: float
) -> None:
    HTTP_REQUESTS_TOTAL.labels(method=method, path=path, status=str(status)).inc()
    HTTP_REQUEST_DURATION_SECONDS.labels(method=method, path=path).observe(duration_s)


def record_agent_execution(harness: str, status: str, duration_s: float) -> None:
    AGENT_EXECUTIONS_TOTAL.labels(harness=harness, status=status).inc()
    AGENT_EXECUTION_DURATION_SECONDS.labels(harness=harness, status=status).observe(
        duration_s
    )


def record_execution_terminal(harness: str, status: str, terminal_reason: str) -> None:
    EXECUTION_TERMINAL_TOTAL.labels(
        harness=harness,
        status=status,
        terminal_reason=terminal_reason,
    ).inc()


def record_tool_call(
    tool_name: str, tool_method: str, success: bool, duration_s: float
) -> None:
    TOOL_CALLS_TOTAL.labels(
        tool_name=tool_name, tool_method=tool_method, success=str(success).lower()
    ).inc()
    TOOL_CALL_DURATION_SECONDS.labels(
        tool_name=tool_name, tool_method=tool_method
    ).observe(duration_s)


def record_execution_enqueued(harness: str) -> None:
    EXECUTIONS_ENQUEUED_TOTAL.labels(harness=harness).inc()


def record_execution_claimed(harness: str, queue_delay_s: float) -> None:
    EXECUTIONS_CLAIMED_TOTAL.labels(harness=harness).inc()
    EXECUTION_QUEUE_DELAY_SECONDS.labels(harness=harness).observe(queue_delay_s)


def record_execution_watchdog_timeout(harness: str, reason: str) -> None:
    EXECUTION_WATCHDOG_TIMEOUTS_TOTAL.labels(harness=harness, reason=reason).inc()


def record_warm_pool_claim(outcome: str) -> None:
    WARM_POOL_CLAIMS_TOTAL.labels(outcome=outcome).inc()


def record_message_observation(
    role: str, text_chars: int, attachment_count: int
) -> None:
    has_attachments = "true" if attachment_count > 0 else "false"
    MESSAGE_EVENTS_TOTAL.labels(role=role, has_attachments=has_attachments).inc()
    MESSAGE_TEXT_CHARS.labels(role=role).observe(max(text_chars, 0))
    if attachment_count > 0:
        MESSAGE_ATTACHMENTS_TOTAL.labels(role=role).inc(attachment_count)


def record_retention_sweep(target: str, action: str, dry_run: bool, count: int) -> None:
    if count > 0:
        RETENTION_SWEEP_ROWS_TOTAL.labels(
            target=target,
            action=action,
            dry_run=str(dry_run).lower(),
        ).inc(count)


def record_ttft(harness: str, ttft_s: float) -> None:
    TTFT_SECONDS.labels(harness=harness).observe(ttft_s)


def record_oneshot(harness: str, success: bool) -> None:
    ONESHOT_TOTAL.labels(harness=harness, success=str(success).lower()).inc()


def record_tool_error_category(tool_name: str, category: str) -> None:
    TOOL_ERROR_CATEGORIES_TOTAL.labels(tool_name=tool_name, category=category).inc()


def record_execution_by_user(user_id: str, harness: str, status: str) -> None:
    EXECUTION_BY_USER_TOTAL.labels(
        user_id=user_id, harness=harness, status=status
    ).inc()


def record_workflow_run_terminal(
    workflow_name: str, status: str, duration_s: float
) -> None:
    WORKFLOW_RUNS_TOTAL.labels(workflow_name=workflow_name, status=status).inc()
    WORKFLOW_RUN_DURATION_SECONDS.labels(
        workflow_name=workflow_name, status=status
    ).observe(duration_s)


def record_workflow_run_enqueued(workflow_name: str) -> None:
    WORKFLOW_RUNS_ENQUEUED_TOTAL.labels(workflow_name=workflow_name).inc()


def record_workflow_run_claimed(workflow_name: str, queue_delay_s: float) -> None:
    WORKFLOW_RUNS_CLAIMED_TOTAL.labels(workflow_name=workflow_name).inc()
    WORKFLOW_RUN_QUEUE_DELAY_SECONDS.labels(workflow_name=workflow_name).observe(
        queue_delay_s
    )


def record_workflow_event_sent(event_type: str) -> None:
    WORKFLOW_EVENTS_TOTAL.labels(event_type=event_type).inc()


def record_etl_items_seen(
    source: str, source_type: str, item_type: str, count: int
) -> None:
    if count > 0:
        ETL_ITEMS_SEEN_TOTAL.labels(
            source=source,
            source_type=source_type,
            item_type=item_type,
        ).inc(count)


def record_etl_items_enqueued(
    source: str, source_type: str, item_type: str, count: int
) -> None:
    if count > 0:
        ETL_ITEMS_ENQUEUED_TOTAL.labels(
            source=source,
            source_type=source_type,
            item_type=item_type,
        ).inc(count)


def record_etl_items_upserted(
    source: str, source_type: str, item_type: str, count: int
) -> None:
    if count > 0:
        ETL_ITEMS_UPSERTED_TOTAL.labels(
            source=source,
            source_type=source_type,
            item_type=item_type,
        ).inc(count)


def record_etl_items_deleted(
    source: str, source_type: str, item_type: str, count: int
) -> None:
    if count > 0:
        ETL_ITEMS_DELETED_TOTAL.labels(
            source=source,
            source_type=source_type,
            item_type=item_type,
        ).inc(count)


def record_etl_items_failed(
    source: str,
    source_type: str,
    item_type: str,
    reason: str,
    count: int = 1,
) -> None:
    if count > 0:
        ETL_ITEMS_FAILED_TOTAL.labels(
            source=source,
            source_type=source_type,
            item_type=item_type,
            reason=reason,
        ).inc(count)


def record_company_context_documents_changed(
    source: str,
    source_type: str,
    action: str,
    count: int = 1,
) -> None:
    if count > 0:
        COMPANY_CONTEXT_DOCUMENTS_CHANGED_TOTAL.labels(
            source=source,
            source_type=source_type,
            action=action,
        ).inc(count)


def observe_company_context_document_size(
    source: str, source_type: str, chars: int
) -> None:
    COMPANY_CONTEXT_DOCUMENT_SIZE_CHARS.labels(
        source=source,
        source_type=source_type,
    ).observe(max(chars, 0))


def record_usage_observation(
    harness: str,
    model: str | None,
    *,
    input_tokens: int = 0,
    output_tokens: int = 0,
    cache_creation_input_tokens: int = 0,
    cache_read_input_tokens: int = 0,
    cost_usd: float = 0.0,
) -> None:
    resolved_model = model or "unknown"
    token_values = {
        "input_tokens": max(input_tokens, 0),
        "output_tokens": max(output_tokens, 0),
        "cache_creation_input_tokens": max(cache_creation_input_tokens, 0),
        "cache_read_input_tokens": max(cache_read_input_tokens, 0),
    }
    for token_type, value in token_values.items():
        if value > 0:
            USAGE_TOKENS_TOTAL.labels(
                harness=harness,
                model=resolved_model,
                token_type=token_type,
            ).inc(value)
    if cost_usd > 0:
        USAGE_COST_USD_TOTAL.labels(harness=harness, model=resolved_model).inc(cost_usd)


# ---------------------------------------------------------------------------
# DB-driven gauge refresh
# ---------------------------------------------------------------------------


async def refresh_runtime_metrics(pool: Pool) -> None:
    active_sessions = await pool.fetchval(
        "SELECT COUNT(*) FROM sandbox_sessions WHERE state = 'running'"
    )
    AGENT_SESSIONS_ACTIVE.set(int(active_sessions or 0))

    EXECUTION_REQUESTS_GAUGE.clear_children()
    rows = await pool.fetch(
        "SELECT status, COUNT(*) AS cnt FROM agent_execution_requests "
        "WHERE status IN ('queued', 'running', 'retry_wait', 'cancel_requested') "
        "GROUP BY status"
    )
    for row in rows:
        EXECUTION_REQUESTS_GAUGE.labels(status=row["status"]).set(row["cnt"])

    FINAL_DELIVERY_OUTBOX_GAUGE.clear_children()
    rows = await pool.fetch(
        "SELECT state, COUNT(*) AS cnt FROM agent_final_delivery_outbox "
        "WHERE state NOT IN ('delivered') "
        "GROUP BY state"
    )
    for row in rows:
        FINAL_DELIVERY_OUTBOX_GAUGE.labels(state=row["state"]).set(row["cnt"])

    RUNTIME_ASSIGNMENTS_GAUGE.clear_children()
    rows = await pool.fetch(
        "SELECT state, COUNT(*) AS cnt FROM agent_runtime_assignments GROUP BY state"
    )
    for row in rows:
        RUNTIME_ASSIGNMENTS_GAUGE.labels(state=row["state"]).set(row["cnt"])
    stale_active = await pool.fetchval(
        "SELECT COUNT(*) FROM agent_runtime_assignments "
        "WHERE state = 'active' "
        "  AND updated_at < NOW() - make_interval(secs => $1::double precision)",
        float(os.getenv("IDLE_TTL_S", "86400")),
    )
    RUNTIME_ASSIGNMENTS_STALE_ACTIVE.set(int(stale_active or 0))

    WORKFLOW_RUNS_GAUGE.clear_children()
    rows = await pool.fetch(
        "SELECT status, COUNT(*) AS cnt FROM workflow_runs "
        "WHERE status IN ('queued', 'running', 'sleeping', 'waiting') "
        "GROUP BY status"
    )
    for row in rows:
        WORKFLOW_RUNS_GAUGE.labels(status=row["status"]).set(row["cnt"])

    await refresh_etl_metrics(pool)


async def _etl_schedule_intervals_s(pool: Pool) -> dict[str, int]:
    intervals = dict(_DEFAULT_ETL_SCHEDULE_INTERVALS_S)
    rows = await pool.fetch(
        "SELECT schedule_id, interval_seconds "
        "FROM workflow_schedules "
        "WHERE schedule_id = ANY($1::text[]) "
        "  AND schedule_kind = 'interval' "
        "  AND interval_seconds IS NOT NULL "
        "  AND interval_seconds > 0",
        list(intervals),
    )
    for row in rows:
        intervals[str(row["schedule_id"])] = int(row["interval_seconds"])
    return intervals


def _sync_freshness_overdue_s(
    success_at: dt.datetime,
    *,
    now: dt.datetime,
    schedule_interval_s: int,
) -> float:
    age_s = (now - success_at.astimezone(dt.timezone.utc)).total_seconds()
    return max(age_s - float(schedule_interval_s), 0.0)


async def refresh_etl_metrics(pool: Pool) -> None:
    """Refresh ETL freshness gauges from durable Postgres state."""
    ETL_SOURCE_CURSOR_LAG_SECONDS.clear_children()
    ETL_SCOPE_SYNC_FRESHNESS_SECONDS.clear_children()
    ETL_ACTIVE_SCOPES.clear_children()
    ETL_FAILED_SCOPES.clear_children()
    ETL_BACKFILL_JOBS.clear_children()
    ETL_BACKFILL_JOB_AGE_SECONDS.clear_children()
    COMPANY_CONTEXT_PROJECTION_LAG_SECONDS.clear_children()

    schedule_intervals_s = await _etl_schedule_intervals_s(pool)
    slack_scope_rows = await pool.fetch(
        "SELECT ch.channel_id, cp.watermark_ts, cp.last_success_at, cp.last_error, cp.updated_at "
        "FROM slack_sync_channels ch "
        "LEFT JOIN slack_sync_checkpoints cp ON cp.channel_id = ch.channel_id "
        "WHERE ch.is_syncable = TRUE"
    )
    ETL_ACTIVE_SCOPES.labels(source="slack", source_type="channel").set(
        len(slack_scope_rows)
    )
    failed_scopes = 0
    max_lag_s = 0.0
    max_sync_freshness_s = 0.0
    now = dt.datetime.now(dt.timezone.utc)
    for row in slack_scope_rows:
        has_error = bool(str(row["last_error"] or "").strip())
        if has_error:
            failed_scopes += 1
        watermark_ts = str(row["watermark_ts"] or "").strip()
        if watermark_ts:
            try:
                watermark = dt.datetime.fromtimestamp(
                    float(watermark_ts), tz=dt.timezone.utc
                )
            except (TypeError, ValueError, OSError):
                pass
            else:
                max_lag_s = max(max_lag_s, (now - watermark).total_seconds())
        if not has_error:
            success_at = row["last_success_at"] or row["updated_at"]
            if isinstance(success_at, dt.datetime):
                max_sync_freshness_s = max(
                    max_sync_freshness_s,
                    _sync_freshness_overdue_s(
                        success_at,
                        now=now,
                        schedule_interval_s=schedule_intervals_s["slack_sync"],
                    ),
                )
    ETL_FAILED_SCOPES.labels(source="slack", source_type="channel").set(failed_scopes)
    ETL_SOURCE_CURSOR_LAG_SECONDS.labels(source="slack", source_type="channel").set(
        max(max_lag_s, 0.0)
    )
    ETL_SCOPE_SYNC_FRESHNESS_SECONDS.labels(source="slack", source_type="channel").set(
        max(max_sync_freshness_s, 0.0)
    )

    drive_scope_rows = await pool.fetch(
        "SELECT scope_id, watermark_time, last_success_at, last_error, updated_at "
        "FROM google_drive_sync_checkpoints"
    )
    drive_active_scopes = len(drive_scope_rows)
    if os.getenv("GOOGLE_DRIVE_ETL_ENABLED", "").strip().lower() not in {
        "",
        "0",
        "false",
        "no",
        "off",
    }:
        drive_active_scopes = max(drive_active_scopes, 1)
    ETL_ACTIVE_SCOPES.labels(source="google_drive", source_type="doc").set(
        drive_active_scopes
    )
    failed_scopes = 0
    max_lag_s = 0.0
    max_sync_freshness_s = 0.0
    for row in drive_scope_rows:
        has_error = bool(str(row["last_error"] or "").strip())
        if has_error:
            failed_scopes += 1
        watermark_time = row["watermark_time"]
        if isinstance(watermark_time, dt.datetime):
            max_lag_s = max(
                max_lag_s,
                (now - watermark_time.astimezone(dt.timezone.utc)).total_seconds(),
            )
        if not has_error:
            success_at = row["last_success_at"] or row["updated_at"]
            if isinstance(success_at, dt.datetime):
                max_sync_freshness_s = max(
                    max_sync_freshness_s,
                    _sync_freshness_overdue_s(
                        success_at,
                        now=now,
                        schedule_interval_s=schedule_intervals_s["google_drive_sync"],
                    ),
                )
    ETL_FAILED_SCOPES.labels(source="google_drive", source_type="doc").set(
        failed_scopes
    )
    ETL_SOURCE_CURSOR_LAG_SECONDS.labels(source="google_drive", source_type="doc").set(
        max(max_lag_s, 0.0)
    )
    ETL_SCOPE_SYNC_FRESHNESS_SECONDS.labels(
        source="google_drive", source_type="doc"
    ).set(max(max_sync_freshness_s, 0.0))

    calendar_scope_rows = await pool.fetch(
        "SELECT cal.calendar_id, cp.watermark_time, cp.last_success_at, cp.last_error, cp.updated_at "
        "FROM google_calendar_sync_calendars cal "
        "LEFT JOIN google_calendar_sync_checkpoints cp ON cp.calendar_id = cal.calendar_id"
    )
    calendar_active_scopes = len(calendar_scope_rows)
    if os.getenv("GOOGLE_CALENDAR_ETL_ENABLED", "").strip().lower() not in {
        "",
        "0",
        "false",
        "no",
        "off",
    }:
        calendar_active_scopes = max(calendar_active_scopes, 1)
    ETL_ACTIVE_SCOPES.labels(source="google_calendar", source_type="calendar").set(
        calendar_active_scopes
    )
    failed_scopes = 0
    max_lag_s = 0.0
    max_sync_freshness_s = 0.0
    for row in calendar_scope_rows:
        has_error = bool(str(row["last_error"] or "").strip())
        if has_error:
            failed_scopes += 1
        watermark_time = row["watermark_time"]
        if isinstance(watermark_time, dt.datetime):
            max_lag_s = max(
                max_lag_s,
                (now - watermark_time.astimezone(dt.timezone.utc)).total_seconds(),
            )
        if not has_error:
            success_at = row["last_success_at"] or row["updated_at"]
            if isinstance(success_at, dt.datetime):
                max_sync_freshness_s = max(
                    max_sync_freshness_s,
                    _sync_freshness_overdue_s(
                        success_at,
                        now=now,
                        schedule_interval_s=schedule_intervals_s[
                            "google_calendar_sync"
                        ],
                    ),
                )
    ETL_FAILED_SCOPES.labels(source="google_calendar", source_type="calendar").set(
        failed_scopes
    )
    ETL_SOURCE_CURSOR_LAG_SECONDS.labels(
        source="google_calendar", source_type="calendar"
    ).set(max(max_lag_s, 0.0))
    ETL_SCOPE_SYNC_FRESHNESS_SECONDS.labels(
        source="google_calendar", source_type="calendar"
    ).set(max(max_sync_freshness_s, 0.0))

    linear_scope_rows = await pool.fetch(
        "SELECT scope_id, watermark_time, last_success_at, last_error, updated_at "
        "FROM linear_sync_checkpoints"
    )
    linear_active_scopes = len(linear_scope_rows)
    if os.getenv("LINEAR_ETL_ENABLED", "").strip().lower() not in {
        "",
        "0",
        "false",
        "no",
        "off",
    }:
        linear_active_scopes = max(linear_active_scopes, 3)
    ETL_ACTIVE_SCOPES.labels(source="linear", source_type="workspace").set(
        linear_active_scopes
    )
    failed_scopes = 0
    max_lag_s = 0.0
    max_sync_freshness_s = 0.0
    for row in linear_scope_rows:
        has_error = bool(str(row["last_error"] or "").strip())
        if has_error:
            failed_scopes += 1
        watermark_time = row["watermark_time"]
        if isinstance(watermark_time, dt.datetime):
            max_lag_s = max(
                max_lag_s,
                (now - watermark_time.astimezone(dt.timezone.utc)).total_seconds(),
            )
        if not has_error:
            success_at = row["last_success_at"] or row["updated_at"]
            if isinstance(success_at, dt.datetime):
                max_sync_freshness_s = max(
                    max_sync_freshness_s,
                    _sync_freshness_overdue_s(
                        success_at,
                        now=now,
                        schedule_interval_s=schedule_intervals_s["linear_sync"],
                    ),
                )
    ETL_FAILED_SCOPES.labels(source="linear", source_type="workspace").set(
        failed_scopes
    )
    ETL_SOURCE_CURSOR_LAG_SECONDS.labels(source="linear", source_type="workspace").set(
        max(max_lag_s, 0.0)
    )
    ETL_SCOPE_SYNC_FRESHNESS_SECONDS.labels(
        source="linear", source_type="workspace"
    ).set(max(max_sync_freshness_s, 0.0))

    backfill_rows = await pool.fetch(
        "SELECT job_type, status, COUNT(*) AS cnt, MIN(updated_at) AS oldest_updated_at "
        "FROM slack_sync_backfill_jobs "
        "GROUP BY job_type, status"
    )
    for row in backfill_rows:
        job_type = str(row["job_type"] or "")
        status = str(row["status"] or "")
        ETL_BACKFILL_JOBS.labels(
            source="slack",
            job_type=job_type,
            status=status,
        ).set(int(row["cnt"] or 0))
        age_seconds = 0.0
        oldest_updated_at = row["oldest_updated_at"]
        if isinstance(oldest_updated_at, dt.datetime):
            age_seconds = max(
                (now - oldest_updated_at.astimezone(dt.timezone.utc)).total_seconds(),
                0.0,
            )
        ETL_BACKFILL_JOB_AGE_SECONDS.labels(
            source="slack",
            job_type=job_type,
            status=status,
        ).set(age_seconds)

    raw_latest = await pool.fetchval("SELECT MAX(updated_at) FROM slack_sync_messages")
    projected_latest = await pool.fetchval(
        "SELECT MAX(source_updated_at) FROM company_context_documents WHERE source = 'slack'"
    )
    projection_lag_s = 0.0
    if isinstance(raw_latest, dt.datetime):
        raw_latest = raw_latest.astimezone(dt.timezone.utc)
        if isinstance(projected_latest, dt.datetime):
            projected_latest = projected_latest.astimezone(dt.timezone.utc)
            projection_lag_s = max((raw_latest - projected_latest).total_seconds(), 0.0)
        else:
            projection_lag_s = max((now - raw_latest).total_seconds(), 0.0)
    COMPANY_CONTEXT_PROJECTION_LAG_SECONDS.labels(source="slack").set(projection_lag_s)

    raw_latest = await pool.fetchval(
        "SELECT MAX(updated_at) FROM google_drive_sync_files"
    )
    projected_latest = await pool.fetchval(
        "SELECT MAX(source_updated_at) FROM company_context_documents "
        "WHERE source = 'google_drive'"
    )
    projection_lag_s = 0.0
    if isinstance(raw_latest, dt.datetime):
        raw_latest = raw_latest.astimezone(dt.timezone.utc)
        if isinstance(projected_latest, dt.datetime):
            projected_latest = projected_latest.astimezone(dt.timezone.utc)
            projection_lag_s = max((raw_latest - projected_latest).total_seconds(), 0.0)
        else:
            projection_lag_s = max((now - raw_latest).total_seconds(), 0.0)
    COMPANY_CONTEXT_PROJECTION_LAG_SECONDS.labels(source="google_drive").set(
        projection_lag_s
    )

    raw_latest = await pool.fetchval(
        "SELECT MAX(source_updated_at) FROM google_calendar_sync_events"
    )
    projected_latest = await pool.fetchval(
        "SELECT MAX(source_updated_at) FROM company_context_documents "
        "WHERE source = 'google_calendar'"
    )
    projection_lag_s = 0.0
    if isinstance(raw_latest, dt.datetime):
        raw_latest = raw_latest.astimezone(dt.timezone.utc)
        if isinstance(projected_latest, dt.datetime):
            projected_latest = projected_latest.astimezone(dt.timezone.utc)
            projection_lag_s = max((raw_latest - projected_latest).total_seconds(), 0.0)
        else:
            projection_lag_s = max((now - raw_latest).total_seconds(), 0.0)
    COMPANY_CONTEXT_PROJECTION_LAG_SECONDS.labels(source="google_calendar").set(
        projection_lag_s
    )


# ---------------------------------------------------------------------------
# Render (text exposition format for VictoriaMetrics import)
# ---------------------------------------------------------------------------


async def render_metrics(pool: Pool) -> bytes:
    await refresh_runtime_metrics(pool)
    parts = [m.render() for m in _ALL_METRICS if m.render()]
    return ("\n".join(parts) + "\n").encode("utf-8")


def _render_metrics_sync() -> str:
    parts = [m.render() for m in _ALL_METRICS if m.render()]
    return "\n".join(parts) + "\n"


# ---------------------------------------------------------------------------
# Push loop
# ---------------------------------------------------------------------------

_push_task: asyncio.Task[None] | None = None
_push_pool: Pool | None = None


async def _push_loop() -> None:
    url = f"{_VM_URL}/api/v1/import/prometheus"
    async with httpx.AsyncClient(timeout=10) as client:
        while True:
            await asyncio.sleep(_PUSH_INTERVAL_S)
            try:
                if _push_pool is not None:
                    await refresh_runtime_metrics(_push_pool)
                payload = _render_metrics_sync()
                if payload.strip():
                    resp = await client.post(
                        url,
                        content=payload,
                        headers={"Content-Type": "text/plain"},
                    )
                    if resp.status_code >= 400:
                        log.warning(
                            "vm_push_failed",
                            status=resp.status_code,
                            body=resp.text[:200],
                        )
            except asyncio.CancelledError:
                raise
            except Exception:
                log.warning("vm_push_error", exc_info=True)


def start_push_loop(pool: Pool) -> None:
    global _push_task, _push_pool
    if not _PUSH_ENABLED:
        log.info("vm_push_loop_disabled")
        return
    _push_pool = pool
    _push_task = asyncio.create_task(_push_loop())
    log.info("vm_push_loop_started", url=_VM_URL, interval_s=_PUSH_INTERVAL_S)


async def stop_push_loop() -> None:
    global _push_task, _push_pool
    if _push_task is not None:
        _push_task.cancel()
        try:
            await _push_task
        except asyncio.CancelledError:
            pass
        _push_task = None
        log.info("vm_push_loop_stopped")
    _push_pool = None
