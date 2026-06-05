"""Pipe agent — spawn sandboxes, pipe stdin/stdout, Postgres-backed sessions.

Thin orchestration layer: one sandbox per thread_key, raw NDJSON streaming.
Session mapping lives in Postgres (sandbox_sessions table). Process-local
runtime state (stream handles, turn bookkeeping) stays in-memory keyed
by sandbox_id.

Streaming architecture (2 layers, 0 queues, 0 threads):
  sandbox stdout (async iterator from the active backend)
    → stream_connect (persistent SSE wire: DB ops + turn detection + yields SSE dicts)
      → EventSourceResponse (SSE formatting + keepalive via sse-starlette)
  stdin written via inject_stdin (flush pending messages + write, returns JSON).
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import re
import time
import uuid
from collections.abc import AsyncIterator
from typing import Any

import structlog

from api.sandbox.base import RuntimeState, SandboxSession
from api.sandbox.harness_protocol import (
    build_user_input,
    extract_result,
    extract_thread_id,
    is_turn_done,
    messages_to_content_blocks,
)
from api.deps import mint_sandbox_token
from api.harness_config import default_harness
from api.sandbox.normalize import normalize_harness_event
from api.sandbox.registry import get_backend
from api.trace_context import get_or_create_thread_trace_id

log = structlog.get_logger()

_GITHUB_HANDLE_RE = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$")
_GITHUB_URL_RE = re.compile(
    r"(?:https?://)?github\.com/([A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?)",
    re.IGNORECASE,
)
_GITHUB_LABEL_RE = re.compile(r"\bgithub\b", re.IGNORECASE)
_GITHUB_PREFIX_RE = re.compile(
    r"\bgithub\b\s*(?:username|user|handle|profile)?\s*[:/@-]?\s*@?([A-Za-z0-9][A-Za-z0-9-]{0,38})",
    re.IGNORECASE,
)

_VALID_STDOUT_EVENT_TYPES = frozenset(
    {
        "amp_raw_event",
        "assistant",
        "command_execution",
        "content_block_delta",
        "content_block_start",
        "content_block_stop",
        "error",
        "file_change",
        "message_delta",
        "message_start",
        "message_stop",
        "item.agentMessage.delta",
        "item.commandExecution.outputDelta",
        "item.completed",
        "item.fileChange.outputDelta",
        "item.fileChange.patchUpdated",
        "item.plan.delta",
        "item.reasoning.summaryPartAdded",
        "item.reasoning.summaryTextDelta",
        "item.reasoning.textDelta",
        "item.started",
        "item.updated",
        "reasoning",
        "result",
        "status",
        "subagent",
        "system",
        "thread.goal.cleared",
        "thread.goal.updated",
        "thread.started",
        "tool",
        "tool_result",
        "tool_use",
        "turn.done",
        "turn.completed",
        "turn.failed",
        "turn.plan.updated",
        "turn.started",
        "usage",
        "user",
    }
)

_ENGINE_HARNESSES = {"amp", "claude-code", "codex", "pi-mono"}
_REUSABLE_DB_STATES = {"running", "idle", "delivering", "error", "suspended"}

IDLE_TTL_S = int(os.getenv("IDLE_TTL_S", "86400"))  # 24 hours
SUSPENDED_RETENTION_S = int(os.getenv("SUSPENDED_RETENTION_S", str(7 * 24 * 60 * 60)))
MAX_ACTIVE_SANDBOX_SESSIONS = int(os.getenv("MAX_ACTIVE_SANDBOX_SESSIONS", "45"))
STREAM_EOF_REATTACH_MAX = int(os.getenv("STREAM_EOF_REATTACH_MAX", "6"))
STREAM_EOF_REATTACH_BACKOFF_S = float(os.getenv("STREAM_EOF_REATTACH_BACKOFF_S", "1.0"))

# ── Process-local runtime state (ephemeral: stream handles, turn counters) ───

_runtime: dict[str, RuntimeState] = {}


def _get_runtime(sandbox_id: str) -> RuntimeState:
    """Get or create process-local runtime state for a sandbox."""
    if sandbox_id not in _runtime:
        _runtime[sandbox_id] = RuntimeState()
    return _runtime[sandbox_id]


def _drop_runtime(sandbox_id: str) -> None:
    """Remove process-local runtime state for a sandbox."""
    _runtime.pop(sandbox_id, None)


def _elapsed_since(start_s: float) -> float:
    """Return a non-negative elapsed duration for logging.

    Most callers pass a monotonic start time, but we defensively fall back to
    wall-clock time if an epoch timestamp is passed through an older code path.
    """
    elapsed_s = time.monotonic() - start_s
    if elapsed_s >= 0:
        return round(elapsed_s, 2)
    return round(max(time.time() - start_s, 0.0), 2)


def _turn_input_metrics(turn_input: dict[str, Any]) -> dict[str, Any]:
    message = turn_input.get("message") if isinstance(turn_input, dict) else None
    content = message.get("content") if isinstance(message, dict) else None
    if not isinstance(content, list):
        return {
            "input_block_count": 0,
            "input_text_chars": 0,
            "input_attachment_refs": 0,
        }
    text_chars = 0
    attachment_refs = 0
    for block in content:
        if not isinstance(block, dict):
            continue
        if block.get("type") == "text":
            text_chars += len(
                block.get("text", "") if isinstance(block.get("text"), str) else ""
            )
        if block.get("type") == "attachment_ref":
            attachment_refs += 1
    return {
        "input_block_count": len(content),
        "input_text_chars": text_chars,
        "input_attachment_refs": attachment_refs,
    }


# ── DB pool access ───────────────────────────────────────────────────────────


def _get_pool():
    """Get the asyncpg pool from the FastAPI app state."""
    from api.app import app

    return app.state.db_pool


# ── DB helpers (async) ───────────────────────────────────────────────────────


def _coerce_json_object(value: Any) -> dict[str, Any] | None:
    """Best-effort decode for json/jsonb values returned as text by asyncpg.

    We accept already-decoded dicts and decode JSON strings. Some older rows may
    contain a double-encoded JSON string; decode one extra layer to recover.
    """
    current: Any = value
    for _ in range(2):
        if isinstance(current, dict):
            return current
        if not isinstance(current, str):
            return None
        try:
            current = json.loads(current)
        except (json.JSONDecodeError, TypeError):
            return None
    return current if isinstance(current, dict) else None


async def _db_get_session(thread_key: str) -> SandboxSession | None:
    """Load a session from the DB. Returns None if not found."""
    pool = _get_pool()
    row = await pool.fetchrow(
        "SELECT thread_key, sandbox_id, harness, engine, state, started_at, "
        "agent_thread_id, last_delivered_id, inflight_turn_id, inflight_turn_input, "
        "inflight_attempts, last_result, trace_id "
        "FROM sandbox_sessions WHERE thread_key = $1",
        thread_key,
    )
    if row is None:
        return None
    session = SandboxSession(
        sandbox_id=row["sandbox_id"],
        thread_key=row["thread_key"],
        harness=row["harness"],
        engine=row["engine"],
        started_at=row["started_at"].timestamp() if row["started_at"] else 0.0,
        backend_name="kubernetes",
        db_state=row["state"],
        agent_thread_id=row["agent_thread_id"] or "",
        last_delivered_id=row["last_delivered_id"] or "",
        inflight_turn_id=row["inflight_turn_id"] or "",
        inflight_turn_input=_coerce_json_object(row["inflight_turn_input"]),
        inflight_attempts=int(row["inflight_attempts"] or 0),
        last_result=row["last_result"] or "",
        trace_id=str(row["trace_id"] or ""),
    )
    rt = _get_runtime(session.sandbox_id)
    if session.inflight_turn_id and rt.turn_counter == 0:
        rt.turn_counter = 1
    if session.last_result and rt.last_result is None:
        rt.last_result = session.last_result
    return session


async def _db_insert_session(
    session: SandboxSession,
    *,
    harness: str,
    engine: str,
    agent_thread_id: str = "",
    last_delivered_id: str = "",
    inflight_turn_id: str = "",
    inflight_turn_input: dict | None = None,
    inflight_attempts: int = 0,
    last_result: str = "",
) -> bool:
    """Insert a session row. Returns True if we won the insert race."""
    pool = _get_pool()
    # Sandbox session state tracks whether a turn is active, not just whether
    # the container process exists. Fresh spawns with no in-flight turn should
    # enter the normal idle TTL path.
    initial_state = "running" if inflight_turn_id else "idle"
    thread_trace_id = await get_or_create_thread_trace_id(pool, session.thread_key)
    trace_id = session.trace_id or thread_trace_id or str(uuid.uuid4())
    session.trace_id = trace_id
    row = await pool.fetchrow(
        "INSERT INTO sandbox_sessions ("
        "thread_key, sandbox_id, harness, engine, state, started_at, "
        "agent_thread_id, last_delivered_id, inflight_turn_id, inflight_turn_input, "
        "inflight_started_at, inflight_attempts, last_result, last_result_at, trace_id"
        ") VALUES ($1, $2, $3, $4, $5, NOW(), $6, $7, $8::text, $9::jsonb, "
        "CASE WHEN $8::text IS NULL THEN NULL ELSE NOW() END, $10, $11, "
        "CASE WHEN $11::text = '' THEN NULL ELSE NOW() END, $12::uuid) "
        "ON CONFLICT (thread_key) DO NOTHING "
        "RETURNING thread_key",
        session.thread_key,
        session.sandbox_id,
        harness,
        engine,
        initial_state,
        agent_thread_id or None,
        last_delivered_id or None,
        inflight_turn_id or None,
        json.dumps(inflight_turn_input) if inflight_turn_input is not None else None,
        max(0, inflight_attempts),
        last_result,
        trace_id,
    )
    return row is not None


async def _db_set_inflight_turn(
    thread_key: str,
    turn_id: str,
    turn_input: dict,
    *,
    attempts: int,
) -> None:
    """Persist the active turn payload for restart-safe replay."""
    pool = _get_pool()
    await pool.execute(
        "UPDATE sandbox_sessions SET inflight_turn_id = $1, inflight_turn_input = $2::jsonb, "
        "inflight_started_at = NOW(), inflight_attempts = $3, state = 'running', "
        "last_result = NULL, last_result_at = NULL, updated_at = NOW() "
        "WHERE thread_key = $4",
        turn_id,
        json.dumps(turn_input),
        max(1, attempts),
        thread_key,
    )


async def _db_complete_inflight_turn(thread_key: str, result_text: str) -> None:
    """Mark the active turn complete and persist the final result."""
    pool = _get_pool()
    await pool.execute(
        "UPDATE sandbox_sessions SET state = 'idle', inflight_turn_id = NULL, inflight_turn_input = NULL, "
        "inflight_started_at = NULL, inflight_attempts = 0, last_result = $1, last_result_at = NOW(), "
        "updated_at = NOW() WHERE thread_key = $2",
        result_text,
        thread_key,
    )


async def _db_get_inflight_turn(thread_key: str) -> tuple[str, dict, int] | None:
    """Return in-flight turn payload (id, input, attempts) or None."""
    pool = _get_pool()
    row = await pool.fetchrow(
        "SELECT inflight_turn_id, inflight_turn_input, inflight_attempts "
        "FROM sandbox_sessions WHERE thread_key = $1",
        thread_key,
    )
    if row is None:
        return None
    turn_id = row["inflight_turn_id"] or ""
    turn_input = _coerce_json_object(row["inflight_turn_input"])
    if not turn_id or turn_input is None:
        return None
    return turn_id, turn_input, int(row["inflight_attempts"] or 0)


async def _db_update_state(thread_key: str, state: str) -> None:
    """Update the state of a session in the DB."""
    pool = _get_pool()
    await pool.execute(
        "UPDATE sandbox_sessions SET state = $1, updated_at = NOW() WHERE thread_key = $2",
        state,
        thread_key,
    )


async def _db_delete_session(thread_key: str) -> None:
    """Delete a session row from the DB."""
    pool = _get_pool()
    await pool.execute("DELETE FROM sandbox_sessions WHERE thread_key = $1", thread_key)


async def _evict_idle_sessions_for_capacity(backend) -> int:
    """Keep enough pod headroom for a new sandbox + per-sandbox proxy.

    Local Kubernetes nodes have a hard pod count limit. Slack threads can leave
    many idle runtimes pinned for fast follow-ups; once the node reaches that
    limit, new proxy pods stay Pending and spawn fails after the readiness
    timeout. Evict the oldest idle sessions before cold-spawning a new runtime.
    """
    if MAX_ACTIVE_SANDBOX_SESSIONS <= 0:
        return 0

    pool = _get_pool()
    active_count = await pool.fetchval(
        "SELECT COUNT(*) FROM sandbox_sessions "
        "WHERE state IN ('running', 'idle', 'delivering', 'error')"
    )
    overage = int(active_count or 0) - MAX_ACTIVE_SANDBOX_SESSIONS + 1
    if overage <= 0:
        return 0

    rows = await pool.fetch(
        "SELECT ss.thread_key, ss.sandbox_id FROM sandbox_sessions ss "
        "WHERE ss.state = 'idle' "
        "AND NOT EXISTS ("
        "  SELECT 1 FROM agent_execution_requests er "
        "  WHERE er.thread_key = ss.thread_key "
        "    AND er.status IN ('queued', 'running', 'retry_wait', 'cancel_requested')"
        ") "
        "ORDER BY ss.updated_at ASC "
        "LIMIT $1",
        overage,
    )

    evicted = 0
    for row in rows:
        thread_key = row["thread_key"]
        sandbox_id = row["sandbox_id"]
        try:
            log.info(
                "idle_capacity_eviction",
                thread_key=thread_key,
                sandbox=sandbox_id[:12],
                active_count=int(active_count or 0),
                max_active=MAX_ACTIVE_SANDBOX_SESSIONS,
            )
            with contextlib.suppress(Exception):
                await backend.pause_by_id(sandbox_id)
            await pool.execute(
                "UPDATE sandbox_sessions SET state = 'suspended', updated_at = NOW() "
                "WHERE thread_key = $1 AND state = 'idle'",
                thread_key,
            )
            _drop_runtime(sandbox_id)
            evicted += 1
        except Exception:
            log.warning(
                "idle_capacity_eviction_failed",
                thread_key=thread_key,
                sandbox=sandbox_id[:12],
                exc_info=True,
            )
    return evicted


# ── Wire lease helpers (separate from sandbox lifecycle) ─────────────────────


async def _db_set_wire(thread_key: str) -> str:
    """Record an active wire lease for a session. Returns the generated lease_id."""
    pool = _get_pool()
    row = await pool.fetchrow(
        "UPDATE sandbox_sessions SET wire_lease_id = gen_random_uuid()::text, "
        "wire_connected_at = NOW(), wire_last_seen_at = NOW(), updated_at = NOW() "
        "WHERE thread_key = $1 RETURNING wire_lease_id",
        thread_key,
    )
    return row["wire_lease_id"]


async def _db_clear_wire(thread_key: str, lease_id: str) -> None:
    """Clear wire lease (only if it matches — prevents stale clears)."""
    pool = _get_pool()
    await pool.execute(
        "UPDATE sandbox_sessions SET wire_lease_id = NULL, wire_connected_at = NULL, "
        "wire_last_seen_at = NULL, updated_at = NOW() "
        "WHERE thread_key = $1 AND wire_lease_id = $2",
        thread_key,
        lease_id,
    )


async def _db_touch_wire(thread_key: str, lease_id: str) -> None:
    """Update wire heartbeat timestamp."""
    pool = _get_pool()
    await pool.execute(
        "UPDATE sandbox_sessions SET wire_last_seen_at = NOW(), updated_at = NOW() "
        "WHERE thread_key = $1 AND wire_lease_id = $2",
        thread_key,
        lease_id,
    )


async def _db_find_stale_wires(ttl_s: int = 120) -> list[dict]:
    """Find sessions with wire leases that haven't been seen recently."""
    pool = _get_pool()
    rows = await pool.fetch(
        "SELECT thread_key, sandbox_id, wire_lease_id, state "
        "FROM sandbox_sessions "
        "WHERE wire_lease_id IS NOT NULL "
        "AND wire_last_seen_at < NOW() - make_interval(secs => $1::double precision)",
        float(ttl_s),
    )
    return [dict(r) for r in rows]


