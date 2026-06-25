"""Laminar trace investigation client for Centaur agents."""

from __future__ import annotations

import json
import os
from typing import Any

import httpx

DEFAULT_BASE_URL = "http://laminar-app-server.laminar.svc.cluster.local:8000"
DEFAULT_EXTERNAL_URL = "http://prd-centaur-na-laminar.tail388b2e.ts.net"
DEFAULT_PROJECT_ID = "202e8d91-1311-40f8-9217-ad375d3ab4df"
MAX_LIMIT = 500
MAX_MINUTES = 60 * 24 * 14


def _clamp_int(value: int, *, minimum: int, maximum: int) -> int:
    return max(minimum, min(maximum, int(value)))


def _safe_json(value: str | None) -> dict[str, Any]:
    if not value:
        return {}
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def _interesting_attributes(attributes: dict[str, Any]) -> dict[str, Any]:
    prefixes = (
        "centaur.",
        "slack.",
        "http.",
        "url.",
        "service.",
        "gen_ai.",
        "codex.",
        "exception.",
    )
    return {
        key: value
        for key, value in attributes.items()
        if any(key.startswith(prefix) for prefix in prefixes)
    }


def _truncate(value: str, limit: int) -> str:
    if len(value) <= limit:
        return value
    return value[:limit] + f"... [truncated {len(value) - limit} chars]"


