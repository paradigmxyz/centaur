from __future__ import annotations

import json
from datetime import datetime
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Query, Request

from api.deps import verify_operator_api_key

router = APIRouter(
    prefix="/admin/monitoring",
    dependencies=[Depends(verify_operator_api_key)],
)

WINDOW_SECONDS = {
    "24h": 24 * 60 * 60,
    "7d": 7 * 24 * 60 * 60,
    "30d": 30 * 24 * 60 * 60,
}
MAX_LIMIT = 200


def _window_seconds(window: str) -> int:
    try:
        return WINDOW_SECONDS[window]
    except KeyError:
        raise HTTPException(
            status_code=400,
            detail=f"Unsupported window '{window}'. Use one of: {', '.join(WINDOW_SECONDS)}",
        )


def _coerce_json(value: Any) -> Any:
    if isinstance(value, str):
        try:
            return json.loads(value)
        except json.JSONDecodeError:
            return value
    return value


def _as_dict(value: Any) -> dict[str, Any]:
    value = _coerce_json(value)
    return value if isinstance(value, dict) else {}


def _as_list(value: Any) -> list[Any]:
    value = _coerce_json(value)
    return value if isinstance(value, list) else []


def _iso(value: Any) -> str | None:
    if isinstance(value, datetime):
        return value.isoformat()
    return str(value) if value is not None else None


def _int(value: Any) -> int:
    try:
        return int(value or 0)
    except (TypeError, ValueError):
        return 0


def _float(value: Any) -> float:
    try:
        return float(value or 0)
    except (TypeError, ValueError):
        return 0.0


def _limit_offset(limit: int, offset: int) -> tuple[int, int]:
    return min(max(limit, 1), MAX_LIMIT), max(offset, 0)


@router.get("/overview")
async def overview(request: Request, window: str = Query("24h")) -> dict[str, Any]:
    pool = request.app.state.db_pool
    seconds = _window_seconds(window)

    live = await pool.fetchrow(
        """
        SELECT
          (
            SELECT COUNT(*) FROM sandbox_sessions
            WHERE state IN ('running', 'idle', 'delivering', 'suspended')
          ) AS active_sessions,
          (
            SELECT COUNT(*) FROM agent_execution_requests
            WHERE status = 'queued'
          ) AS queued_executions,
          (
            SELECT COUNT(*) FROM agent_execution_requests
            WHERE status = 'running'
          ) AS running_executions,
          (
            SELECT COUNT(*) FROM agent_execution_requests
            WHERE status = 'retry_wait'
          ) AS retry_wait_executions,
          (
            SELECT COUNT(*) FROM agent_final_delivery_outbox
            WHERE state NOT IN ('delivered')
          ) AS pending_deliveries
        """
    )
    totals = await pool.fetchrow(
        """
        SELECT
          COUNT(*) AS executions,
          COUNT(*) FILTER (WHERE event_json->>'status' = 'completed') AS completed,
          COUNT(*) FILTER (WHERE event_json->>'status' IS DISTINCT FROM 'completed') AS failed,
          COALESCE(SUM((event_json->>'total_tokens')::numeric), 0) AS total_tokens,
          COALESCE(SUM((event_json->>'cost_usd')::numeric), 0) AS cost_usd,
          COALESCE(SUM((event_json->>'assistant_tool_use_events')::numeric), 0) AS tool_calls,
          COALESCE(SUM((event_json->>'tool_error_events')::numeric), 0) AS tool_errors
        FROM agent_execution_events
        WHERE event_kind = 'execution_summary'
          AND created_at >= NOW() - make_interval(secs => $1::double precision)
        """,
        float(seconds),
    )
    recent = await pool.fetch(
        """
        SELECT
          date_trunc('hour', created_at) AS bucket,
          COUNT(*) AS executions,
          COUNT(*) FILTER (WHERE event_json->>'status' = 'completed') AS completed,
          COUNT(*) FILTER (WHERE event_json->>'status' IS DISTINCT FROM 'completed') AS failed,
          COALESCE(SUM((event_json->>'cost_usd')::numeric), 0) AS cost_usd
        FROM agent_execution_events
        WHERE event_kind = 'execution_summary'
          AND created_at >= NOW() - make_interval(secs => $1::double precision)
        GROUP BY bucket
        ORDER BY bucket ASC
        """,
        float(seconds),
    )
    top_tools = await pool.fetch(
        """
        SELECT event_json->>'tool_name' AS tool_name, COUNT(*) AS calls
        FROM agent_execution_events
        WHERE event_kind = 'assistant_tool_use_observed'
          AND created_at >= NOW() - make_interval(secs => $1::double precision)
          AND COALESCE(event_json->>'tool_name', '') <> ''
        GROUP BY tool_name
        ORDER BY calls DESC, tool_name ASC
        LIMIT 10
        """,
        float(seconds),
    )

    return {
        "window": window,
        "live": dict(live or {}),
        "totals": {
            "executions": _int(totals["executions"] if totals else 0),
            "completed": _int(totals["completed"] if totals else 0),
            "failed": _int(totals["failed"] if totals else 0),
            "total_tokens": _int(totals["total_tokens"] if totals else 0),
            "cost_usd": round(_float(totals["cost_usd"] if totals else 0), 6),
            "tool_calls": _int(totals["tool_calls"] if totals else 0),
            "tool_errors": _int(totals["tool_errors"] if totals else 0),
        },
        "series": [
            {
                "bucket": _iso(row["bucket"]),
                "executions": _int(row["executions"]),
                "completed": _int(row["completed"]),
                "failed": _int(row["failed"]),
                "cost_usd": round(_float(row["cost_usd"]), 6),
            }
            for row in recent
        ],
        "top_tools": [
            {"tool_name": row["tool_name"], "calls": _int(row["calls"])}
            for row in top_tools
        ],
    }