# ── Flush pipeline helpers ───────────────────────────────────────────────────


async def _flush_pending(thread_key: str, last_delivered_id: str | None) -> list[dict]:
    """Fetch messages from chat_messages that haven't been delivered yet.

    Persistent harness sessions already retain their own assistant context, so
    we only replay user/system messages from durable storage. The exception is
    assistant messages imported as Slack history backfill for a new persona
    assignment; those rows are needed because the new sandbox has no prior
    assistant context.

    If last_delivered_id is NULL, returns all replayable messages.
    Otherwise returns non-assistant messages created after the cursor's
    created_at.
    """
    pool = _get_pool()
    role_filter = "(role <> 'assistant' OR metadata->>'history_backfill' = 'true')"
    if last_delivered_id is None:
        rows = await pool.fetch(
            "SELECT id, role, parts, user_id, metadata, created_at "
            f"FROM chat_messages WHERE thread_key = $1 AND {role_filter} ORDER BY created_at",
            thread_key,
        )
    else:
        rows = await pool.fetch(
            "SELECT id, role, parts, user_id, metadata, created_at "
            "FROM chat_messages WHERE thread_key = $1 "
            f"AND {role_filter} "
            "AND created_at > (SELECT created_at FROM chat_messages WHERE id = $2) "
            "ORDER BY created_at",
            thread_key,
            last_delivered_id,
        )
    return [dict(r) for r in rows]


def _flushed_to_messages(flushed_rows: list[dict]) -> list[dict]:
    """Convert flushed DB rows into the message format expected by
    ``messages_to_content_blocks``."""
    messages = []
    for row in flushed_rows:
        parts = row.get("parts", [])
        if isinstance(parts, str):
            parts = json.loads(parts)
        metadata = row.get("metadata") or {}
        if isinstance(metadata, str):
            metadata = json.loads(metadata)
        user_id = row.get("user_id")
        messages.append(
            {
                "role": row.get("role", "user"),
                "parts": parts,
                "history_backfill": (
                    metadata.get("history_backfill") is True
                    if isinstance(metadata, dict)
                    else False
                ),
                **({"user_id": user_id} if user_id else {}),
            }
        )
    return messages


