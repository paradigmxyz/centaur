"""VictoriaLogs HTTP API client for LogsQL queries, field names, and field values."""

import json
import os
import re
import shlex
from typing import Any

import httpx

_DURATION_RE = re.compile(r"^\d+[smhdw]$")
_DURATION_SECONDS = {"s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}


def _quote_logsql_value(value: str) -> str:
    """Quote a LogsQL field value, escaping characters used by thread keys."""
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def _field_expr(field: str, value: str) -> str:
    return f"{field}:{_quote_logsql_value(value)}"


class VictoriaLogsClient:
    """Client for the VictoriaLogs HTTP API.

    Queries logs via LogsQL, lists field names/values, and retrieves log streams.
    Connects directly to the VictoriaLogs instance (default: http://victorialogs:9428).
    """

    def __init__(self, url: str | None = None, timeout: float = 30.0):
        self._url = url
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def base_url(self) -> str:
        url = (self._url or os.getenv("VICTORIALOGS_URL", "http://victorialogs:9428")).rstrip("/")  # noqa: TID251
        if url and not url.startswith(("http://", "https://")):
            url = f"http://{url}"
        return url

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(base_url=self.base_url, timeout=self.timeout)
        return self._client

    def _request(
        self,
        method: str,
        path: str,
        params: dict | None = None,
        data: dict | None = None,
    ) -> httpx.Response:
        resp = self.client.request(method, path, params=params, data=data)
        if resp.status_code >= 400:
            raise RuntimeError(f"VictoriaLogs API error ({resp.status_code}): {resp.text}")
        return resp

    # -- Queries ---------------------------------------------------------------

    def query(
        self,
        query: str,
        limit: int = 100,
        start: str | None = None,
        end: str | None = None,
    ) -> list[dict]:
        """Run a LogsQL query and return matching log entries.

        Args:
            query: LogsQL expression (e.g. '_time:5m error').
            limit: Max log lines to return.
            start: Range start (RFC3339 or Unix timestamp).
            end: Range end. Defaults to now.
        """
        data: dict[str, Any] = {"query": f"{query} | limit {limit}"}
        if start:
            data["start"] = start
        if end:
            data["end"] = end
        resp = self._request("POST", "/select/logsql/query", data=data)
        lines = []
        for line in resp.text.strip().split("\n"):
            if line:
                lines.append(json.loads(line))
        if len(lines) >= limit:
            lines.append(
                {"_note": f"Results truncated at {limit}. Increase limit or narrow your query."}
            )
        return lines

    def hits(
        self,
        query: str,
        start: str | None = None,
        end: str | None = None,
        step: str | None = None,
    ) -> dict:
        """Query log hits stats over a time range.

        Args:
            query: LogsQL expression.
            start: Range start.
            end: Range end.
            step: Step between data points (e.g. '5m').
        """
        data: dict[str, Any] = {"query": query}
        if start:
            data["start"] = start
        if end:
            data["end"] = end
        if step:
            data["step"] = step
        resp = self._request("POST", "/select/logsql/hits", data=data)
        return resp.json()

    def field_names(
        self, query: str = "*", start: str | None = None, end: str | None = None
    ) -> list[str]:
        """List all known field names.

        Args:
            query: Optional LogsQL filter to scope field names.
            start: Optional start time filter.
            end: Optional end time filter.
        """
        data: dict[str, str] = {"query": query}
        if start:
            data["start"] = start
        if end:
            data["end"] = end
        resp = self._request("POST", "/select/logsql/field_names", data=data)
        result = resp.json()
        return [v["value"] for v in result.get("values", [])]

    def field_values(
        self,
        field: str,
        query: str = "*",
        limit: int = 100,
        start: str | None = None,
        end: str | None = None,
    ) -> list[str]:
        """Get all values for a specific field.

        Args:
            field: Field name (e.g. 'service', 'container').
            query: Optional LogsQL filter to scope values.
            limit: Max values to return.
            start: Optional start time filter.
            end: Optional end time filter.
        """
        data: dict[str, Any] = {"query": query, "field": field, "limit": limit}
        if start:
            data["start"] = start
        if end:
            data["end"] = end
        resp = self._request("POST", "/select/logsql/field_values", data=data)
        result = resp.json()
        return [v["value"] for v in result.get("values", [])]

    def streams(
        self,
        query: str = "*",
        start: str | None = None,
        end: str | None = None,
    ) -> list[dict]:
        """Find log streams matching a query.

        Args:
            query: LogsQL expression.
            start: Optional start time filter.
            end: Optional end time filter.
        """
        data: dict[str, str] = {"query": query}
        if start:
            data["start"] = start
        if end:
            data["end"] = end
        resp = self._request("POST", "/select/logsql/streams", data=data)
        result = resp.json()
        return result.get("values", [])

    def ready(self) -> bool:
        """Check if VictoriaLogs is ready to serve requests."""
        try:
            resp = self.client.get("/health")
            return resp.status_code == 200
        except Exception:
            return False

    # -- Helpers ---------------------------------------------------------------

    @staticmethod
    def _clean_entry(entry: dict) -> dict:
        """Strip internal VictoriaLogs fields for readability."""
        return {k: v for k, v in entry.items() if k not in ("_stream_id", "_stream")}

    @staticmethod
    def _time_prefix(start: str) -> str:
        """Convert a shorthand duration like '1h' into a LogsQL _time: prefix."""
        if start and _DURATION_RE.match(start):
            return f"_time:{start} "
        return ""

    def _time_params(self, start: str) -> dict[str, str]:
        """Return start param dict only when start is an absolute timestamp."""
        if start and not _DURATION_RE.match(start):
            return {"start": start}
        return {}

    @staticmethod
    def _coerce_float(value: Any) -> float:
        if isinstance(value, bool):
            return float(value)
        if isinstance(value, int | float):
            return float(value)
        if isinstance(value, str):
            try:
                return float(value)
            except ValueError:
                return 0.0
        return 0.0

    @staticmethod
    def _format_tool_args(value: Any) -> str:
        if value is None:
            return "(no args)"
        if isinstance(value, list | tuple):
            if not value:
                return "(no args)"
            return shlex.join(str(item) for item in value)
        if isinstance(value, dict):
            if not value:
                return "(no args)"
            return json.dumps(value, sort_keys=True, separators=(",", ":"), default=str)
        text = str(value)
        return text if text else "(no args)"

    @classmethod
    def _tool_args_label(cls, entry: dict) -> str:
        if "tool_args" not in entry:
            return "(not captured)"
        return cls._format_tool_args(entry.get("tool_args"))

    @classmethod
    def _hits_step(cls, start: str) -> str:
        """Choose a reasonable bucket size for `/select/logsql/hits`."""
        if not start or not _DURATION_RE.match(start):
            return "1h"
        total_seconds = int(start[:-1]) * _DURATION_SECONDS[start[-1]]
        if total_seconds <= 3600:
            return "1m"
        if total_seconds <= 6 * 3600:
            return "5m"
        if total_seconds <= 86400:
            return "1h"
        if total_seconds <= 7 * 86400:
            return "1d"
        return "1w"

    @classmethod
    def _hits_total(cls, payload: dict[str, Any]) -> int:
        total = 0
        for series in payload.get("hits", []):
            if isinstance(series, dict) and "total" in series:
                total += int(cls._coerce_float(series.get("total")))
                continue
            values = series.get("values", []) if isinstance(series, dict) else []
            total += sum(int(cls._coerce_float(value)) for value in values)
        return total

    # -- Convenience methods ---------------------------------------------------

    def thread_logs(
        self,
        thread_key: str,
        level: str = "",
        limit: int = 50,
        start: str = "",
        end: str = "",
    ) -> list[dict]:
        """Get all logs for a thread across all services.

        Args:
            thread_key: Thread identifier.
            level: Optional log level filter (e.g. 'error').
            limit: Max entries to return.
            start: Time range start (shorthand like '1h' or RFC3339).
            end: Time range end (RFC3339).
        """
        parts = [self._time_prefix(start), _field_expr("thread_key", thread_key)]
        if level:
            parts.append(f"level:{level}")
        q = " AND ".join(p for p in parts if p and not p.startswith("_time:"))
        q = f"{self._time_prefix(start)}{q}"
        params = self._time_params(start)
        results = self.query(q, limit=limit, end=end or None, **params)
        return [self._clean_entry(e) for e in results if "_note" not in e]

    def thread_trace(
        self,
        thread_key: str,
        start: str = "24h",
        limit: int = 500,
    ) -> list[dict]:
        """Return an end-to-end filtered timeline for a thread across services."""
        flow_events = [
            "mention_received",
            "thread_history_backfilled",
            "assignment_ready",
            "spawn_completed",
            "message_buffered",
            "message_stored",
            "execute_start",
            "execute_queued",
            "execute_claimed",
            "execute_started",
            "sse_connect",
            "turn_start",
            "sandbox_spawned",
            "sandbox_attached",
            "sandbox_stdin_write",
            "turn_first_output",
            "assistant_tool_use_observed",
            "tool_call_started",
            "tool_call_completed",
            "tool_result_observed",
            "subagent_status_observed",
            "command_execution_observed",
            "assistant_text_observed",
            "usage_observed",
            "result_observed",
            "turn_done",
            "execution_summary",
            "execute_complete",
            "execute_completed",
            "final_delivery_ready",
            "final_delivery_claimed",
            "final_delivery_started",
            "final_delivery_delivered",
            "final_delivery_completed",
            "final_delivery_failed",
            "final_delivery_ack_failed",
            "final_delivery_post_failed",
            "final_delivery_suppressed",
            "sse_disconnect",
            "wire_reconnecting",
            "wire_reconnect_exhausted",
            "inflight_turn_replayed",
        ]
        event_expr = " OR ".join(f"event:{event_name}" for event_name in flow_events)
        q = f"{self._time_prefix(start)}{_field_expr('thread_key', thread_key)} AND ({event_expr})"
        results = self.query(q, limit=limit, **self._time_params(start))
        return [self._clean_entry(e) for e in results if "_note" not in e]

    def errors(
        self,
        service: str = "",
        thread_key: str = "",
        start: str = "1h",
        limit: int = 50,
    ) -> list[dict]:
        """Query error-level logs, optionally filtered by service and/or thread.

        Args:
            service: Service name filter.
            thread_key: Thread key filter.
            start: Time range (default '1h'). Shorthand or RFC3339.
            limit: Max entries.
        """
        parts = ["level:error"]
        if service:
            parts.append(f'_stream:{{service="{service}"}}')
        if thread_key:
            parts.append(_field_expr("thread_key", thread_key))
        q = f"{self._time_prefix(start)}{' AND '.join(parts)}"
        results = self.query(q, limit=limit, **self._time_params(start))
        return [self._clean_entry(e) for e in results if "_note" not in e]

    def slow_requests(
        self,
        threshold_ms: int = 5000,
        start: str = "1h",
        limit: int = 50,
    ) -> list[dict]:
        """Find HTTP requests slower than threshold.

        Args:
            threshold_ms: Duration threshold in milliseconds.
            start: Time range (default '1h').
            limit: Max entries.
        """
        q = f"{self._time_prefix(start)}event:http_request AND duration_ms:>{threshold_ms}"
        results = self.query(q, limit=limit, **self._time_params(start))
        cleaned = [self._clean_entry(e) for e in results if "_note" not in e]
        cleaned.sort(key=lambda e: self._coerce_float(e.get("duration_ms", 0)), reverse=True)
        return cleaned

    def tool_calls(
        self,
        tool_name: str = "",
        thread_key: str = "",
        start: str = "1h",
        limit: int = 100,
    ) -> list[dict]:
        """Query tool call events.

        Args:
            tool_name: Optional tool name filter.
            thread_key: Optional thread key filter.
            start: Time range (default '1h').
            limit: Max entries.
        """
        parts = ["event:tool_call_completed"]
        if tool_name:
            parts.append(f"tool_name:{tool_name}")
        if thread_key:
            parts.append(_field_expr("thread_key", thread_key))
        q = f"{self._time_prefix(start)}{' AND '.join(parts)}"
        results = self.query(q, limit=limit, **self._time_params(start))
        keep_fields = {
            "_time",
            "_msg",
            "duration_ms",
            "success",
            "tool_args",
            "tool_args_count",
            "tool_args_truncated",
            "tool_name",
            "tool_method",
            "thread_key",
        }
        return [
            {k: v for k, v in self._clean_entry(e).items() if k in keep_fields}
            for e in results
            if "_note" not in e
        ]

    def execution_timeline(self, execution_id: str) -> list[dict]:
        """Get all log events for a specific execution, chronologically ordered.

        Args:
            execution_id: Execution identifier to search for.
        """
        q = f"execution_id:{execution_id}"
        results = self.query(q, limit=1000)
        cleaned = [self._clean_entry(e) for e in results if "_note" not in e]
        cleaned.sort(key=lambda e: e.get("_time", ""))
        return cleaned

    def service_health(self, start: str = "1h") -> dict:
        """Aggregate error and total log counts per service.

        Args:
            start: Time range (default '1h').

        Returns:
            Dict keyed by service name with error_count and total_count.
        """
        time_prefix = self._time_prefix(start)
        time_params = self._time_params(start)

        svc_names = [
            svc
            for svc in self.field_values("service", query=f"{time_prefix}*", **time_params)
            if svc.strip()
        ]
        result: dict[str, dict[str, int]] = {}
        step = self._hits_step(start)
        for svc in svc_names:
            total = self.hits(f'_stream:{{service="{svc}"}}', start=start or None, step=step)
            total_count = self._hits_total(total)
            errors = self.hits(
                f'_stream:{{service="{svc}"}} AND level:error',
                start=start or None,
                step=step,
            )
            error_count = self._hits_total(errors)
            result[svc] = {"total_count": total_count, "error_count": error_count}
        return result

    def sandbox_activity(self, start: str = "1h", limit: int = 50) -> list[dict]:
        """Query sandbox container lifecycle events.

        Args:
            start: Time range (default '1h').
            limit: Max entries.
        """
        q = (
            f"{self._time_prefix(start)}"
            f'_stream:{{service="sandbox"}} OR event:warm_container_claimed OR event:sandbox_*'
        )
        results = self.query(q, limit=limit, **self._time_params(start))
        return [self._clean_entry(e) for e in results if "_note" not in e]

    def tool_analytics(
        self,
        start: str = "24h",
        limit: int = 30,
    ) -> list[dict]:
        """Get tool usage analytics: call counts, failure rates, avg duration per tool.

        Args:
            start: Time range (default '24h').
            limit: Max tools to return.
        """
        q = f"{self._time_prefix(start)}event:tool_call_completed"
        results = self.query(q, limit=10000, **self._time_params(start))

        from collections import defaultdict

        stats: dict[str, dict] = defaultdict(
            lambda: {
                "calls": 0,
                "failures": 0,
                "total_duration_ms": 0,
                "args": defaultdict(int),
                "methods": defaultdict(int),
                "threads": set(),
            }
        )
        for entry in results:
            if "_note" in entry:
                continue
            tool = entry.get("tool_name", "unknown")
            method = entry.get("tool_method", "unknown")
            args = self._tool_args_label(entry)
            success = entry.get("success", "true") == "true"
            duration = round(self._coerce_float(entry.get("duration_ms", 0)))
            thread = entry.get("thread_key", "")

            stats[tool]["calls"] += 1
            if not success:
                stats[tool]["failures"] += 1
            stats[tool]["total_duration_ms"] += duration
            stats[tool]["args"][args] += 1
            stats[tool]["methods"][method] += 1
            if thread:
                stats[tool]["threads"].add(thread)

        result = []
        for tool, s in sorted(stats.items(), key=lambda x: x[1]["calls"], reverse=True)[:limit]:
            avg_ms = round(s["total_duration_ms"] / s["calls"]) if s["calls"] else 0
            failure_rate = round(s["failures"] / s["calls"] * 100, 1) if s["calls"] else 0
            result.append(
                {
                    "tool": tool,
                    "calls": s["calls"],
                    "failures": s["failures"],
                    "failure_rate_pct": failure_rate,
                    "avg_duration_ms": avg_ms,
                    "unique_threads": len(s["threads"]),
                    "args": dict(
                        sorted(s["args"].items(), key=lambda item: item[1], reverse=True)
                    ),
                    "methods": dict(s["methods"]),
                }
            )
        return result

    def tool_usage_by_thread(
        self,
        thread_key: str = "",
        start: str = "24h",
        limit: int = 200,
    ) -> list[dict]:
        """Get tool calls for a specific thread, or top threads by tool usage.

        Args:
            thread_key: If provided, show all tool calls for this thread. If empty, show top threads.
            start: Time range (default '24h').
            limit: Max entries.
        """
        if thread_key:
            q = (
                f"{self._time_prefix(start)}event:tool_call_completed "
                f"AND {_field_expr('thread_key', thread_key)}"
            )
            results = self.query(q, limit=limit, **self._time_params(start))
            keep_fields = {
                "_time",
                "tool_args",
                "tool_args_count",
                "tool_args_truncated",
                "tool_name",
                "tool_method",
                "duration_ms",
                "success",
            }
            return [
                {k: v for k, v in self._clean_entry(e).items() if k in keep_fields}
                for e in results
                if "_note" not in e
            ]

        # Top threads by tool usage
        q = f"{self._time_prefix(start)}event:tool_call_completed AND thread_key:*"
        results = self.query(q, limit=10000, **self._time_params(start))
        from collections import Counter

        threads: Counter = Counter()
        for entry in results:
            if "_note" in entry:
                continue
            tk = entry.get("thread_key", "")
            if tk:
                threads[tk] += 1
        return [{"thread_key": tk, "tool_calls": cnt} for tk, cnt in threads.most_common(limit)]

    def execution_summaries(
        self,
        start: str = "24h",
        harness: str = "",
        status: str = "",
        prompt_ref: str = "",
        limit: int = 100,
    ) -> list[dict]:
        """Return recent execution summary events for improvement-loop analysis."""
        parts = ["event:execution_summary"]
        if harness:
            parts.append(f"harness:{harness}")
        if status:
            parts.append(f"status:{status}")
        if prompt_ref:
            parts.append(f"prompt_ref:{prompt_ref}")
        q = f"{self._time_prefix(start)}{' AND '.join(parts)}"
        results = self.query(q, limit=limit, **self._time_params(start))
        keep_fields = {
            "_time",
            "execution_id",
            "thread_key",
            "harness",
            "engine",
            "persona_id",
            "prompt_ref",
            "prompt_sha",
            "status",
            "terminal_reason",
            "duration_s",
            "ttft_ms",
            "execution_sequence",
            "user_id",
            "total_tokens",
            "cost_usd",
            "models",
            "assistant_tool_use_events",
            "tool_error_events",
            "tool_retry_count",
            "tool_error_categories",
            "command_error_events",
            "subagent_failures",
            "tool_calls_by_name",
            "tool_errors_by_name",
        }
        return [
            {k: v for k, v in self._clean_entry(entry).items() if k in keep_fields}
            for entry in results
            if "_note" not in entry
        ]

    def prompt_analytics(
        self,
        start: str = "7d",
        limit: int = 50,
    ) -> list[dict]:
        """Aggregate execution outcomes by prompt lineage for prompt optimization."""
        from collections import defaultdict

        stats: dict[tuple[str, str], dict[str, Any]] = defaultdict(
            lambda: {
                "runs": 0,
                "completed": 0,
                "failed": 0,
                "total_duration_s": 0.0,
                "total_tokens": 0,
                "total_cost_usd": 0.0,
                "models": set(),
                "harnesses": set(),
                "terminal_reasons": defaultdict(int),
            }
        )
        for entry in self.execution_summaries(start=start, limit=10000):
            prompt_ref = str(entry.get("prompt_ref") or "")
            prompt_sha = str(entry.get("prompt_sha") or "")
            if not prompt_ref:
                continue
            key = (prompt_ref, prompt_sha)
            stat = stats[key]
            stat["runs"] += 1
            if entry.get("status") == "completed":
                stat["completed"] += 1
            else:
                stat["failed"] += 1
            stat["total_duration_s"] += float(entry.get("duration_s") or 0.0)
            stat["total_tokens"] += int(entry.get("total_tokens") or 0)
            stat["total_cost_usd"] += float(entry.get("cost_usd") or 0.0)
            if entry.get("harness"):
                stat["harnesses"].add(str(entry["harness"]))
            for model in entry.get("models") or []:
                if model:
                    stat["models"].add(str(model))
            reason = str(entry.get("terminal_reason") or "")
            if reason:
                stat["terminal_reasons"][reason] += 1

        rows: list[dict] = []
        for (prompt_ref, prompt_sha), stat in stats.items():
            runs = stat["runs"]
            rows.append(
                {
                    "prompt_ref": prompt_ref,
                    "prompt_sha": prompt_sha,
                    "runs": runs,
                    "completed": stat["completed"],
                    "failed": stat["failed"],
                    "success_rate_pct": round((stat["completed"] / runs) * 100, 1) if runs else 0.0,
                    "avg_duration_s": round(stat["total_duration_s"] / runs, 3) if runs else 0.0,
                    "avg_tokens": round(stat["total_tokens"] / runs) if runs else 0,
                    "avg_cost_usd": round(stat["total_cost_usd"] / runs, 6) if runs else 0.0,
                    "models": sorted(stat["models"]),
                    "harnesses": sorted(stat["harnesses"]),
                    "terminal_reasons": dict(stat["terminal_reasons"]),
                }
            )
        return sorted(rows, key=lambda row: row["runs"], reverse=True)[:limit]

    def model_analytics(
        self,
        start: str = "24h",
        limit: int = 50,
    ) -> list[dict]:
        """Aggregate model usage and cost for research-loop tuning."""
        from collections import defaultdict

        q = f"{self._time_prefix(start)}event:usage_observed"
        results = self.query(q, limit=10000, **self._time_params(start))
        stats: dict[tuple[str, str], dict[str, float]] = defaultdict(
            lambda: {
                "calls": 0.0,
                "input_tokens": 0.0,
                "output_tokens": 0.0,
                "cache_creation_input_tokens": 0.0,
                "cache_read_input_tokens": 0.0,
                "cost_usd": 0.0,
            }
        )
        for entry in results:
            if "_note" in entry:
                continue
            harness = str(entry.get("harness") or "unknown")
            model = str(entry.get("model") or "unknown")
            stat = stats[(harness, model)]
            stat["calls"] += 1
            stat["input_tokens"] += float(entry.get("input_tokens") or 0)
            stat["output_tokens"] += float(entry.get("output_tokens") or 0)
            stat["cache_creation_input_tokens"] += float(
                entry.get("cache_creation_input_tokens") or 0
            )
            stat["cache_read_input_tokens"] += float(entry.get("cache_read_input_tokens") or 0)
            stat["cost_usd"] += float(entry.get("cost_usd") or 0.0)

        rows = []
        for (harness, model), stat in stats.items():
            rows.append(
                {
                    "harness": harness,
                    "model": model,
                    "calls": int(stat["calls"]),
                    "input_tokens": int(stat["input_tokens"]),
                    "output_tokens": int(stat["output_tokens"]),
                    "cache_creation_input_tokens": int(stat["cache_creation_input_tokens"]),
                    "cache_read_input_tokens": int(stat["cache_read_input_tokens"]),
                    "total_tokens": int(
                        stat["input_tokens"]
                        + stat["output_tokens"]
                        + stat["cache_creation_input_tokens"]
                        + stat["cache_read_input_tokens"]
                    ),
                    "cost_usd": round(stat["cost_usd"], 6),
                    "avg_cost_usd": round(stat["cost_usd"] / stat["calls"], 6)
                    if stat["calls"]
                    else 0.0,
                }
            )
        return sorted(rows, key=lambda row: row["cost_usd"], reverse=True)[:limit]

    # -- Lifecycle -------------------------------------------------------------

    def close(self):
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def _client() -> VictoriaLogsClient:
    return VictoriaLogsClient()