@router.get("/leaderboard")
async def leaderboard(
    request: Request,
    window: str = Query("7d"),
    limit: int = Query(50, ge=1),
) -> dict[str, Any]:
    pool = request.app.state.db_pool
    seconds = _window_seconds(window)
    limit, _ = _limit_offset(limit, 0)
    rows = await pool.fetch(
        """
        SELECT
          COALESCE(NULLIF(event_json->>'user_id', ''), 'unknown') AS user_id,
          COUNT(*) AS executions,
          COUNT(*) FILTER (WHERE event_json->>'status' = 'completed') AS completed,
          COUNT(*) FILTER (WHERE event_json->>'status' IS DISTINCT FROM 'completed') AS failed,
          COALESCE(SUM((event_json->>'total_tokens')::numeric), 0) AS total_tokens,
          COALESCE(SUM((event_json->>'cost_usd')::numeric), 0) AS cost_usd,
          COALESCE(SUM((event_json->>'assistant_tool_use_events')::numeric), 0) AS tool_calls,
          COALESCE(SUM((event_json->>'tool_error_events')::numeric), 0) AS tool_errors,
          MAX(created_at) AS last_activity_at
        FROM agent_execution_events
        WHERE event_kind = 'execution_summary'
          AND created_at >= NOW() - make_interval(secs => $1::double precision)
        GROUP BY user_id
        ORDER BY executions DESC, total_tokens DESC, user_id ASC
        LIMIT $2
        """,
        float(seconds),
        limit,
    )
    return {
        "window": window,
        "items": [
            {
                "user_id": row["user_id"],
                "executions": _int(row["executions"]),
                "completed": _int(row["completed"]),
                "failed": _int(row["failed"]),
                "total_tokens": _int(row["total_tokens"]),
                "cost_usd": round(_float(row["cost_usd"]), 6),
                "tool_calls": _int(row["tool_calls"]),
                "tool_errors": _int(row["tool_errors"]),
                "last_activity_at": _iso(row["last_activity_at"]),
            }
            for row in rows
        ],
    }