async def _advance_cursor(thread_key: str, last_msg_id: str) -> None:
    """Advance the session cursor to the last delivered message ID."""
    pool = _get_pool()
    await pool.execute(
        "UPDATE sandbox_sessions SET last_delivered_id = $1, updated_at = NOW() "
        "WHERE thread_key = $2",
        last_msg_id,
        thread_key,
    )


async def _get_last_delivered_id(thread_key: str) -> str | None:
    """Get the last_delivered_id cursor from sandbox_sessions."""
    pool = _get_pool()
    row = await pool.fetchrow(
        "SELECT last_delivered_id FROM sandbox_sessions WHERE thread_key = $1",
        thread_key,
    )
    return row["last_delivered_id"] if row else None


async def _get_latest_thread_user_id(thread_key: str) -> str | None:
    """Return the most recent user id recorded for this thread.

    Slack turns can surface the requester in several durable rows depending on
    where execution is when prompt context is assembled. Prefer the newest row
    across those sources so the session context does not depend on one caller
    preserving one specific delivery field.

    Slack history backfill rows preserve thread context and may include the
    thread root author. They are not the requester for the active prompt.
    """
    pool = _get_pool()
    row = await pool.fetchrow(
        "WITH candidates AS ("
        "  SELECT COALESCE(user_id, metadata->>'user_id') AS user_id, created_at, 1 AS source_rank "
        "  FROM chat_messages "
        "  WHERE thread_key = $1 AND role = 'user' "
        "    AND COALESCE(metadata->>'history_backfill', 'false') <> 'true' "
        "  UNION ALL "
        "  SELECT COALESCE(metadata->>'user_id', delivery->>'recipient_user_id', delivery->>'user_id') "
        "    AS user_id, created_at, 2 AS source_rank "
        "  FROM agent_execution_requests "
        "  WHERE thread_key = $1 "
        "  UNION ALL "
        "  SELECT COALESCE(input_json->>'user_id', input_json#>>'{delivery,recipient_user_id}', "
        "    input_json#>>'{delivery,user_id}') AS user_id, created_at, 3 AS source_rank "
        "  FROM workflow_runs "
        "  WHERE thread_key = $1 AND workflow_name = 'slack_thread_turn' "
        ") "
        "SELECT user_id FROM candidates "
        "WHERE user_id IS NOT NULL AND btrim(user_id) <> '' "
        "ORDER BY created_at DESC, source_rank ASC "
        "LIMIT 1",
        thread_key,
    )
    user_id = row["user_id"] if row else None
    if not user_id:
        return None
    return str(user_id).strip() or None


def _valid_github_handle(value: str) -> str | None:
    candidate = value.strip().strip("@").strip()
    candidate = candidate.rstrip("/").split("/", 1)[0]
    return candidate if _GITHUB_HANDLE_RE.match(candidate) else None


def _extract_github_handle_from_slack_profile(
    profile: dict[str, Any],
) -> tuple[str | None, str | None, str]:
    """Return (handle, source, unavailable_reason) from Slack profile fields."""
    custom_fields = profile.get("custom_fields")
    if not isinstance(custom_fields, dict) or not custom_fields:
        return None, None, "no GitHub custom field found on Slack profile"

    saw_github_field = False
    for label, raw_value in custom_fields.items():
        label_text = str(label or "").strip()
        value = str(raw_value or "").strip()
        if not value:
            continue

        label_mentions_github = bool(_GITHUB_LABEL_RE.search(label_text))
        value_mentions_github = bool(_GITHUB_LABEL_RE.search(value))
        if not label_mentions_github and not value_mentions_github:
            continue
        saw_github_field = True

        source = (
            f'Slack profile custom field "{label_text}"'
            if label_text
            else "Slack profile custom field"
        )
        url_match = _GITHUB_URL_RE.search(value)
        if url_match:
            handle = _valid_github_handle(url_match.group(1))
            if handle:
                return f"@{handle}", source, ""

        prefixed_match = _GITHUB_PREFIX_RE.search(value)
        if prefixed_match:
            handle = _valid_github_handle(prefixed_match.group(1))
            if handle:
                return f"@{handle}", source, ""

        if label_mentions_github:
            handle = _valid_github_handle(value)
            if handle:
                return f"@{handle}", source, ""

    if saw_github_field:
        return (
            None,
            None,
            "GitHub profile field did not contain a valid GitHub handle",
        )
    return None, None, "no GitHub custom field found on Slack profile"


async def _resolve_requester_identity(
    *,
    platform: str | None,
    user_id: str | None,
) -> dict[str, str | bool] | None:
    if not user_id or (platform or "").lower() != "slack":
        return None

    identity: dict[str, str | bool] = {
        "slack_user_id": user_id,
        "slack_mention": f"<@{user_id}>",
    }
    try:
        from api.app import get_tool_manager

        profile = await get_tool_manager().call_tool_raw(
            "slack", "get_user_profile", {"user_id": user_id}
        )
    except Exception as exc:
        log.warning(
            "requester_identity_lookup_failed",
            platform=platform,
            user_id=user_id,
            error=str(exc),
        )
        identity.update(
            {
                "github_handle_verified": False,
                "github_handle_unavailable_reason": "Slack profile could not be fetched",
            }
        )
        return identity

    if not isinstance(profile, dict) or profile.get("error"):
        error = str(profile.get("error") or "Slack profile could not be fetched")
        log.warning(
            "requester_identity_lookup_failed",
            platform=platform,
            user_id=user_id,
            error=error,
        )
        identity.update(
            {
                "github_handle_verified": False,
                "github_handle_unavailable_reason": "Slack profile could not be fetched",
            }
        )
        return identity

    handle, source, reason = _extract_github_handle_from_slack_profile(profile)
    if handle:
        identity.update(
            {
                "github_handle": handle,
                "github_handle_source": source or "Slack profile custom field",
                "github_handle_verified": True,
            }
        )
    else:
        identity.update(
            {
                "github_handle_verified": False,
                "github_handle_unavailable_reason": reason,
            }
        )
    return identity


async def _insert_system_message(
    thread_key: str,
    platform: str | None,
    *,
    user_id: str | None = None,
) -> None:
    """Insert a static system message with platform formatting rules (idempotent)."""
    pool = _get_pool()
    effective_platform = platform or ("slack" if thread_key.startswith("slack:") else None)
    msg_id = f"system-{thread_key}-{effective_platform or 'generic'}"
    effective_user_id = user_id or await _get_latest_thread_user_id(thread_key)
    requester_identity = await _resolve_requester_identity(
        platform=effective_platform,
        user_id=effective_user_id,
    )
    context_metadata = {
        "session_context": True,
        "platform": effective_platform or "generic",
    }
    if effective_user_id:
        context_metadata["prompt_requester_user_id"] = effective_user_id
    context = _build_session_context(
        thread_key,
        platform=effective_platform,
        user_id=effective_user_id,
        requester_identity=requester_identity,
    )
    log.info(
        "session_context_prepared",
        thread_key=thread_key,
        platform=effective_platform,
        explicit_user_id=bool(user_id),
        effective_user_id=bool(effective_user_id),
        requester_identity=bool(requester_identity),
        github_handle_verified=bool(
            requester_identity and requester_identity.get("github_handle_verified")
        ),
    )
    await pool.execute(
        "INSERT INTO chat_messages (id, thread_key, role, parts, metadata) "
        "VALUES ($1, $2, 'system', $3::jsonb, $5::jsonb) "
        "ON CONFLICT (id) DO UPDATE SET "
        "  parts = EXCLUDED.parts, "
        "  metadata = EXCLUDED.metadata, "
        "  created_at = CASE "
        "    WHEN chat_messages.metadata->>'prompt_requester_user_id' "
        "      IS DISTINCT FROM EXCLUDED.metadata->>'prompt_requester_user_id' "
        "    THEN NOW() "
        "    ELSE chat_messages.created_at "
        "  END "
        "WHERE $4::boolean",
        msg_id,
        thread_key,
        json.dumps([{"type": "text", "text": context}]),
        bool(effective_user_id),
        json.dumps(context_metadata),
    )


# ── Harness / persona resolution ────────────────────────────────────────────