class LaminarClient:
    """Query Laminar traces through the Laminar app-server SQL API.

    The production Centaur deployment exports API and sandbox traces to the in-cluster
    Laminar app-server. This tool uses Laminar's read SQL endpoint so agents can
    inspect their own traces without Kubernetes access.
    """

    def __init__(
        self,
        base_url: str | None = None,
        api_key: str | None = None,
        project_id: str | None = None,
        external_url: str | None = None,
        timeout: float = 30.0,
    ):
        self._base_url = base_url
        self._api_key = api_key
        self._project_id = project_id
        self._external_url = external_url
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def base_url(self) -> str:
        url = (
            self._base_url or os.getenv("LAMINAR_BASE_URL", DEFAULT_BASE_URL)  # noqa: TID251
        ).rstrip("/")
        if url and not url.startswith(("http://", "https://")):
            url = f"http://{url}"
        return url

    @property
    def external_url(self) -> str:
        return (
            self._external_url or os.getenv("LAMINAR_EXTERNAL_URL", DEFAULT_EXTERNAL_URL)  # noqa: TID251
        ).rstrip("/")

    @property
    def api_key(self) -> str:
        return (self._api_key or os.getenv("LAMINAR_API_KEY", "")).strip()  # noqa: TID251

    @property
    def project_id(self) -> str:
        return (
            self._project_id or os.getenv("LAMINAR_PROJECT_ID", DEFAULT_PROJECT_ID)  # noqa: TID251
        ).strip()

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(base_url=self.base_url, timeout=self.timeout)
        return self._client

    def _headers(self) -> dict[str, str]:
        key = self.api_key
        if not key:
            return {}
        return {"authorization": f"Bearer {key}"}

    def _query(self, sql: str, parameters: dict[str, Any] | None = None) -> list[dict[str, Any]]:
        project_id = self.project_id
        path = f"/api/v1/projects/{project_id}/sql/query" if project_id else "/v1/sql/query"
        if not project_id and not self.api_key:
            raise RuntimeError("Set LAMINAR_PROJECT_ID or LAMINAR_API_KEY to query Laminar traces")
        response = self.client.post(
            path,
            headers=self._headers(),
            json={"query": sql, "parameters": parameters or {}},
        )
        if response.status_code >= 400:
            raise RuntimeError(f"Laminar SQL API error ({response.status_code}): {response.text}")
        payload = response.json()
        if isinstance(payload, list):
            return payload
        data = payload.get("data", payload)
        if not isinstance(data, list):
            raise RuntimeError(f"Unexpected Laminar SQL response: {payload!r}")
        return data

    def health(self) -> dict[str, Any]:
        """Check whether the configured Laminar app-server is reachable."""
        try:
            response = self.client.get("/health")
            ready = response.status_code == 200
            body = response.text[:200]
        except Exception as exc:
            ready = False
            body = str(exc)
        return {
            "ready": ready,
            "base_url": self.base_url,
            "external_url": self.external_url,
            "project_id": self.project_id,
            "auth_configured": bool(self.api_key),
            "response": body,
        }

    def query(self, sql: str, parameters: dict[str, Any] | None = None) -> list[dict[str, Any]]:
        """Run a read-only Laminar SQL query.

        Useful tables are `traces` and `spans`. Laminar validates and scopes the query
        to `LAMINAR_PROJECT_ID`, or to `LAMINAR_API_KEY` when no project id is set.
        """
        return self._query(sql, parameters)

    def recent_traces(self, minutes: int = 60, limit: int = 20) -> list[dict[str, Any]]:
        """List recent traces with span counts, duration, status, and top span names."""
        minutes = _clamp_int(minutes, minimum=1, maximum=MAX_MINUTES)
        limit = _clamp_int(limit, minimum=1, maximum=MAX_LIMIT)
        return self._query(
            f"""
            SELECT
                id AS trace_id,
                start_time,
                end_time,
                duration,
                status,
                top_span_name,
                session_id,
                user_id,
                total_tokens,
                total_cost
            FROM traces
            WHERE start_time >= now() - INTERVAL {minutes} MINUTE
            ORDER BY start_time DESC
            LIMIT {limit}
            """
        )

    def find_traces(
        self,
        thread_key: str | None = None,
        execution_id: str | None = None,
        session_id: str | None = None,
        minutes: int = 60 * 24,
        limit: int = 20,
    ) -> list[dict[str, Any]]:
        """Find traces by Centaur thread key, execution id, or Laminar session id."""
        if not any((thread_key, execution_id, session_id)):
            raise ValueError("Provide at least one of thread_key, execution_id, or session_id")

        minutes = _clamp_int(minutes, minimum=1, maximum=MAX_MINUTES)
        limit = _clamp_int(limit, minimum=1, maximum=MAX_LIMIT)
        filters = [f"s.start_time >= now() - INTERVAL {minutes} MINUTE"]
        parameters: dict[str, Any] = {}
        if thread_key:
            filters.append(
                "JSONExtractString(s.attributes, 'centaur.thread_key') = {thread_key:String}"
            )
            parameters["thread_key"] = thread_key
        if execution_id:
            filters.append(
                "JSONExtractString(s.attributes, 'centaur.execution_id') = {execution_id:String}"
            )
            parameters["execution_id"] = execution_id
        if session_id:
            filters.append("t.session_id = {session_id:String}")
            parameters["session_id"] = session_id

        return self._query(
            f"""
            SELECT
                s.trace_id,
                min(s.start_time) AS start_time,
                max(s.end_time) AS end_time,
                dateDiff('millisecond', min(s.start_time), max(s.end_time)) AS duration_ms,
                count() AS span_count,
                groupUniqArray(s.name) AS span_names,
                groupUniqArray(JSONExtractString(s.attributes, 'service.name')) AS services,
                any(JSONExtractString(s.attributes, 'centaur.thread_key')) AS thread_key,
                any(JSONExtractString(s.attributes, 'centaur.execution_id')) AS execution_id,
                any(t.session_id) AS session_id
            FROM spans AS s
            LEFT JOIN traces AS t ON s.trace_id = t.id
            WHERE {" AND ".join(filters)}
            GROUP BY s.trace_id
            ORDER BY start_time DESC
            LIMIT {limit}
            """,
            parameters,
        )

    def get_trace(
        self,
        trace_id: str,
        include_io: bool = False,
        input_output_chars: int = 1000,
        limit: int = 300,
    ) -> dict[str, Any]:
        """Return span-level detail for one trace id."""
        limit = _clamp_int(limit, minimum=1, maximum=MAX_LIMIT)
        input_output_chars = _clamp_int(input_output_chars, minimum=0, maximum=20000)
        rows = self._query(
            f"""
            SELECT
                span_id,
                parent_span_id,
                name,
                span_type,
                start_time,
                end_time,
                dateDiff('millisecond', start_time, end_time) AS duration_ms,
                status,
                model,
                provider,
                input_tokens,
                output_tokens,
                total_tokens,
                total_cost,
                attributes,
                input,
                output
            FROM spans
            WHERE trace_id = {{trace_id:UUID}}
            ORDER BY start_time ASC
            LIMIT {limit}
            """,
            {"trace_id": trace_id},
        )

        spans: list[dict[str, Any]] = []
        for row in rows:
            attributes = _safe_json(row.pop("attributes", ""))
            span = {
                **{k: v for k, v in row.items() if k not in ("input", "output")},
                "attributes": _interesting_attributes(attributes),
            }
            if include_io:
                span["input"] = _truncate(str(row.get("input") or ""), input_output_chars)
                span["output"] = _truncate(str(row.get("output") or ""), input_output_chars)
            spans.append(span)

        return {
            "trace_id": trace_id,
            "span_count": len(spans),
            "trace_url": f"{self.external_url}/traces/{trace_id}",
            "spans": spans,
        }

    def diagnose_thread(
        self,
        thread_key: str,
        minutes: int = 60 * 24,
        trace_limit: int = 10,
        slow_span_limit: int = 20,
    ) -> dict[str, Any]:
        """Summarize recent Laminar evidence for one Centaur thread."""
        minutes = _clamp_int(minutes, minimum=1, maximum=MAX_MINUTES)
        trace_limit = _clamp_int(trace_limit, minimum=1, maximum=100)
        slow_span_limit = _clamp_int(slow_span_limit, minimum=1, maximum=100)
        traces = self.find_traces(thread_key=thread_key, minutes=minutes, limit=trace_limit)
        errors = self._query(
            f"""
            SELECT
                trace_id,
                span_id,
                name,
                start_time,
                status,
                JSONExtractString(attributes, 'exception.message') AS exception_message,
                JSONExtractString(attributes, 'http.response.status_code') AS http_status
            FROM spans
            WHERE start_time >= now() - INTERVAL {minutes} MINUTE
              AND JSONExtractString(attributes, 'centaur.thread_key') = {{thread_key:String}}
              AND (
                lower(status) IN ('error', 'failed')
                OR JSONExtractString(attributes, 'exception.message') != ''
                OR toInt32OrZero(JSONExtractString(attributes, 'http.response.status_code')) >= 500
              )
            ORDER BY start_time DESC
            LIMIT 100
            """,
            {"thread_key": thread_key},
        )
        slow_spans = self._query(
            f"""
            SELECT
                trace_id,
                span_id,
                name,
                start_time,
                dateDiff('millisecond', start_time, end_time) AS duration_ms,
                status
            FROM spans
            WHERE start_time >= now() - INTERVAL {minutes} MINUTE
              AND JSONExtractString(attributes, 'centaur.thread_key') = {{thread_key:String}}
            ORDER BY duration_ms DESC
            LIMIT {slow_span_limit}
            """,
            {"thread_key": thread_key},
        )
        return {
            "thread_key": thread_key,
            "minutes": minutes,
            "trace_count": len(traces),
            "traces": traces,
            "error_count": len(errors),
            "errors": errors,
            "slow_spans": slow_spans,
        }

    def errors(self, minutes: int = 60, limit: int = 50) -> list[dict[str, Any]]:
        """List recent failed/error spans across the Laminar project."""
        minutes = _clamp_int(minutes, minimum=1, maximum=MAX_MINUTES)
        limit = _clamp_int(limit, minimum=1, maximum=MAX_LIMIT)
        return self._query(
            f"""
            SELECT
                trace_id,
                span_id,
                name,
                start_time,
                status,
                JSONExtractString(attributes, 'centaur.thread_key') AS thread_key,
                JSONExtractString(attributes, 'centaur.execution_id') AS execution_id,
                JSONExtractString(attributes, 'service.name') AS service_name,
                JSONExtractString(attributes, 'exception.message') AS exception_message,
                JSONExtractString(attributes, 'http.response.status_code') AS http_status
            FROM spans
            WHERE start_time >= now() - INTERVAL {minutes} MINUTE
              AND (
                lower(status) IN ('error', 'failed')
                OR JSONExtractString(attributes, 'exception.message') != ''
                OR toInt32OrZero(JSONExtractString(attributes, 'http.response.status_code')) >= 500
              )
            ORDER BY start_time DESC
            LIMIT {limit}
            """
        )

    def close(self) -> None:
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self) -> LaminarClient:
        return self

    def __exit__(self, *args: object) -> None:
        self.close()


def _client() -> LaminarClient:
    return LaminarClient()