@router.get("/executions")
async def executions(
    request: Request,
    window: str = Query("7d"),
    limit: int = Query(50, ge=1),
    offset: int = Query(0, ge=0),
    status: str | None = None,
    user_id: str | None = None,
) -> dict[str, Any]:
    pool = request.app.state.db_pool
    seconds = _window_seconds(window)
    limit, offset = _limit_offset(limit, offset)
    rows = await pool.fetch(
        """
        SELECT
          ee.event_id,
          ee.thread_key,
          ee.execution_id,
          ee.created_at,
          ee.event_json,
          aer.created_at AS requested_at,
          aer.started_at,
          aer.completed_at,
          aer.status AS request_status,
          aer.terminal_reason
        FROM agent_execution_events ee
        LEFT JOIN agent_execution_requests aer ON aer.execution_id = ee.execution_id
        WHERE ee.event_kind = 'execution_summary'
          AND ee.created_at >= NOW() - make_interval(secs => $1::double precision)
          AND ($2::text IS NULL OR ee.event_json->>'status' = $2)
          AND ($3::text IS NULL OR ee.event_json->>'user_id' = $3)
        ORDER BY ee.created_at DESC, ee.event_id DESC
        LIMIT $4 OFFSET $5
        """,
        float(seconds),
        status,
        user_id,
        limit,
        offset,
    )
    return {
        "window": window,
        "limit": limit,
        "offset": offset,
        "items": [_execution_item(row) for row in rows],
    }


def _execution_item(row: Any) -> dict[str, Any]:
    payload = _as_dict(row["event_json"])
    return {
        "event_id": _int(row["event_id"]),
        "execution_id": row["execution_id"],
        "thread_key": row["thread_key"],
        "created_at": _iso(row["created_at"]),
        "requested_at": _iso(row["requested_at"]),
        "started_at": _iso(row["started_at"]),
        "completed_at": _iso(row["completed_at"]),
        "status": payload.get("status") or row["request_status"],
        "terminal_reason": payload.get("terminal_reason") or row["terminal_reason"],
        "harness": payload.get("harness"),
        "engine": payload.get("engine"),
        "persona_id": payload.get("persona_id"),
        "user_id": payload.get("user_id"),
        "duration_s": payload.get("duration_s"),
        "ttft_ms": payload.get("ttft_ms"),
        "total_tokens": _int(payload.get("total_tokens")),
        "cost_usd": round(_float(payload.get("cost_usd")), 6),
        "tool_calls": _int(payload.get("assistant_tool_use_events")),
        "tool_errors": _int(payload.get("tool_error_events")),
        "models": _as_list(payload.get("models")),
        "tool_calls_by_name": _as_dict(payload.get("tool_calls_by_name")),
        "tool_errors_by_name": _as_dict(payload.get("tool_errors_by_name")),
        "command_events": _int(payload.get("command_events")),
        "file_change_events": _int(payload.get("file_change_events")),
        "subagent_events": _int(payload.get("subagent_events")),
    }


@router.get("/tool-events")
async def tool_events(
    request: Request,
    window: str = Query("7d"),
    limit: int = Query(50, ge=1),
    offset: int = Query(0, ge=0),
    tool_name: str | None = None,
    user_id: str | None = None,
    errors_only: bool = False,
) -> dict[str, Any]:
    pool = request.app.state.db_pool
    seconds = _window_seconds(window)
    limit, offset = _limit_offset(limit, offset)
    rows = await pool.fetch(
        """
        WITH uses AS (
          SELECT event_id, thread_key, execution_id, created_at, event_json
          FROM agent_execution_events
          WHERE event_kind = 'assistant_tool_use_observed'
            AND created_at >= NOW() - make_interval(secs => $1::double precision)
            AND ($2::text IS NULL OR event_json->>'tool_name' = $2)
        ),
        results AS (
          SELECT DISTINCT ON (execution_id, event_json->>'tool_use_id')
            execution_id,
            event_json->>'tool_use_id' AS tool_use_id,
            event_id,
            created_at,
            event_json
          FROM agent_execution_events
          WHERE event_kind = 'tool_result_observed'
            AND created_at >= NOW() - make_interval(secs => $1::double precision)
          ORDER BY execution_id, event_json->>'tool_use_id', event_id ASC
        ),
        summaries AS (
          SELECT DISTINCT ON (execution_id)
            execution_id,
            event_json
          FROM agent_execution_events
          WHERE event_kind = 'execution_summary'
          ORDER BY execution_id, event_id DESC
        )
        SELECT
          uses.event_id AS use_event_id,
          uses.thread_key,
          uses.execution_id,
          uses.created_at AS use_created_at,
          uses.event_json AS use_json,
          results.event_id AS result_event_id,
          results.created_at AS result_created_at,
          results.event_json AS result_json,
          summaries.event_json AS summary_json
        FROM uses
        LEFT JOIN results
          ON results.execution_id = uses.execution_id
         AND results.tool_use_id = uses.event_json->>'tool_use_id'
        LEFT JOIN summaries ON summaries.execution_id = uses.execution_id
        WHERE ($3::text IS NULL OR summaries.event_json->>'user_id' = $3)
          AND (
            $4::boolean IS FALSE
            OR COALESCE((results.event_json->>'is_error')::boolean, FALSE) IS TRUE
          )
        ORDER BY uses.created_at DESC, uses.event_id DESC
        LIMIT $5 OFFSET $6
        """,
        float(seconds),
        tool_name,
        user_id,
        errors_only,
        limit,
        offset,
    )
    return {
        "window": window,
        "limit": limit,
        "offset": offset,
        "items": [_tool_event_item(row) for row in rows],
    }