def _resolve_harness_profile(
    harness: str | None,
    *,
    persona: str | None = None,
    engine_override: str | None = None,
) -> tuple[str, str | None, str | None]:
    """Return ``(engine, persona_name, default_repo)`` for a spawn.

    ``harness`` is the legacy caller-facing name for what is now treated as
    the sandbox engine. Precedence is:
    explicit ``engine_override`` > explicit differing ``harness`` >
    persona-declared engine > deployment default.
    """
    from api.app import get_tool_manager

    normalized_engine_override = (engine_override or "").strip() or None
    if (
        normalized_engine_override
        and normalized_engine_override not in _ENGINE_HARNESSES
    ):
        raise ValueError(f"Unknown engine override: {normalized_engine_override}")

    normalized_harness = (harness or "").strip() or None
    if normalized_harness and normalized_harness not in _ENGINE_HARNESSES:
        raise ValueError(f"Unknown harness: {normalized_harness}")

    persona_info = (
        get_tool_manager().get_persona(persona) if persona else None
    )
    if persona and persona_info is None:
        raise ValueError(f"Unknown persona: {persona}")

    persona_engine = (
        (getattr(persona_info, "engine", None) or "").strip() or None
        if persona_info
        else None
    )

    if normalized_engine_override:
        engine = normalized_engine_override
    elif persona_engine:
        # Persona declares an engine. Use it unless the caller passed a
        # *different* harness arg, in which case the explicit harness arg is
        # treated as a user-driven engine override (e.g. `--invest --claude`).
        if normalized_harness and normalized_harness != persona_engine:
            engine = normalized_harness
        else:
            engine = persona_engine
    elif normalized_harness:
        engine = normalized_harness
    else:
        engine = default_harness()

    if persona_info:
        return engine, persona_info.name, persona_info.default_repo
    return engine, None, None


# ── Async public API ─────────────────────────────────────────────────────────


async def get_or_spawn(
    thread_key: str,
    harness: str | None = None,
    *,
    engine: str | None = None,
    persona: str | None = None,
) -> SandboxSession:
    """Get existing session or spawn a new sandbox.

    Tries (in order): DB session → warm pool → cold spawn.
    For suspended/dead sessions, preserves agent_thread_id for resume.
    """
    old_agent_thread_id: str = ""
    old_last_delivered_id: str = ""
    old_inflight_turn_id: str = ""
    old_inflight_turn_input: dict | None = None
    old_inflight_attempts: int = 0
    old_last_result: str = ""
    old_trace_id: str = ""
    pool = _get_pool()
    session = await _db_get_session(thread_key)
    if session:
        if session.db_state in _REUSABLE_DB_STATES:
            backend = get_backend()
            st = await backend.status(session)
            if st == "running":
                _get_runtime(session.sandbox_id)
                return session
            if session.db_state == "suspended":
                try:
                    await backend.resume_by_id(session.sandbox_id)
                    resumed_status = await backend.status(session)
                    if resumed_status != "running":
                        raise RuntimeError(
                            f"suspended sandbox did not resume: {resumed_status}"
                        )
                    await pool.execute(
                        "UPDATE sandbox_sessions SET state = 'idle', updated_at = NOW() "
                        "WHERE thread_key = $1 AND sandbox_id = $2",
                        thread_key,
                        session.sandbox_id,
                    )
                    session.db_state = "idle"
                    _get_runtime(session.sandbox_id)
                    return session
                except Exception as exc:
                    log.warning(
                        "suspended_session_resume_failed",
                        thread_key=thread_key,
                        sandbox=session.sandbox_id[:12],
                        error=str(exc),
                        exc_info=True,
                    )
                    raise RuntimeError(
                        f"failed to resume suspended sandbox: {session.sandbox_id}"
                    ) from exc
            # Container is gone — save agent_thread_id and cursor for resume, clean up row
            old_agent_thread_id = session.agent_thread_id
            old_last_delivered_id = session.last_delivered_id
            old_inflight_turn_id = session.inflight_turn_id
            old_inflight_turn_input = session.inflight_turn_input
            old_inflight_attempts = session.inflight_attempts
            old_last_result = session.last_result
            old_trace_id = session.trace_id
            if session.db_state == "suspended":
                with contextlib.suppress(Exception):
                    await backend.stop_by_id(session.sandbox_id)
            await _db_delete_session(thread_key)
            _drop_runtime(session.sandbox_id)
        else:
            # state is stopped/gone — clean up stale row
            old_agent_thread_id = session.agent_thread_id
            old_last_delivered_id = session.last_delivered_id
            old_inflight_turn_id = session.inflight_turn_id
            old_inflight_turn_input = session.inflight_turn_input
            old_inflight_attempts = session.inflight_attempts
            old_last_result = session.last_result
            old_trace_id = session.trace_id
            await _db_delete_session(thread_key)
            _drop_runtime(session.sandbox_id)

    thread_trace_id = await get_or_create_thread_trace_id(pool, thread_key)

    effective_harness = harness or default_harness()

    # Resolve harness profile (engine, persona, repo) once for both warm and cold paths
    resolved_engine, resolved_persona, repo = _resolve_harness_profile(
        effective_harness, persona=persona, engine_override=engine
    )

    # Try warm pool first
    should_try_warm = (
        not engine
        and not old_agent_thread_id
        and not old_inflight_turn_id
        and not (effective_harness == "amp" and resolved_engine == "codex")
    )
    if should_try_warm:
        from api.warm_pool import claim_container

        trace_id = old_trace_id or thread_trace_id or str(uuid.uuid4())
        claimed = await claim_container(
            thread_key, effective_harness, persona=resolved_persona, repo=repo, trace_id=trace_id
        )
        if claimed:
            if old_agent_thread_id:
                claimed.agent_thread_id = old_agent_thread_id
            won = await _db_insert_session(
                claimed,
                harness=claimed.harness,
                engine=claimed.engine,
                agent_thread_id=old_agent_thread_id,
                last_delivered_id=old_last_delivered_id,
                inflight_turn_id=old_inflight_turn_id,
                inflight_turn_input=old_inflight_turn_input,
                inflight_attempts=old_inflight_attempts,
                last_result=old_last_result,
            )
            if won:
                _get_runtime(claimed.sandbox_id)
                return claimed

    # Cold spawn
    resolved_engine, resolved_persona, repo = _resolve_harness_profile(
        effective_harness, persona=persona, engine_override=engine
    )
    backend = get_backend()
    await _evict_idle_sessions_for_capacity(backend)
    trace_id = old_trace_id or thread_trace_id or str(uuid.uuid4())
    session = await backend.create(
        thread_key,
        effective_harness,
        resolved_engine,
        persona=resolved_persona,
        repo=repo,
        resume_thread_id=old_agent_thread_id or None,
        trace_id=trace_id,
    )
    session.trace_id = trace_id
    if old_agent_thread_id:
        session.agent_thread_id = old_agent_thread_id
    _get_runtime(session.sandbox_id)
    log.info(
        "pipe_session_spawned", thread_key=thread_key, sandbox=session.sandbox_id[:12]
    )

    # INSERT into sandbox_sessions — race-safe
    won = await _db_insert_session(
        session,
        harness=session.harness,
        engine=session.engine,
        agent_thread_id=old_agent_thread_id,
        last_delivered_id=old_last_delivered_id,
        inflight_turn_id=old_inflight_turn_id,
        inflight_turn_input=old_inflight_turn_input,
        inflight_attempts=old_inflight_attempts,
        last_result=old_last_result,
    )
    if not won:
        log.warning(
            "spawn_race_lost", thread_key=thread_key, sandbox=session.sandbox_id[:12]
        )
        await backend.stop_by_id(session.sandbox_id)
        _drop_runtime(session.sandbox_id)
        winner = await _db_get_session(thread_key)
        if winner is None:
            raise RuntimeError(f"spawn race: winner row vanished for {thread_key}")
        _get_runtime(winner.sandbox_id)
        return winner

    return session


def _build_session_context(
    thread_key: str,
    *,
    platform: str | None = None,
    user_id: str | None = None,
    requester_identity: dict[str, str | bool] | None = None,
) -> str:
    """Build session context to append to the system prompt.

    Contains metadata (time, thread, platform) and platform-specific formatting
    rules so the agent produces output suitable for the target platform.
    """
    from datetime import datetime, timezone

    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S")
    lines = [
        "# Session Context",
        "",
        f"- **Date/Time**: {now} UTC",
        f"- **Thread ID**: {thread_key}",
    ]
    if platform:
        lines.append(f"- **Platform**: {platform}")

    if requester_identity:
        lines.extend(
            [
                "",
                "## Requester Identity",
                "",
                f"- Slack user ID: {requester_identity['slack_user_id']}",
                f"- Slack mention: {requester_identity['slack_mention']}",
            ]
        )
        if requester_identity.get("github_handle_verified"):
            github_handle = requester_identity["github_handle"]
            github_login = github_handle.removeprefix("@")
            lines.extend(
                [
                    "- GitHub handle from Slack profile: "
                    f"{github_handle}",
                    "- GitHub handle source: "
                    f"{requester_identity['github_handle_source']}",
                    "- GitHub handle verified: yes",
                    "",
                    "## GitHub PR Attribution",
                    "",
                    "- If you create a GitHub PR for this Slack request, "
                    f"the PR body MUST contain this standalone line: `Prompted by: {github_handle}`",
                    "- The credited prompter is the requester in this section, not the Slack thread OP/root author.",
                    "- This is a GitHub PR body requirement, not a Slack response mention rule.",
                    "- Assign the PR to the requester when possible: "
                    f"`{github_login}`",
                ]
            )
        else:
            lines.extend(
                [
                    "- GitHub handle from Slack profile: unavailable",
                    "- GitHub handle unavailable reason: "
                    f"{requester_identity['github_handle_unavailable_reason']}",
                    "- GitHub handle verified: no",
                    "",
                    "## GitHub PR Attribution",
                    "",
                    "- If you create a GitHub PR for this Slack request, do not infer a GitHub "
                    "username from Slack display name, real name, or email.",
                    "- Omit the `Prompted by` line unless a verified GitHub handle is present.",
                ]
            )

    if platform and platform.lower() == "slack":
        lines.extend(
            [
                "",
                "## Slack Formatting Rules",
                "",
                "- Use standard markdown links `[Display Text](URL)` for hyperlinks",
                "- Do NOT use Slack-native `<URL|text>` link syntax",
                "- Preserve Slack user mentions (`<@UXXXXXXX>`) exactly as-is — only use these for actual Slack users",
                "- For Twitter/X handles, link to the profile WITHOUT an @ prefix in the display text: `[handle](https://x.com/handle)` (NOT `[@handle](...)`)",
                "- Prefer concise, well-structured markdown; long replies may be split across multiple Slack messages",
                "- Markdown tables are allowed and may render as native Slack tables when the structure is clean",
                "- NEVER put links/URLs inside code blocks (``` ```) — they won't be clickable. Use markdown tables or plain text with `[text](url)` links instead",
                "- For links to Slack threads or messages, always use the canonical `https://slack.com/archives/{CHANNEL_ID}/p{TS_WITHOUT_DOT}` form. Slack redirects this to the correct workspace. Do not invent or hardcode a `<workspace>.slack.com` subdomain.",
                "- Do not @-mention or tag the requester when replying; reply naturally in the thread.",
            ]
        )

    lines.extend(["", "---", ""])
    return "\n".join(lines)


def _terminal_error_from_harness_event(event: dict) -> str | None:
    """Return terminal error text when an end-of-turn event represents failure."""
    event_type = event.get("type")

    if event_type == "error":
        err = event.get("error")
        if isinstance(err, str) and err.strip():
            return err.strip()
        if isinstance(err, dict):
            message = err.get("message")
            if isinstance(message, str) and message.strip():
                return message.strip()
        message = event.get("message")
        if isinstance(message, str) and message.strip():
            return message.strip()
        return "Harness reported an error"

    if event_type == "result":
        subtype = str(event.get("subtype") or "").strip().lower()
        is_error = bool(event.get("is_error")) or (subtype not in {"", "success"})
        if not is_error:
            return None

        err = event.get("error")
        if isinstance(err, str) and err.strip():
            return err.strip()
        if isinstance(err, dict):
            message = err.get("message")
            if isinstance(message, str) and message.strip():
                return message.strip()

        result = event.get("result")
        if isinstance(result, str) and result.strip():
            return result.strip()
        return "Harness reported an error"

    return None


async def _stream_stdout(
    session: SandboxSession,
    backend: Any,
    rt: RuntimeState,
    turn_id: int,
    t0: float,
) -> AsyncIterator[dict]:
    """Stream sandbox stdout, normalize events, yield SSE dicts.

    Keeps streaming across turns until the container exits (EOF).
    Callers that only need one turn can ``return`` from their own loop.
    """
    result_text = ""
    agent_thread_id: str | None = None
    first_output = False
    eof_reattach_attempts = 0

    while True:
        async for line in backend.stream_stdout(session):
            eof_reattach_attempts = 0
            if not first_output:
                first_output = True
                log.info(
                    "turn_first_output",
                    thread_key=session.thread_key,
                    sandbox=session.sandbox_id[:12],
                    harness=session.harness,
                    turn_id=turn_id,
                    elapsed_s=_elapsed_since(t0),
                )

            try:
                evt = json.loads(line)
            except (json.JSONDecodeError, TypeError):
                continue

            evt_type = evt.get("type", "") if isinstance(evt, dict) else ""
            if evt_type and evt_type not in _VALID_STDOUT_EVENT_TYPES:
                log.warning(
                    "stdout_unknown_event_type",
                    type=evt_type,
                    thread_key=session.thread_key,
                    sandbox=session.sandbox_id[:12],
                )

            tid = extract_thread_id(session.engine, evt)
            if tid:
                agent_thread_id = tid
                if session.agent_thread_id != tid:
                    session.agent_thread_id = tid
                    try:
                        pool = _get_pool()
                        await pool.execute(
                            "UPDATE sandbox_sessions SET agent_thread_id = $1, updated_at = NOW() "
                            "WHERE thread_key = $2",
                            tid,
                            session.thread_key,
                        )
                    except Exception:
                        log.warning(
                            "agent_thread_id_persist_failed",
                            thread_key=session.thread_key,
                        )
            r = extract_result(session.engine, evt)
            if r is not None:
                result_text = r
            if evt.get("type") == "error":
                result_text = ""

            for canonical in normalize_harness_event(session.engine, evt):
                yield {"data": json.dumps(canonical, separators=(",", ":"))}

            if is_turn_done(session.engine, evt):
                terminal_error = _terminal_error_from_harness_event(evt)
                terminal_result = result_text or terminal_error or ""
                rt.last_result = result_text
                # Persist agent_thread_id for conversation resume
                if agent_thread_id and session.agent_thread_id != agent_thread_id:
                    try:
                        pool = _get_pool()
                        await pool.execute(
                            "UPDATE sandbox_sessions SET agent_thread_id = $1, updated_at = NOW() "
                            "WHERE thread_key = $2",
                            agent_thread_id,
                            session.thread_key,
                        )
                        session.agent_thread_id = agent_thread_id
                    except Exception:
                        log.warning(
                            "agent_thread_id_persist_failed",
                            thread_key=session.thread_key,
                        )
                turn_id = rt.turn_counter  # pick up latest turn_id for next turn
                # Persist completion before emitting turn.done so reconnect callers
                # can't cancel the stream before durable state is committed.
                await asyncio.gather(
                    _persist_turn_messages(
                        session.thread_key, "", terminal_result, session.harness
                    ),
                    _db_complete_inflight_turn(session.thread_key, terminal_result),
                )
                turn_done_payload: dict[str, Any] = {
                    "type": "turn.done",
                    "turn_id": turn_id,
                    "result": terminal_result,
                    "agent_thread_id": agent_thread_id or "",
                }
                for key in ("cwd", "repo_owner", "repo_name", "git_ref", "git_commit"):
                    value = evt.get(key)
                    if isinstance(value, str) and value.strip():
                        turn_done_payload[key] = value.strip()
                if terminal_error:
                    turn_done_payload["is_error"] = True
                    turn_done_payload["error"] = terminal_error
                yield {"data": json.dumps(turn_done_payload)}
                log.info(
                    "turn_done",
                    thread_key=session.thread_key,
                    sandbox=session.sandbox_id[:12],
                    harness=session.harness,
                    turn_id=turn_id,
                    duration_s=_elapsed_since(t0),
                    reason="error" if terminal_error else "completed",
                )
                result_text = ""
                agent_thread_id = None
                t0 = time.monotonic()

        status = "gone"
        with contextlib.suppress(Exception):
            status = await backend.status(session)

        if status in {"running", "created"}:
            eof_reattach_attempts += 1
            if eof_reattach_attempts > STREAM_EOF_REATTACH_MAX:
                log.warning(
                    "stream_eof_reattach_exhausted",
                    thread_key=session.thread_key,
                    sandbox=session.sandbox_id[:12],
                    harness=session.harness,
                    turn_id=turn_id,
                    attempts=eof_reattach_attempts,
                )
                break
            log.info(
                "stream_eof_running_reattach",
                thread_key=session.thread_key,
                sandbox=session.sandbox_id[:12],
                harness=session.harness,
                turn_id=turn_id,
                attempts=eof_reattach_attempts,
            )
            with contextlib.suppress(Exception):
                await backend.close_streams(session)
            try:
                await backend.attach(session)
            except Exception:
                log.warning(
                    "stream_eof_reattach_failed",
                    thread_key=session.thread_key,
                    sandbox=session.sandbox_id[:12],
                    harness=session.harness,
                    turn_id=turn_id,
                )
                break
            await asyncio.sleep(STREAM_EOF_REATTACH_BACKOFF_S)
            continue

        break

    # EOF — container exited or stream ended
    log.info(
        "stream_eof",
        thread_key=session.thread_key,
        sandbox=session.sandbox_id[:12],
        harness=session.harness,
        turn_id=turn_id,
        duration_s=_elapsed_since(t0),
        reason="eof",
    )


# ── New API: connect (persistent stdout wire) + inject_stdin ─────────────────