def _tool_event_item(row: Any) -> dict[str, Any]:
    use = _as_dict(row["use_json"])
    result = _as_dict(row["result_json"])
    summary = _as_dict(row["summary_json"])
    return {
        "use_event_id": _int(row["use_event_id"]),
        "result_event_id": _int(row["result_event_id"]),
        "thread_key": row["thread_key"],
        "execution_id": row["execution_id"],
        "user_id": summary.get("user_id"),
        "harness": use.get("harness") or summary.get("harness"),
        "engine": use.get("engine") or summary.get("engine"),
        "persona_id": use.get("persona_id") or summary.get("persona_id"),
        "tool_use_id": use.get("tool_use_id"),
        "tool_name": use.get("tool_name"),
        "input_keys": _as_list(use.get("input_keys")),
        "input_size_bytes": _int(use.get("input_size_bytes")),
        "use_created_at": _iso(row["use_created_at"]),
        "result_created_at": _iso(row["result_created_at"]),
        "result_status": (
            "error"
            if result.get("is_error") is True
            else "success"
            if result
            else "pending"
        ),
        "error_category": result.get("error_category"),
        "content_size_bytes": _int(result.get("content_size_bytes")),
    }


@router.get("/threads/{thread_key}")
async def thread_detail(request: Request, thread_key: str) -> dict[str, Any]:
    pool = request.app.state.db_pool
    session = await pool.fetchrow(
        """
        SELECT thread_key, sandbox_id, harness, engine, state, thread_name, started_at, updated_at,
               wire_connected_at, wire_last_seen_at, agent_thread_id
        FROM sandbox_sessions
        WHERE thread_key = $1
        """,
        thread_key,
    )
    messages = await pool.fetchrow(
        """
        SELECT
          COUNT(*) AS message_count,
          COUNT(*) FILTER (WHERE role = 'user') AS user_messages,
          COUNT(*) FILTER (WHERE role = 'assistant') AS assistant_messages,
          COUNT(*) FILTER (WHERE jsonb_array_length(parts) > 0) AS messages_with_parts,
          MIN(created_at) AS first_message_at,
          MAX(created_at) AS last_message_at
        FROM chat_messages
        WHERE thread_key = $1
        """,
        thread_key,
    )
    attachments = await pool.fetchval(
        "SELECT COUNT(*) FROM attachments WHERE thread_key = $1",
        thread_key,
    )
    execution_rows = await pool.fetch(
        """
        SELECT ee.event_id, ee.thread_key, ee.execution_id, ee.created_at, ee.event_json,
               aer.created_at AS requested_at, aer.started_at, aer.completed_at,
               aer.status AS request_status, aer.terminal_reason
        FROM agent_execution_events ee
        LEFT JOIN agent_execution_requests aer ON aer.execution_id = ee.execution_id
        WHERE ee.thread_key = $1 AND ee.event_kind = 'execution_summary'
        ORDER BY ee.created_at DESC, ee.event_id DESC
        LIMIT 50
        """,
        thread_key,
    )
    timeline_rows = await pool.fetch(
        """
        SELECT event_id, execution_id, event_kind, event_json, created_at
        FROM agent_execution_events
        WHERE thread_key = $1
          AND event_kind IN (
            'execution_started',
            'assistant_tool_use_observed',
            'tool_result_observed',
            'usage_observed',
            'command_execution_observed',
            'file_change_observed',
            'error_observed',
            'execution_summary'
          )
        ORDER BY event_id DESC
        LIMIT 200
        """,
        thread_key,
    )

    return {
        "thread_key": thread_key,
        "session": _session_item(session) if session else None,
        "messages": {
            "message_count": _int(messages["message_count"] if messages else 0),
            "user_messages": _int(messages["user_messages"] if messages else 0),
            "assistant_messages": _int(messages["assistant_messages"] if messages else 0),
            "messages_with_parts": _int(messages["messages_with_parts"] if messages else 0),
            "first_message_at": _iso(messages["first_message_at"] if messages else None),
            "last_message_at": _iso(messages["last_message_at"] if messages else None),
            "attachment_count": _int(attachments),
        },
        "executions": [_execution_item(row) for row in execution_rows],
        "timeline": [_timeline_item(row) for row in timeline_rows],
    }