async def stream_connect(
    session: SandboxSession,
    *,
    platform: str | None = None,
    user_id: str | None = None,
) -> AsyncIterator[dict]:
    """Attach to a sandbox's stdout and return a persistent SSE wire.

    Stays open across multiple turns until the container exits.
    Emits a wire.ready event once the reader is attached so the client
    knows it's safe to call inject_stdin / POST /agent/execute.
    """
    rt = _get_runtime(session.sandbox_id)

    effective_platform = platform or ("slack" if session.thread_key.startswith("slack:") else None)
    if effective_platform:
        await _insert_system_message(
            session.thread_key,
            effective_platform,
            user_id=user_id,
        )

    backend = get_backend()
    await backend.attach(session)
    await _db_update_state(session.thread_key, "running")
    lease_id = await _db_set_wire(session.thread_key)

    log.info(
        "sse_connect",
        thread_key=session.thread_key,
        sandbox=session.sandbox_id[:12],
        lease_id=lease_id,
        harness=session.harness,
        engine=session.engine,
    )

    # Signal the client that the wire is ready
    yield {
        "data": json.dumps(
            {
                "type": "wire.ready",
                "lease_id": lease_id,
                "turn_counter": rt.turn_counter,
            }
        )
    }

    # Heartbeat runs as an independent task so it fires even during long
    # silent tool calls (when _stream_stdout yields nothing for minutes).
    heartbeat_stop = asyncio.Event()

    async def _heartbeat_loop() -> None:
        while not heartbeat_stop.is_set():
            try:
                await asyncio.wait_for(heartbeat_stop.wait(), timeout=30)
                return  # event was set → stop
            except asyncio.TimeoutError:
                pass
            try:
                await _db_touch_wire(session.thread_key, lease_id)
            except Exception:
                pass

    heartbeat_task = asyncio.create_task(_heartbeat_loop())

    try:
        async for sse_dict in _stream_stdout(
            session,
            backend,
            rt,
            rt.turn_counter,
            time.monotonic(),
        ):
            yield sse_dict
    finally:
        heartbeat_stop.set()
        heartbeat_task.cancel()
        try:
            await heartbeat_task
        except (asyncio.CancelledError, Exception):
            pass
        await _db_clear_wire(session.thread_key, lease_id)
        if await _db_get_inflight_turn(session.thread_key) is None:
            await _db_update_state(session.thread_key, "idle")
        log.info(
            "sse_disconnect",
            thread_key=session.thread_key,
            sandbox=session.sandbox_id[:12],
            lease_id=lease_id,
            harness=session.harness,
            engine=session.engine,
        )


async def inject_stdin(
    session: SandboxSession,
    message: str | list,
    *,
    platform: str | None = None,
    user_id: str | None = None,
    trace_id: str | None = None,
    traceparent: str | None = None,
    trace_metadata: dict | None = None,
) -> dict:
    """Flush pending messages + write to stdin. Does not touch stdout.

    Returns a summary dict for the JSON response.
    """
    rt = _get_runtime(session.sandbox_id)

    effective_platform = platform or ("slack" if session.thread_key.startswith("slack:") else None)
    if effective_platform:
        await _insert_system_message(
            session.thread_key,
            effective_platform,
            user_id=user_id,
        )

    last_delivered_id = await _get_last_delivered_id(session.thread_key)
    flushed = await _flush_pending(session.thread_key, last_delivered_id)

    # Build harness-native input
    inline_blocks: list[dict] | None = None
    if isinstance(message, list) and message:
        inline_blocks = message
    elif isinstance(message, str) and message:
        inline_blocks = [{"type": "text", "text": message}]

    if flushed and inline_blocks:
        msgs = _flushed_to_messages(flushed)
        content_blocks = messages_to_content_blocks(msgs) + inline_blocks
        turn_input = build_user_input(
            content_blocks,
            thread_key=session.thread_key,
            trace_id=trace_id or session.trace_id,
            traceparent=traceparent,
            trace_metadata=trace_metadata,
        )
    elif flushed:
        msgs = _flushed_to_messages(flushed)
        content_blocks = messages_to_content_blocks(msgs)
        turn_input = build_user_input(
            content_blocks,
            thread_key=session.thread_key,
            trace_id=trace_id or session.trace_id,
            traceparent=traceparent,
            trace_metadata=trace_metadata,
        )
    elif inline_blocks:
        turn_input = build_user_input(
            inline_blocks,
            thread_key=session.thread_key,
            trace_id=trace_id or session.trace_id,
            traceparent=traceparent,
            trace_metadata=trace_metadata,
        )
    else:
        return {"ok": True, "injected": False}

    rt.turn_counter += 1
    rt.last_result = None
    durable_turn_id = f"turn-{uuid.uuid4().hex[:16]}"
    await _db_set_inflight_turn(
        session.thread_key,
        durable_turn_id,
        turn_input,
        attempts=1,
    )

    backend = get_backend()

    # Refresh sandbox token on every turn so it never expires mid-session
    try:
        fresh_token = mint_sandbox_token(session.thread_key, session.sandbox_id)
        await backend.refresh_token_by_id(session.sandbox_id, fresh_token)
    except Exception:
        log.warning(
            "token_refresh_failed",
            thread_key=session.thread_key,
            sandbox=session.sandbox_id[:12],
        )

    await backend.attach(session)

    try:
        await backend.write_stdin(session, turn_input)
    except (BrokenPipeError, OSError, RuntimeError, AssertionError) as exc:
        log.info(
            "stdin_broken_pipe_recovering",
            thread_key=session.thread_key,
            sandbox=session.sandbox_id[:12],
            durable_turn_id=durable_turn_id,
            error=str(exc),
        )
        st = await backend.status(session)
        if st != "running":
            log.warning(
                "stdin_broken_pipe_unrecovered",
                thread_key=session.thread_key,
                sandbox=session.sandbox_id[:12],
                durable_turn_id=durable_turn_id,
                sandbox_status=st,
            )
            raise RuntimeError(f"sandbox exited (status={st})") from exc
        # Only reset stdin — leave stdout reader intact
        await backend.reattach_stdin(session)
        await backend.write_stdin(session, turn_input)
        log.info(
            "stdin_broken_pipe_recovered",
            thread_key=session.thread_key,
            sandbox=session.sandbox_id[:12],
            durable_turn_id=durable_turn_id,
        )

    await _db_update_state(session.thread_key, "running")

    # Advance cursor so these messages aren't re-flushed
    last_flushed_id = flushed[-1]["id"] if flushed else None
    if last_flushed_id:
        await _advance_cursor(session.thread_key, last_flushed_id)

    turn_metrics = _turn_input_metrics(turn_input)

    log.info(
        "turn_start",
        thread_key=session.thread_key,
        sandbox=session.sandbox_id[:12],
        turn_id=rt.turn_counter,
        durable_turn_id=durable_turn_id,
        platform=platform,
        user_id=user_id,
        flushed_message_count=len(flushed),
        **turn_metrics,
    )
    return {
        "ok": True,
        "injected": True,
        "turn_id": rt.turn_counter,
        "durable_turn_id": durable_turn_id,
    }


async def steer_stdin(
    session: SandboxSession,
    content_blocks: list[dict],
) -> dict:
    """Inject a steer message into a running sandbox's stdin.

    Unlike inject_stdin(), this does NOT start a new turn or reset turn counters.
    The steer message tells Amp to cancel the current tool call and process
    the new message instead, preserving conversation context.
    """
    turn_input = build_user_input(
        content_blocks,
        steer=True,
        thread_key=session.thread_key,
        trace_id=session.trace_id,
    )
    backend = get_backend()

    is_amp = session.engine == "amp" or session.harness == "amp"
    try:
        if is_amp:
            await backend.interrupt_by_id(session.sandbox_id)
            await asyncio.sleep(0.05)
        try:
            await backend.write_stdin(session, turn_input)
        except (BrokenPipeError, OSError, RuntimeError, AssertionError):
            if not is_amp:
                raise
            await backend.reattach_stdin(session)
            await backend.write_stdin(session, turn_input)
    except (BrokenPipeError, OSError, RuntimeError, AssertionError) as exc:
        log.warning(
            "steer_stdin_failed",
            thread_key=session.thread_key,
            sandbox=session.sandbox_id[:12],
            error=str(exc),
        )
        return {"ok": False, "error": str(exc)}

    log.info(
        "steer_injected",
        thread_key=session.thread_key,
        sandbox=session.sandbox_id[:12],
    )
    return {"ok": True, "steered": True}


async def replay_inflight_turn(session: SandboxSession) -> dict:
    """Replay the persisted in-flight turn into a (new) sandbox.

    This is used after container replacement so Slack reconnect can continue
    without losing the active turn.
    """
    inflight = await _db_get_inflight_turn(session.thread_key)
    if inflight is None:
        return {"ok": True, "replayed": False}

    durable_turn_id, turn_input, attempts = inflight
    next_attempt = attempts + 1
    rt = _get_runtime(session.sandbox_id)
    rt.turn_counter += 1
    rt.last_result = None

    await _db_set_inflight_turn(
        session.thread_key,
        durable_turn_id,
        turn_input,
        attempts=next_attempt,
    )

    backend = get_backend()

    try:
        fresh_token = mint_sandbox_token(session.thread_key, session.sandbox_id)
        await backend.refresh_token_by_id(session.sandbox_id, fresh_token)
    except Exception:
        log.warning(
            "token_refresh_failed",
            thread_key=session.thread_key,
            sandbox=session.sandbox_id[:12],
        )

    await backend.attach(session)
    try:
        await backend.write_stdin(session, turn_input)
    except (BrokenPipeError, OSError, RuntimeError, AssertionError) as exc:
        log.info(
            "replay_broken_pipe_recovering",
            thread_key=session.thread_key,
            sandbox=session.sandbox_id[:12],
            durable_turn_id=durable_turn_id,
            attempt=next_attempt,
            error=str(exc),
        )
        st = await backend.status(session)
        if st != "running":
            log.warning(
                "replay_broken_pipe_unrecovered",
                thread_key=session.thread_key,
                sandbox=session.sandbox_id[:12],
                durable_turn_id=durable_turn_id,
                attempt=next_attempt,
                sandbox_status=st,
            )
            raise RuntimeError(f"sandbox exited during replay (status={st})") from exc
        await backend.reattach_stdin(session)
        await backend.write_stdin(session, turn_input)
        log.info(
            "replay_broken_pipe_recovered",
            thread_key=session.thread_key,
            sandbox=session.sandbox_id[:12],
            durable_turn_id=durable_turn_id,
            attempt=next_attempt,
        )

    await _db_update_state(session.thread_key, "running")
    log.info(
        "inflight_turn_replayed",
        thread_key=session.thread_key,
        sandbox=session.sandbox_id[:12],
        durable_turn_id=durable_turn_id,
        attempt=next_attempt,
    )
    return {
        "ok": True,
        "replayed": True,
        "turn_id": rt.turn_counter,
        "durable_turn_id": durable_turn_id,
        "attempt": next_attempt,
    }


# ── Supervisor ───────────────────────────────────────────────────────────────


async def supervise_wires() -> None:
    """Detect stale wire leases and clean up dead sessions.

    Runs periodically from app lifespan. Checks:
    1. Wires with no heartbeat in 120s → clear the lease
    2. Sessions whose container is gone → mark gone
    """
    try:
        stale = await _db_find_stale_wires(ttl_s=120)
        if not stale:
            return

        backend = get_backend()
        for row in stale:
            thread_key = row["thread_key"]
            sandbox_id = row["sandbox_id"]
            lease_id = row["wire_lease_id"]

            # Check if container is still alive
            session = SandboxSession(
                sandbox_id=sandbox_id,
                thread_key=thread_key,
                harness="",
                engine="",
            )
            try:
                st = await backend.status(session)
            except Exception:
                st = "gone"

            pool = _get_pool()
            if st != "running":
                log.warning(
                    "supervisor_dead_session",
                    thread_key=thread_key,
                    sandbox=sandbox_id[:12],
                    container_status=st,
                )
                await _db_update_state(thread_key, "gone")
                await pool.execute(
                    "UPDATE sandbox_sessions SET wire_lease_id = NULL, "
                    "wire_connected_at = NULL, wire_last_seen_at = NULL "
                    "WHERE thread_key = $1",
                    thread_key,
                )
                _drop_runtime(sandbox_id)
            else:
                log.info(
                    "supervisor_stale_wire_cleared",
                    thread_key=thread_key,
                    sandbox=sandbox_id[:12],
                    lease_id=lease_id,
                )
                await pool.execute(
                    "UPDATE sandbox_sessions SET wire_lease_id = NULL, "
                    "wire_connected_at = NULL, wire_last_seen_at = NULL, "
                    "updated_at = NOW() WHERE thread_key = $1 AND wire_lease_id = $2",
                    thread_key,
                    lease_id,
                )
    except Exception:
        log.warning("supervisor_error", exc_info=True)


async def _release_stale_runtime_assignments(pool, backend, *, limit: int = 500) -> int:
    """Release active assignment rows whose runtime is gone and no execution is live.

    This is intentionally conservative: a transient backend/API lookup failure
    skips the row, and assignments with non-terminal executions are left alone
    for the execution watchdog to handle.
    """
    rows = await pool.fetch(
        "SELECT a.thread_key, a.assignment_generation, a.runtime_id, a.updated_at, "
        "       s.state AS session_state "
        "FROM agent_runtime_assignments a "
        "LEFT JOIN sandbox_sessions s "
        "  ON s.thread_key = a.thread_key AND s.sandbox_id = a.runtime_id "
        "WHERE a.state = 'active' "
        "  AND a.updated_at < NOW() - make_interval(secs => $1::double precision) "
        "  AND NOT EXISTS ("
        "    SELECT 1 FROM agent_execution_requests e "
        "    WHERE e.thread_key = a.thread_key "
        "      AND e.assignment_generation = a.assignment_generation "
        "      AND e.status IN ('queued', 'running', 'retry_wait', 'cancel_requested')"
        "  ) "
        "  AND NOT EXISTS ("
        "    SELECT 1 FROM agent_message_requests m "
        "    WHERE m.thread_key = a.thread_key "
        "      AND m.assignment_generation = a.assignment_generation "
        "      AND m.delivered_execution_id IS NULL"
        "  ) "
        "ORDER BY a.updated_at ASC "
        "LIMIT $2",
        float(IDLE_TTL_S),
        max(1, min(limit, 500)),
    )
    released = 0
    for row in rows:
        thread_key = row["thread_key"]
        generation = int(row["assignment_generation"])
        runtime_id = str(row["runtime_id"])
        session_state = str(row["session_state"] or "")
        try:
            if session_state in {"gone", "stopped"}:
                runtime_status = session_state
            else:
                runtime_status = await backend.status_by_id(runtime_id)
        except Exception:
            log.warning(
                "runtime_assignment_gc_status_failed",
                thread_key=thread_key,
                assignment_generation=generation,
                runtime_id=runtime_id[:12],
                exc_info=True,
            )
            continue

        if runtime_status in {"running", "created"}:
            continue

        result = await pool.execute(
            "UPDATE agent_runtime_assignments "
            "SET state = 'released', released_at = NOW(), updated_at = NOW() "
            "WHERE thread_key = $1 AND assignment_generation = $2 AND state = 'active'",
            thread_key,
            generation,
        )
        if result.endswith(" 1"):
            released += 1
            log.info(
                "runtime_assignment_released_stale",
                thread_key=thread_key,
                assignment_generation=generation,
                runtime_id=runtime_id[:12],
                runtime_status=runtime_status,
                session_state=session_state or None,
            )
            _drop_runtime(runtime_id)
    return released