def _session_item(row: Any) -> dict[str, Any]:
    return {
        "thread_key": row["thread_key"],
        "sandbox_id": row["sandbox_id"],
        "harness": row["harness"],
        "engine": row["engine"],
        "state": row["state"],
        "thread_name": row["thread_name"],
        "agent_thread_id": row["agent_thread_id"],
        "started_at": _iso(row["started_at"]),
        "updated_at": _iso(row["updated_at"]),
        "wire_connected_at": _iso(row["wire_connected_at"]),
        "wire_last_seen_at": _iso(row["wire_last_seen_at"]),
    }


def _timeline_item(row: Any) -> dict[str, Any]:
    payload = _as_dict(row["event_json"])
    event_kind = row["event_kind"]
    base = {
        "event_id": _int(row["event_id"]),
        "execution_id": row["execution_id"],
        "event_kind": event_kind,
        "created_at": _iso(row["created_at"]),
    }
    if event_kind == "assistant_tool_use_observed":
        base.update(
            {
                "tool_name": payload.get("tool_name"),
                "input_keys": _as_list(payload.get("input_keys")),
                "input_size_bytes": _int(payload.get("input_size_bytes")),
            }
        )
    elif event_kind == "tool_result_observed":
        base.update(
            {
                "tool_use_id": payload.get("tool_use_id"),
                "result_status": "error" if payload.get("is_error") else "success",
                "error_category": payload.get("error_category"),
                "content_size_bytes": _int(payload.get("content_size_bytes")),
            }
        )
    elif event_kind == "usage_observed":
        base.update(
            {
                "model": payload.get("model"),
                "total_tokens": _int(payload.get("total_tokens")),
                "cost_usd": round(_float(payload.get("cost_usd")), 6),
            }
        )
    elif event_kind == "execution_summary":
        base.update(
            {
                "status": payload.get("status"),
                "terminal_reason": payload.get("terminal_reason"),
                "total_tokens": _int(payload.get("total_tokens")),
                "cost_usd": round(_float(payload.get("cost_usd")), 6),
                "tool_calls": _int(payload.get("assistant_tool_use_events")),
                "tool_errors": _int(payload.get("tool_error_events")),
            }
        )
    else:
        base.update({"summary": _summarize_payload(payload)})
    return base


def _summarize_payload(payload: dict[str, Any]) -> dict[str, Any]:
    allowed = (
        "type",
        "status",
        "terminal_reason",
        "harness",
        "engine",
        "persona_id",
        "command_size_bytes",
        "output_size_bytes",
        "exit_code",
        "change_count",
        "error_chars",
    )
    return {key: payload[key] for key in allowed if key in payload}