async def reconcile_tick() -> None:
    """Periodic reconciliation: check DB vs backend, enforce idle TTL, clean orphans.

    Runs every 60s from app lifespan. Replaces supervise_wires().
    """
    try:
        pool = _get_pool()
        backend = get_backend()

        async def _mark_inactive(thread_key: str) -> None:
            try:
                await pool.execute(
                    "UPDATE sandbox_sessions SET state = 'suspended', "
                    "wire_lease_id = NULL, wire_connected_at = NULL, "
                    "wire_last_seen_at = NULL, "
                    "inflight_turn_id = NULL, inflight_turn_input = NULL, "
                    "inflight_started_at = NULL, inflight_attempts = 0, "
                    "updated_at = NOW() "
                    "WHERE thread_key = $1",
                    thread_key,
                )
            except Exception:
                # Compatibility fallback for deployments missing the suspended state.
                log.warning("reconcile_suspend_fallback_gone", thread_key=thread_key)
                await pool.execute(
                    "UPDATE sandbox_sessions SET state = 'gone', "
                    "wire_lease_id = NULL, wire_connected_at = NULL, "
                    "wire_last_seen_at = NULL, "
                    "inflight_turn_id = NULL, inflight_turn_input = NULL, "
                    "inflight_started_at = NULL, inflight_attempts = 0, "
                    "updated_at = NOW() "
                    "WHERE thread_key = $1",
                    thread_key,
                )

        # Step A: Reconcile DB sessions against the active sandbox backend.
        rows = await pool.fetch(
            "SELECT thread_key, sandbox_id, state "
            "FROM sandbox_sessions "
            "WHERE state IN ('running', 'idle', 'delivering', 'error') "
            "LIMIT 50"
        )
        for row in rows:
            thread_key = row["thread_key"]
            sandbox_id = row["sandbox_id"]
            try:
                try:
                    st = await backend.status_by_id(sandbox_id)
                except Exception:
                    continue  # transient backend error -- skip, don't destroy
                if st not in ("running", "created"):
                    log.info(
                        "reconcile_session_gone",
                        thread_key=thread_key,
                        sandbox=sandbox_id[:12],
                        container_status=st,
                        db_state=row["state"],
                    )
                    await _mark_inactive(thread_key)
                    _drop_runtime(sandbox_id)
            except Exception:
                log.warning(
                    "reconcile_session_row_error",
                    thread_key=thread_key,
                    sandbox=sandbox_id[:12],
                    exc_info=True,
                )

        # Step B: Idle TTL enforcement
        idle_rows = await pool.fetch(
            "SELECT ss.thread_key, ss.sandbox_id FROM sandbox_sessions ss "
            "WHERE ss.state = 'idle' "
            "AND ss.updated_at < NOW() - make_interval(secs => $1::double precision) "
            "AND NOT EXISTS ("
            "  SELECT 1 FROM agent_execution_requests er "
            "  WHERE er.thread_key = ss.thread_key "
            "    AND er.status IN ('queued', 'running', 'retry_wait', 'cancel_requested')"
            ")",
            float(IDLE_TTL_S),
        )
        for row in idle_rows:
            thread_key = row["thread_key"]
            sandbox_id = row["sandbox_id"]
            try:
                log.info(
                    "idle_ttl_expired", thread_key=thread_key, sandbox=sandbox_id[:12]
                )
                with contextlib.suppress(Exception):
                    await backend.pause_by_id(sandbox_id)
                await _mark_inactive(thread_key)
                _drop_runtime(sandbox_id)
            except Exception:
                log.warning(
                    "reconcile_idle_row_error",
                    thread_key=thread_key,
                    sandbox=sandbox_id[:12],
                    exc_info=True,
                )

        # Step C: Reap old rows that are still marked running but have no live
        # wire, turn, or execution activity left to drive them.
        stale_running_rows = await pool.fetch(
            "SELECT ss.thread_key, ss.sandbox_id, ss.state "
            "FROM sandbox_sessions ss "
            "WHERE ss.state IN ('running', 'delivering', 'error') "
            "AND ss.updated_at < NOW() - make_interval(secs => $1::double precision) "
            "AND ss.wire_lease_id IS NULL "
            "AND ss.inflight_turn_id IS NULL "
            "AND NOT EXISTS ("
            "  SELECT 1 FROM agent_execution_requests er "
            "  WHERE er.thread_key = ss.thread_key "
            "    AND er.status IN ('queued', 'running', 'retry_wait', 'cancel_requested')"
            ")",
            float(IDLE_TTL_S),
        )
        for row in stale_running_rows:
            thread_key = row["thread_key"]
            sandbox_id = row["sandbox_id"]
            try:
                log.info(
                    "inactive_running_ttl_expired",
                    thread_key=thread_key,
                    sandbox=sandbox_id[:12],
                    state=row["state"],
                )
                with contextlib.suppress(Exception):
                    await backend.pause_by_id(sandbox_id)
                await _mark_inactive(thread_key)
                _drop_runtime(sandbox_id)
            except Exception:
                log.warning(
                    "reconcile_inactive_running_row_error",
                    thread_key=thread_key,
                    sandbox=sandbox_id[:12],
                    exc_info=True,
                )

        # Step D: Reap stale rows that still have an inflight turn recorded but no
        # execution remains to complete or replay it.
        stale_inflight_rows = await pool.fetch(
            "SELECT ss.thread_key, ss.sandbox_id, ss.state, ss.inflight_turn_id "
            "FROM sandbox_sessions ss "
            "WHERE ss.state IN ('running', 'delivering', 'error') "
            "AND ss.inflight_turn_id IS NOT NULL "
            "AND COALESCE(ss.inflight_started_at, ss.updated_at) "
            "    < NOW() - make_interval(secs => $1::double precision) "
            "AND ss.wire_lease_id IS NULL "
            "AND NOT EXISTS ("
            "  SELECT 1 FROM agent_execution_requests er "
            "  WHERE er.thread_key = ss.thread_key "
            "    AND er.status IN ('queued', 'running', 'retry_wait', 'cancel_requested')"
            ")",
            float(IDLE_TTL_S),
        )
        for row in stale_inflight_rows:
            thread_key = row["thread_key"]
            sandbox_id = row["sandbox_id"]
            try:
                log.warning(
                    "inflight_reaped",
                    thread_key=thread_key,
                    sandbox=sandbox_id[:12],
                    state=row["state"],
                    inflight_turn_id=row["inflight_turn_id"],
                )
                with contextlib.suppress(Exception):
                    await backend.pause_by_id(sandbox_id)
                await _mark_inactive(thread_key)
                _drop_runtime(sandbox_id)
            except Exception:
                log.warning(
                    "reconcile_stale_inflight_row_error",
                    thread_key=thread_key,
                    sandbox=sandbox_id[:12],
                    exc_info=True,
                )

        # Step E: Clean old terminated rows
        await pool.execute(
            "DELETE FROM sandbox_sessions "
            "WHERE state IN ('gone', 'stopped') "
            "AND updated_at < NOW() - INTERVAL '1 hour'"
        )
        expired_suspended_rows = await pool.fetch(
            "SELECT thread_key, sandbox_id FROM sandbox_sessions "
            "WHERE state = 'suspended' "
            "AND updated_at < NOW() - make_interval(secs => $1::double precision)",
            float(SUSPENDED_RETENTION_S),
        )
        for row in expired_suspended_rows:
            sandbox_id = row["sandbox_id"]
            with contextlib.suppress(Exception):
                await backend.stop_by_id(sandbox_id)
            _drop_runtime(sandbox_id)
        await pool.execute(
            "DELETE FROM sandbox_sessions "
            "WHERE state = 'suspended' "
            "AND updated_at < NOW() - make_interval(secs => $1::double precision)",
            float(SUSPENDED_RETENTION_S),
        )

        # Step E: Release active assignment rows whose runtime has disappeared.
        # This keeps spawn gating and operator views from being poisoned by
        # historical assignment rows while leaving live executions to the
        # execution watchdog.
        try:
            released = await _release_stale_runtime_assignments(pool, backend)
            if released:
                log.info("runtime_assignment_gc_completed", released=released)
        except Exception:
            log.warning("runtime_assignment_gc_failed", exc_info=True)

    except Exception:
        log.warning("reconcile_tick_error", exc_info=True)


async def stream_reconnect(
    session: SandboxSession, *, skip_done_count: int = 0
) -> AsyncIterator[dict]:
    """Re-attach to a running sandbox's stdout without sending a new turn.

    Yields SSE-ready ``{"data": line}`` dicts directly to EventSourceResponse.
    """
    backend = get_backend()
    await backend.close_streams(session)
    await backend.attach(session, logs=True)

    rt = _get_runtime(session.sandbox_id)
    turn_id = rt.turn_counter
    done_seen = 0

    async for sse_dict in _stream_stdout(
        session, backend, rt, turn_id, time.monotonic()
    ):
        evt_data = json.loads(sse_dict["data"])
        if evt_data.get("type") == "turn.done":
            done_seen += 1
            if done_seen <= skip_done_count:
                continue
        yield sse_dict
        if evt_data.get("type") == "turn.done" and done_seen > skip_done_count:
            return


async def _persist_turn_messages(
    thread_key: str, user_text: str, assistant_text: str, harness: str
) -> None:
    """Persist assistant message to chat_messages after a turn completes.

    User messages are already in the transcript from POST /agent/messages.
    Only the assistant response needs to be written here.
    """
    try:
        pool = _get_pool()
        now_ms = int(time.time() * 1000)
        asst_id = f"turn-{thread_key}-{now_ms}"

        async with pool.acquire() as conn:
            if assistant_text:
                await conn.execute(
                    "INSERT INTO chat_messages (id, thread_key, role, parts, metadata) "
                    "VALUES ($1, $2, 'assistant', $3::jsonb, $4::jsonb) "
                    "ON CONFLICT (id) DO NOTHING",
                    asst_id,
                    thread_key,
                    json.dumps([{"type": "text", "text": assistant_text}]),
                    json.dumps({"harness": harness}),
                )
                await conn.execute(
                    "UPDATE sandbox_sessions SET thread_name = $1, updated_at = NOW() "
                    "WHERE thread_key = $2",
                    assistant_text[:60],
                    thread_key,
                )
    except Exception as exc:
        log.warning(
            "chat_messages_persist_failed",
            thread_key=thread_key,
            harness=harness,
            error=str(exc),
        )


async def stop_session(thread_key: str) -> bool:
    """Stop sandbox and update DB. Returns True if stopped."""
    session = await _db_get_session(thread_key)
    if not session:
        return False

    return await stop_session_by_id(session.sandbox_id, thread_key=thread_key)


async def stop_session_by_id(sandbox_id: str, *, thread_key: str | None = None) -> bool:
    """Stop a sandbox by runtime id without relying on the current thread mapping."""
    backend = get_backend()
    await backend.stop_by_id(sandbox_id)
    _drop_runtime(sandbox_id)
    pool = _get_pool()
    await pool.execute(
        "UPDATE sandbox_sessions SET state = 'stopped', updated_at = NOW() "
        "WHERE sandbox_id = $1",
        sandbox_id,
    )
    log.info("pipe_session_stopped", thread_key=thread_key, sandbox=sandbox_id[:12])
    return True


async def get_status(thread_key: str) -> dict[str, Any]:
    """Check if a session/sandbox is alive."""
    session = await _db_get_session(thread_key)
    if not session:
        return {"thread_key": thread_key, "status": "not_found"}
    backend = get_backend()
    st = await backend.status(session)
    if st == "gone":
        await _db_update_state(thread_key, "gone")
        return {"thread_key": thread_key, "status": "gone"}
    rt = _runtime.get(session.sandbox_id)
    result: dict[str, Any] = {
        "thread_key": thread_key,
        "status": st,
        "state": session.db_state,
        "sandbox_id": session.sandbox_id[:12],
        "harness": session.harness,
        "engine": session.engine,
        "started_at": session.started_at,
    }
    if session.inflight_turn_id:
        result["inflight_turn_id"] = session.inflight_turn_id
    if session.inflight_attempts:
        result["inflight_attempts"] = session.inflight_attempts
    if session.last_result:
        result["last_result"] = session.last_result
    elif rt and rt.last_result is not None:
        # Best-effort bridge while the turn.done DB write is in-flight.
        result["last_result"] = rt.last_result
    return result
