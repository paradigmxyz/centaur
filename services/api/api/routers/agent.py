"""Agent router — durable control-plane endpoints for agents."""

from __future__ import annotations

import asyncio
import json as _json
import os
import re
import uuid


import structlog
from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import JSONResponse
from typing import Any
from sse_starlette import EventSourceResponse, ServerSentEvent

from pydantic import BaseModel

from api.agent import (
    get_status,
    stop_session,
)
from api.deps import (
    enforce_sandbox_thread_scope,
    get_sandbox_claims,
    require_scope,
    verify_api_key,
)
from api.final_delivery import (
    format_last_error,
    requires_delivery_lease,
    should_dead_letter_failure,
)
from api.runtime_control import (
    ControlPlaneError,
    append_message,
    cancel_execution,
    canonical_json,
    enqueue_execution,
    get_active_assignment,
    get_execution,
    get_execution_terminal_snapshot,
    list_thread_executions,
    release_assignment,
    spawn_assignment,
    steer_execution,
)
from api.trace_context import traceparent_from_trace_id
from api.warm_pool import pool_status
from api.warm_pool import replenish as replenish_pool

log = structlog.get_logger()

FINAL_DELIVERY_MAX_ATTEMPTS = int(os.getenv("FINAL_DELIVERY_MAX_ATTEMPTS", "50"))

router = APIRouter(
    prefix="/agent",
    tags=["agent"],
    dependencies=[Depends(verify_api_key)],
)


# ── Known harness flags ─────────────────────────────────────────────────────

_HARNESS_FLAGS: dict[str, str] = {
    "amp": "amp",
    "claude": "claude-code",
    "claude-code": "claude-code",
    "codex": "codex",
    "pi": "pi-mono",
    "pi-mono": "pi-mono",
}

_KNOWN_FLAGS = {
    *_HARNESS_FLAGS,
    "opus", "sonnet", "haiku", "engine", "model",
}


def parse_harness_from_message(text: str) -> tuple[str | None, str, bool]:
    """Parse harness directives from message text.

    Returns (harness_or_None, cleaned_text, harness_was_explicit).
    """
    cleaned = text
    harness: str | None = None
    explicit = False

    # 1. key=value syntax: harness=X
    kv_match = re.search(r"\bharness\s*=\s*([A-Za-z0-9_-]+)\b", cleaned, re.IGNORECASE)
    if kv_match:
        harness = kv_match.group(1).lower()
        explicit = True
        cleaned = (cleaned[: kv_match.start()] + cleaned[kv_match.end() :]).strip()

    # 2. Known harness flags: --amp, --claude, etc.
    for flag, value in _HARNESS_FLAGS.items():
        pattern = re.compile(r"(^|\s)--" + re.escape(flag) + r"(?=\s|$)", re.IGNORECASE)
        if pattern.search(cleaned):
            harness = value
            explicit = True
            cleaned = pattern.sub(" ", cleaned)

    # 3. Strip legacy engine/model flags (no harness effect)
    cleaned = re.sub(
        r"(^|\s)--(engine|model)\s+[A-Za-z0-9._-]+(?=\s|$)", " ", cleaned, flags=re.IGNORECASE
    )
    cleaned = re.sub(r"(^|\s)--(opus|sonnet|haiku)(?=\s|$)", " ", cleaned, flags=re.IGNORECASE)
    cleaned = re.sub(r"\bmodel\s*=\s*[A-Za-z0-9._-]+\b", "", cleaned, flags=re.IGNORECASE)

    # 4. Generic --flag → persona/harness name (any unknown flag)
    generic_re = re.compile(r"(^|\s)--([a-z][a-z0-9-]*)(?=\s|$)", re.IGNORECASE)
    for m in generic_re.finditer(cleaned):
        flag = m.group(2).lower()
        if flag in _KNOWN_FLAGS:
            continue
        harness = flag
        explicit = True
    cleaned = generic_re.sub(" ", cleaned)

    # Normalise whitespace
    cleaned = re.sub(r"\s+", " ", cleaned).strip()

    return harness, cleaned, explicit


class ExecuteRequest(BaseModel):
    thread_key: str
    assignment_generation: int | None = None
    execute_id: str | None = None
    harness: str | None = None
    delivery: dict[str, Any] | None = None
    platform: str | None = None
    user_id: str | None = None
    metadata: dict[str, Any] | None = None
    # Convenience: if message is set and assignment_generation is omitted,
    # the server auto-orchestrates spawn → message → execute.
    message: str | None = None
    engine: str | None = None
    persona_id: str | None = None


class SpawnRequest(BaseModel):
    thread_key: str
    spawn_id: str | None = None
    harness: str | None = None
    engine: str | None = None
    persona_id: str | None = None
    agents_md_override: str | None = None


class MessageRequest(BaseModel):
    thread_key: str
    assignment_generation: int
    message_id: str | None = None
    event: dict[str, Any] | None = None
    role: str | None = None
    parts: list[dict[str, Any]] | None = None
    user_id: str | None = None
    metadata: dict[str, Any] | None = None


class SteerExecutionRequest(BaseModel):
    content_blocks: list[dict[str, Any]] | None = None
    message_id: str | None = None
    user_id: str | None = None
    metadata: dict[str, Any] | None = None


def _normalize_message_event(body: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    metadata = body.get("metadata") if isinstance(body.get("metadata"), dict) else {}
    if body.get("user_id"):
        metadata = {**metadata, "user_id": body.get("user_id")}

    raw_event = body.get("event")
    if isinstance(raw_event, dict):
        return raw_event, metadata

    parts = body.get("parts")
    if not isinstance(parts, list):
        parts = []

    role = body.get("role") if isinstance(body.get("role"), str) else "user"
    event = {
        "type": "user",
        "message": {
            "role": role,
            "content": parts,
        },
    }
    return event, metadata


def _json_error(code: str, message: str, status: int) -> JSONResponse:
    return JSONResponse(status_code=status, content={"code": code, "message": message})


def _persona_payload(persona_name: str | None) -> dict[str, Any] | None:
    if not persona_name:
        return None
    from api.app import get_tool_manager

    persona = get_tool_manager().get_persona(persona_name)
    if persona is None:
        return None
    return {
        "name": persona.name,
        "description": persona.description,
        "engine": persona.engine,
        "default_repo": persona.default_repo,
        "prompt_file": persona.prompt_file,
        "has_custom_executor": persona.has_custom_executor,
    }


def _available_personas() -> list[str]:
    from api.app import get_tool_manager

    return sorted(get_tool_manager().personas)


def _overlay_runtime_payload() -> dict[str, Any]:
    from pathlib import Path as _Path

    api_mount_raw = (os.getenv("CENTAUR_OVERLAY_DIR") or "").strip() or None
    overlay_image = (os.getenv("CENTAUR_OVERLAY_IMAGE") or "").strip() or None
    # `loaded` mirrors what `assemble_prompt` actually saw on disk so the runtime
    # endpoint can never disagree with the [Active deployment] block we baked
    # into the prompt.
    api_mount_present = bool(api_mount_raw and _Path(api_mount_raw).exists())
    return {
        "loaded": bool(api_mount_present or overlay_image),
        "mount_api": api_mount_raw if api_mount_present else None,
        "mount_sandbox": "/home/agent/overlay/org" if overlay_image else None,
        "image": overlay_image,
    }


@router.post("/execute", dependencies=[Depends(require_scope("agent:execute"))])
async def execute(request: Request):
    body = ExecuteRequest.model_validate(await request.json())
    enforce_sandbox_thread_scope(request, body.thread_key, write=True)
    pool = request.app.state.db_pool

    # Auto-orchestrate spawn → message → execute when assignment_generation
    # is omitted.  This is the sub-agent fire-and-forget convenience path:
    #   POST /agent/execute {"thread_key":"task:…","message":"…","harness":"invest"}
    if body.assignment_generation is None:
        if not body.message:
            return _json_error(
                "MISSING_FIELD",
                "assignment_generation is required when message is not provided",
                422,
            )
        try:
            return await _auto_execute(pool, body)
        except ControlPlaneError as exc:
            return _json_error(exc.code, exc.message, exc.status_code)

    execute_id = body.execute_id or f"exec-{uuid.uuid4().hex[:16]}"
    delivery = body.delivery or {
        "channel": "slack",
        "platform": body.platform or "slack",
        "recipient_user_id": body.user_id,
    }
    metadata = body.metadata or {}
    if body.user_id:
        metadata = {**metadata, "user_id": body.user_id}

    try:
        result = await enqueue_execution(
            pool,
            thread_key=body.thread_key,
            assignment_generation=body.assignment_generation,
            execute_id=execute_id,
            harness=body.harness,
            delivery=delivery,
            metadata=metadata,
        )
    except ControlPlaneError as exc:
        return _json_error(exc.code, exc.message, exc.status_code)

    return JSONResponse(status_code=202, content=result)


async def _auto_execute(pool, body: ExecuteRequest) -> JSONResponse:
    """Server-side spawn → message → execute for sub-agent convenience.

    Callers can fire a single POST /agent/execute with {thread_key, message,
    harness} and get back an execution handle without manually threading
    assignment_generation through three separate calls.
    """
    nonce = f"auto-{uuid.uuid4().hex[:12]}"

    # 1. Spawn
    spawn_result = await spawn_assignment(
        pool,
        thread_key=body.thread_key,
        spawn_id=f"{nonce}:spawn",
        harness=body.harness,
        engine=body.engine,
        persona_id=body.persona_id,
        agents_md_override=None,
    )
    assignment_generation = int(spawn_result["assignment_generation"])

    # 2. Message
    message_event = {
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": body.message}],
        },
    }
    metadata = body.metadata or {}
    if body.user_id:
        metadata = {**metadata, "user_id": body.user_id}

    await append_message(
        pool,
        thread_key=body.thread_key,
        assignment_generation=assignment_generation,
        message_id=f"{nonce}:message",
        event=message_event,
        metadata=metadata,
    )

    # 3. Execute
    execute_id = body.execute_id or f"exec-{nonce}"
    delivery = body.delivery or {
        "channel": "dev",
        "platform": body.platform or "dev",
        "recipient_user_id": body.user_id,
    }

    result = await enqueue_execution(
        pool,
        thread_key=body.thread_key,
        assignment_generation=assignment_generation,
        execute_id=execute_id,
        harness=body.harness,
        delivery=delivery,
        metadata=metadata,
    )

    return JSONResponse(status_code=202, content=result)


@router.post("/spawn", dependencies=[Depends(require_scope("agent:execute"))])
async def spawn(req: SpawnRequest, request: Request):
    enforce_sandbox_thread_scope(request, req.thread_key, write=True)
    pool = request.app.state.db_pool
    spawn_id = req.spawn_id or f"spawn-{uuid.uuid4().hex[:16]}"
    try:
        return await spawn_assignment(
            pool,
            thread_key=req.thread_key,
            spawn_id=spawn_id,
            harness=req.harness,
            engine=req.engine,
            persona_id=req.persona_id,
            agents_md_override=req.agents_md_override,
        )
    except ControlPlaneError as exc:
        return _json_error(exc.code, exc.message, exc.status_code)


@router.post("/message", dependencies=[Depends(require_scope("agent:execute"))])
async def post_message(request: Request):
    body = MessageRequest.model_validate(await request.json())
    enforce_sandbox_thread_scope(request, body.thread_key, write=True)
    event, metadata = _normalize_message_event(body.model_dump(exclude_none=True))
    message_id = body.message_id or f"msg-{uuid.uuid4().hex[:16]}"
    pool = request.app.state.db_pool
    try:
        return await append_message(
            pool,
            thread_key=body.thread_key,
            assignment_generation=body.assignment_generation,
            message_id=message_id,
            event=event,
            metadata=metadata,
        )
    except ControlPlaneError as exc:
        return _json_error(exc.code, exc.message, exc.status_code)


@router.post("/messages", dependencies=[Depends(require_scope("agent:execute"))])
async def post_messages(request: Request):
    """Batch variant of /agent/message."""
    body = await request.json()
    thread_key = body.get("thread_key")
    if not thread_key:
        raise HTTPException(status_code=422, detail="thread_key is required")
    assignment_generation = body.get("assignment_generation")
    if assignment_generation is None:
        raise HTTPException(status_code=422, detail="assignment_generation is required")

    raw_messages = body.get("messages") if isinstance(body.get("messages"), list) else None
    if raw_messages is None:
        raw_messages = [body]

    pool = request.app.state.db_pool
    inserted = 0
    stored: list[str] = []

    for msg in raw_messages:
        if not isinstance(msg, dict):
            continue
        normalized = {
            **body,
            **msg,
            "thread_key": thread_key,
            "assignment_generation": msg.get("assignment_generation", assignment_generation),
        }
        event, metadata = _normalize_message_event(normalized)
        message_id = str(msg.get("message_id") or f"msg-{uuid.uuid4().hex[:16]}")
        try:
            result = await append_message(
                pool,
                thread_key=thread_key,
                assignment_generation=int(normalized["assignment_generation"]),
                message_id=message_id,
                event=event,
                metadata=metadata,
            )
            inserted += 1
            stored.append(str(result.get("message_id") or message_id))
        except ControlPlaneError as exc:
            return _json_error(exc.code, exc.message, exc.status_code)

    log.info("message_buffered", thread_key=thread_key, message_count=len(raw_messages), inserted=inserted)
    return {"ok": True, "inserted": inserted, "message_ids": stored}


@router.get("/messages", dependencies=[Depends(require_scope("agent:execute"))])
async def get_messages(request: Request, thread_key: str, cursor: str | None = None, limit: int = 50):
    """Paginated chat_messages for a thread."""
    enforce_sandbox_thread_scope(request, thread_key, write=False)
    pool = request.app.state.db_pool
    limit = min(limit, 200)

    if cursor:
        rows = await pool.fetch(
            "SELECT id, role, parts, user_id, metadata, created_at "
            "FROM chat_messages WHERE thread_key = $1 "
            "AND created_at > (SELECT created_at FROM chat_messages WHERE id = $2) "
            "ORDER BY created_at LIMIT $3",
            thread_key,
            cursor,
            limit + 1,
        )
    else:
        rows = await pool.fetch(
            "SELECT id, role, parts, user_id, metadata, created_at "
            "FROM chat_messages WHERE thread_key = $1 "
            "ORDER BY created_at LIMIT $2",
            thread_key,
            limit + 1,
        )

    has_more = len(rows) > limit
    if has_more:
        rows = rows[:limit]

    messages = []
    last_id = None
    for row in rows:
        last_id = row["id"]
        parts = row["parts"]
        if isinstance(parts, str):
            parts = _json.loads(parts)
        meta = row["metadata"]
        if isinstance(meta, str):
            meta = _json.loads(meta)
        messages.append({
            "id": row["id"],
            "role": row["role"],
            "parts": parts,
            "user_id": row["user_id"],
            "metadata": meta,
            "created_at": row["created_at"].isoformat() if row["created_at"] else None,
        })

    return {
        "messages": messages,
        "cursor": last_id if has_more else None,
        "has_more": has_more,
    }


class StopRequest(BaseModel):
    thread_key: str


@router.post("/stop", dependencies=[Depends(require_scope("agent:stop"))])
async def stop(req: StopRequest, request: Request):
    enforce_sandbox_thread_scope(request, req.thread_key, write=True)
    ok = await stop_session(req.thread_key)
    return {"ok": ok}


class TitleRequest(BaseModel):
    thread_key: str
    title: str


@router.post("/title", dependencies=[Depends(require_scope("agent:execute"))])
async def set_title(req: TitleRequest, request: Request):
    enforce_sandbox_thread_scope(request, req.thread_key, write=True)
    pool = request.app.state.db_pool
    await pool.execute(
        "UPDATE sandbox_sessions SET thread_name = $1, updated_at = NOW() WHERE thread_key = $2",
        req.title,
        req.thread_key,
    )
    return {"ok": True}


@router.get("/status", dependencies=[Depends(require_scope("agent:status"))])
async def status(request: Request, key: str):
    enforce_sandbox_thread_scope(request, key, write=False)
    result = await get_status(key)
    # Add pending message count
    try:
        pool = request.app.state.db_pool
        session_row = await pool.fetchrow(
            "SELECT last_delivered_id FROM sandbox_sessions WHERE thread_key = $1", key
        )
        if session_row:
            last_id = session_row["last_delivered_id"]
            if last_id is None:
                count_row = await pool.fetchrow(
                    "SELECT COUNT(*) as cnt FROM chat_messages WHERE thread_key = $1", key
                )
            else:
                count_row = await pool.fetchrow(
                    "SELECT COUNT(*) as cnt FROM chat_messages WHERE thread_key = $1 "
                    "AND created_at > (SELECT created_at FROM chat_messages WHERE id = $2)",
                    key, last_id,
                )
            result["pending_messages"] = count_row["cnt"] if count_row else 0
        else:
            # No session yet — count all messages for this thread
            count_row = await pool.fetchrow(
                "SELECT COUNT(*) as cnt FROM chat_messages WHERE thread_key = $1", key
            )
            result["pending_messages"] = count_row["cnt"] if count_row else 0
    except Exception:
        pass
    try:
        active = await get_active_assignment(request.app.state.db_pool, key)
        if active:
            result["active_assignment"] = {
                "assignment_generation": int(active["assignment_generation"]),
                "runtime_id": active["runtime_id"],
                "harness": active["harness"],
                "persona_id": active["persona_id"],
                "prompt_ref": active["prompt_ref"],
                "effective_agents_md_sha256": active["effective_agents_md_sha256"],
                "state": active["state"],
            }
    except Exception:
        pass
    return result


@router.get("/runtime", dependencies=[Depends(require_scope("agent:status"))])
async def runtime(request: Request, key: str):
    enforce_sandbox_thread_scope(request, key, write=False)
    active = await get_active_assignment(request.app.state.db_pool, key)
    persona_id = active["persona_id"] if active else None
    return {
        "thread_key": key,
        "assignment_generation": int(active["assignment_generation"]) if active else None,
        "runtime_id": active["runtime_id"] if active else None,
        "harness": active["harness"] if active else None,
        "engine": active["engine"] if active else None,
        "persona_id": persona_id,
        "persona": _persona_payload(persona_id),
        "overlay": _overlay_runtime_payload(),
        "available_personas": _available_personas(),
    }


@router.get("/executions/{execution_id}", dependencies=[Depends(require_scope("agent:execute"))])
async def execution_status(request: Request, execution_id: str):
    pool = request.app.state.db_pool
    result = await get_execution(pool, execution_id)
    if not result:
        raise HTTPException(status_code=404, detail="execution not found")
    enforce_sandbox_thread_scope(request, result["thread_key"], write=False)
    return result


@router.get("/threads/{thread_key}/executions", dependencies=[Depends(require_scope("agent:execute"))])
async def thread_executions(request: Request, thread_key: str, limit: int = 20):
    enforce_sandbox_thread_scope(request, thread_key, write=False)
    pool = request.app.state.db_pool
    return {
        "thread_key": thread_key,
        "executions": await list_thread_executions(pool, thread_key, limit),
    }


@router.get("/threads/{thread_key}/events", dependencies=[Depends(require_scope("agent:execute"))])
async def thread_events(
    request: Request,
    thread_key: str,
    after_event_id: int = 0,
    execution_id: str | None = None,
    poll_ms: int = 500,
):
    enforce_sandbox_thread_scope(request, thread_key, write=False)
    pool = request.app.state.db_pool
    poll_s = max(0.05, min(poll_ms / 1000.0, 5.0))

    async def _iter_events():
        cursor = max(0, after_event_id)
        emitted_rows = False
        while True:
            if await request.is_disconnected():
                break

            if execution_id:
                rows = await pool.fetch(
                    "SELECT event_id, event_kind, event_json FROM agent_execution_events "
                    "WHERE thread_key = $1 AND event_id > $2 AND execution_id = $3 "
                    "ORDER BY event_id ASC LIMIT 200",
                    thread_key,
                    cursor,
                    execution_id,
                )
            else:
                rows = await pool.fetch(
                    "SELECT event_id, event_kind, event_json FROM agent_execution_events "
                    "WHERE thread_key = $1 AND event_id > $2 "
                    "ORDER BY event_id ASC LIMIT 200",
                    thread_key,
                    cursor,
                )

            if not rows:
                if execution_id:
                    snapshot = await get_execution_terminal_snapshot(pool, execution_id)
                    if snapshot and snapshot["event_json"].get("thread_key") == thread_key:
                        if emitted_rows:
                            return
                        payload = snapshot["event_json"]
                        snapshot_id = max(cursor, int(snapshot["event_id"]))
                        yield ServerSentEvent(
                            id=str(snapshot_id),
                            event=str(snapshot["event_kind"]),
                            data=_json.dumps(payload, separators=(",", ":")),
                        )
                        return
                await asyncio.sleep(poll_s)
                continue

            for row in rows:
                emitted_rows = True
                cursor = int(row["event_id"])
                payload = row["event_json"]
                if isinstance(payload, str):
                    try:
                        payload = _json.loads(payload)
                    except Exception:
                        payload = {"type": "unknown", "raw": payload}
                yield ServerSentEvent(
                    id=str(cursor),
                    event=str(row["event_kind"]),
                    data=_json.dumps(payload, separators=(",", ":")),
                )

    return EventSourceResponse(
        _iter_events(),
        ping_message_factory=lambda: ServerSentEvent(comment="keepalive"),
        sep="\n",
    )


class ReleaseRequest(BaseModel):
    release_id: str | None = None
    cancel_inflight: bool = False


@router.post("/threads/{thread_key}/release", dependencies=[Depends(require_scope("agent:execute"))])
async def release_thread(request: Request, thread_key: str, body: ReleaseRequest):
    enforce_sandbox_thread_scope(request, thread_key, write=True)
    pool = request.app.state.db_pool
    release_id = body.release_id or f"rel-{uuid.uuid4().hex[:16]}"
    try:
        return await release_assignment(
            pool,
            thread_key=thread_key,
            release_id=release_id,
            cancel_inflight=body.cancel_inflight,
        )
    except ControlPlaneError as exc:
        return _json_error(exc.code, exc.message, exc.status_code)


@router.post("/executions/{execution_id}/cancel", dependencies=[Depends(require_scope("agent:execute"))])
async def execution_cancel(request: Request, execution_id: str):
    pool = request.app.state.db_pool
    # Resolve the execution's thread first so a sandbox token cannot cancel an
    # execution belonging to a different thread.
    existing = await get_execution(pool, execution_id)
    if not existing:
        raise HTTPException(status_code=404, detail="execution not found")
    enforce_sandbox_thread_scope(request, existing["thread_key"], write=True)
    result = await cancel_execution(pool, execution_id)
    if not result:
        raise HTTPException(status_code=404, detail="execution not found")
    log.info(
        "execution_cancel_requested",
        execution_id=execution_id,
        thread_key=result.get("thread_key"),
        status=result.get("status"),
    )
    return result


@router.post("/executions/{execution_id}/steer", dependencies=[Depends(require_scope("agent:execute"))])
async def steer_execution_endpoint(execution_id: str, request: Request):
    """Steer a running execution with a new user message.

    Injects a steer message into the sandbox's stdin, causing Amp to
    cancel the current tool call and process the new message instead.
    Falls back to cancellation if steering fails.
    """
    pool = request.app.state.db_pool
    # Resolve the execution's thread first so a sandbox token cannot steer (and
    # thereby inject a user message into) an execution on a different thread.
    existing = await get_execution(pool, execution_id)
    if not existing:
        raise HTTPException(status_code=404, detail="Execution not found")
    enforce_sandbox_thread_scope(request, existing["thread_key"], write=True)
    raw_bytes = await request.body()
    raw_body = _json.loads(raw_bytes) if raw_bytes else {}
    body = SteerExecutionRequest.model_validate(raw_body)
    metadata = body.metadata or {}
    if body.user_id and "user_id" not in metadata:
        metadata = {**metadata, "user_id": body.user_id}
    result = await steer_execution(
        pool,
        execution_id,
        content_blocks=body.content_blocks,
        message_id=body.message_id,
        metadata=metadata,
    )
    if result is None:
        raise HTTPException(status_code=404, detail="Execution not found")
    return JSONResponse(status_code=200, content=result)


class ClaimFinalDeliveryRequest(BaseModel):
    consumer_id: str
    limit: int = 1
    lease_seconds: int = 60
    platform: str | None = None


@router.post("/final-deliveries/claim", dependencies=[Depends(require_scope("agent:execute"))])
async def claim_final_delivery(request: Request, body: ClaimFinalDeliveryRequest):
    claims = get_sandbox_claims(request)
    if claims is not None:
        raise HTTPException(status_code=403, detail="Sandbox tokens cannot claim final deliveries")
    pool = request.app.state.db_pool
    limit = max(1, min(body.limit, 20))
    lease_seconds = max(15, min(body.lease_seconds, 600))
    platform = body.platform.strip() if body.platform else None
    rows = await pool.fetch(
        "WITH candidates AS ("
        "  SELECT execution_id FROM agent_final_delivery_outbox "
        "  WHERE ((state = 'pending' "
        "          AND COALESCE(next_attempt_at, NOW()) <= NOW()) "
        "         OR state = 'sending') "
        "    AND (lease_expires_at IS NULL OR lease_expires_at <= NOW()) "
        "    AND ($4::text IS NULL OR delivery->>'platform' = $4) "
        "  ORDER BY created_at ASC "
        "  LIMIT $1 "
        "  FOR UPDATE SKIP LOCKED"
        "), claimed AS ("
        "  UPDATE agent_final_delivery_outbox o "
        "  SET state = 'sending', lease_owner = $2, "
        "      lease_expires_at = NOW() + make_interval(secs => $3), "
        "      last_attempt_at = NOW(), attempt_count = o.attempt_count + 1, updated_at = NOW() "
        "  FROM candidates c "
        "  WHERE o.execution_id = c.execution_id "
        "  RETURNING o.execution_id, o.thread_key, o.delivery, o.final_payload, o.attempt_count"
        ") "
        "SELECT claimed.*, COALESCE(tt.trace_id, ss.trace_id)::text AS trace_id "
        "FROM claimed "
        "LEFT JOIN thread_traces tt ON tt.thread_key = claimed.thread_key "
        "LEFT JOIN sandbox_sessions ss ON ss.thread_key = claimed.thread_key",
        limit,
        body.consumer_id,
        lease_seconds,
        platform,
    )
    deliveries = []
    for row in rows:
        delivery = row["delivery"]
        payload = row["final_payload"]
        if isinstance(delivery, str):
            delivery = _json.loads(delivery)
        if isinstance(payload, str):
            payload = _json.loads(payload)
        deliveries.append(
            {
                "execution_id": row["execution_id"],
                "thread_key": row["thread_key"],
                "trace_id": row["trace_id"],
                "traceparent": traceparent_from_trace_id(row["trace_id"]),
                "attempt_count": int(row["attempt_count"]),
                "delivery": delivery,
                "final_payload": payload,
            }
        )
        log.info(
            "final_delivery_claimed",
            execution_id=row["execution_id"],
            thread_key=row["thread_key"],
            trace_id=row["trace_id"],
            consumer_id=body.consumer_id,
            attempt_count=int(row["attempt_count"]),
            platform=(delivery or {}).get("platform") if isinstance(delivery, dict) else None,
        )
    return {"deliveries": deliveries}


class RenewFinalDeliveryLeaseRequest(BaseModel):
    consumer_id: str
    lease_seconds: int = 60


@router.post(
    "/final-deliveries/{execution_id}/heartbeat",
    dependencies=[Depends(require_scope("agent:execute"))],
)
async def renew_final_delivery_lease(
    request: Request,
    execution_id: str,
    body: RenewFinalDeliveryLeaseRequest,
):
    pool = request.app.state.db_pool
    lease_seconds = max(15, min(body.lease_seconds, 600))
    row = await pool.fetchrow(
        "UPDATE agent_final_delivery_outbox "
        "SET lease_expires_at = NOW() + make_interval(secs => $3), "
        "    updated_at = NOW() "
        "WHERE execution_id = $1 "
        "  AND state = 'sending' "
        "  AND lease_owner = $2 "
        "RETURNING execution_id, thread_key",
        execution_id,
        body.consumer_id,
        lease_seconds,
    )
    if not row:
        raise HTTPException(status_code=409, detail="delivery not claimable")
    return {"ok": True, "execution_id": execution_id}


class MarkFinalDeliveredRequest(BaseModel):
    consumer_id: str | None = None


@router.post(
    "/final-deliveries/{execution_id}/delivered",
    dependencies=[Depends(require_scope("agent:execute"))],
)
async def mark_final_delivered(
    request: Request,
    execution_id: str,
    body: MarkFinalDeliveredRequest,
):
    pool = request.app.state.db_pool
    owner_check = ""
    params: list[Any] = [execution_id]
    if body.consumer_id:
        owner_check = " AND lease_owner = $2"
        params.append(body.consumer_id)

    row = await pool.fetchrow(
        (
            "UPDATE agent_final_delivery_outbox SET state = 'delivered', delivered_at = NOW(), "
            "lease_owner = NULL, lease_expires_at = NULL, updated_at = NOW() "
            "WHERE execution_id = $1 AND state <> 'delivered'"
        )
        + owner_check
        + " RETURNING execution_id, thread_key",
        *params,
    )
    if not row:
        existing = await pool.fetchrow(
            "SELECT thread_key, state FROM agent_final_delivery_outbox WHERE execution_id = $1",
            execution_id,
        )
        if existing and existing["state"] == "delivered":
            return {"ok": True, "execution_id": execution_id, "idempotent": True}
        raise HTTPException(status_code=409, detail="delivery not claimable")

    await pool.execute(
        "INSERT INTO agent_execution_events (thread_key, execution_id, event_kind, event_json) "
        "VALUES ($1, $2, 'final_delivery_delivered', $3::jsonb)",
        row["thread_key"],
        execution_id,
        canonical_json(
            {
                "type": "final_delivery.delivered",
                "execution_id": execution_id,
                "thread_key": row["thread_key"],
            }
        ),
    )
    log.info(
        "final_delivery_delivered",
        execution_id=execution_id,
        thread_key=row["thread_key"],
        consumer_id=body.consumer_id,
    )
    return {"ok": True, "execution_id": execution_id}


class MarkFinalFailedRequest(BaseModel):
    consumer_id: str | None = None
    error: str
    retry_after_seconds: int = 15
    non_retryable: bool = False
    error_class: str | None = None


@router.post(
    "/final-deliveries/{execution_id}/failed",
    dependencies=[Depends(require_scope("agent:execute"))],
)
async def mark_final_failed(
    request: Request,
    execution_id: str,
    body: MarkFinalFailedRequest,
):
    pool = request.app.state.db_pool
    delay = max(5, min(body.retry_after_seconds, 600))
    last_error = format_last_error(body.error, body.error_class)
    active_lease_required = requires_delivery_lease(
        non_retryable=body.non_retryable,
        error_class=body.error_class,
    )
    if active_lease_required and not body.consumer_id:
        raise HTTPException(status_code=409, detail="non-retryable delivery failures require an active lease")
    owner_check = ""
    params: list[Any] = [execution_id, last_error]
    if body.consumer_id:
        owner_check = " AND lease_owner = $3"
        if active_lease_required:
            owner_check += " AND state = 'sending' AND lease_expires_at > NOW()"
        params.append(body.consumer_id)

    async with pool.acquire() as conn:
        async with conn.transaction():
            row = await conn.fetchrow(
                (
                    "UPDATE agent_final_delivery_outbox SET last_error = $2, updated_at = NOW() "
                    "WHERE execution_id = $1"
                )
                + owner_check
                + " RETURNING execution_id, thread_key, attempt_count, last_error",
                *params,
            )
            if not row:
                raise HTTPException(status_code=409, detail="delivery not claimable")
            dead_letter = should_dead_letter_failure(
                non_retryable=body.non_retryable,
                error_class=body.error_class,
                attempt_count=int(row["attempt_count"]),
                max_attempts=FINAL_DELIVERY_MAX_ATTEMPTS,
            )
            update_row = await conn.fetchrow(
                "UPDATE agent_final_delivery_outbox SET "
                "state = $2::text, "
                "next_attempt_at = CASE WHEN $2::text = 'dead_letter' THEN NULL "
                "  ELSE NOW() + make_interval(secs => $3) END, "
                "lease_owner = NULL, lease_expires_at = NULL, updated_at = NOW() "
                "WHERE execution_id = $1 "
                "RETURNING execution_id, thread_key, state, attempt_count, last_error",
                execution_id,
                "dead_letter" if dead_letter else "pending",
                delay,
            )
            if not update_row:
                raise HTTPException(status_code=409, detail="delivery not claimable")
    if update_row["state"] == "dead_letter":
        log.warning(
            "final_delivery_dead_lettered",
            execution_id=execution_id,
            thread_key=update_row["thread_key"],
            attempt_count=update_row["attempt_count"],
            last_error=update_row["last_error"],
            error_class=body.error_class,
            non_retryable=body.non_retryable,
        )
    else:
        log.warning(
            "final_delivery_failed",
            execution_id=execution_id,
            thread_key=update_row["thread_key"],
            consumer_id=body.consumer_id,
            retry_after_seconds=delay,
            error=update_row["last_error"],
            error_class=body.error_class,
            non_retryable=body.non_retryable,
        )
    return {"ok": True, "execution_id": execution_id}


@router.get("/pool", dependencies=[Depends(require_scope("admin"))])
async def pool():
    """Return warm pool diagnostics."""
    return pool_status()


@router.post("/pool/replenish", dependencies=[Depends(require_scope("admin"))])
async def pool_replenish():
    """Manually trigger pool replenishment."""
    spawned = await replenish_pool()
    return {"spawned": spawned, **pool_status()}


@router.get("/threads", dependencies=[Depends(require_scope("agent:status"))])
async def list_threads(request: Request, limit: int = 200):
    """List threads with summary info."""
    claims = get_sandbox_claims(request)
    if claims is not None:
        raise HTTPException(status_code=403, detail="Sandbox tokens cannot list all threads")
    pool = request.app.state.db_pool
    limit = min(limit, 500)
    rows = await pool.fetch(
        """
        WITH thread_summary AS (
            SELECT
                cm.thread_key,
                MIN(cm.created_at) AS created_at,
                MAX(cm.created_at) AS last_activity,
                COUNT(*)::int AS message_count
            FROM chat_messages cm
            GROUP BY cm.thread_key
            ORDER BY MAX(cm.created_at) DESC
            LIMIT $1
        )
        SELECT
            ts.thread_key,
            ts.created_at,
            ts.last_activity,
            ts.message_count,
            first_user.text AS first_user_text,
            last_user.text AS last_user_text,
            ss.thread_name,
            COALESCE(ss.harness, 'amp') AS harness,
            COALESCE(ss.state, 'stopped') AS state
        FROM thread_summary ts
        LEFT JOIN sandbox_sessions ss ON ss.thread_key = ts.thread_key
        LEFT JOIN LATERAL (
            SELECT (
                SELECT left(part.elem->>'text', 500)
                FROM jsonb_array_elements(cm2.parts) AS part(elem)
                WHERE jsonb_typeof(part.elem) = 'object'
                  AND part.elem ? 'text'
                  AND jsonb_typeof(part.elem->'text') = 'string'
                LIMIT 1
            ) AS text
            FROM chat_messages cm2
            WHERE cm2.thread_key = ts.thread_key AND cm2.role = 'user'
            ORDER BY cm2.created_at ASC
            LIMIT 1
        ) first_user ON TRUE
        LEFT JOIN LATERAL (
            SELECT (
                SELECT left(part.elem->>'text', 500)
                FROM jsonb_array_elements(cm3.parts) AS part(elem)
                WHERE jsonb_typeof(part.elem) = 'object'
                  AND part.elem ? 'text'
                  AND jsonb_typeof(part.elem->'text') = 'string'
                LIMIT 1
            ) AS text
            FROM chat_messages cm3
            WHERE cm3.thread_key = ts.thread_key AND cm3.role = 'user'
            ORDER BY cm3.created_at DESC
            LIMIT 1
        ) last_user ON TRUE
        ORDER BY ts.last_activity DESC
        """,
        limit,
    )

    threads = []
    for row in rows:
        threads.append({
            "slack_thread_key": row["thread_key"],
            "harness": row["harness"],
            "state": row["state"],
            "created_at": row["created_at"].timestamp() if row["created_at"] else None,
            "last_activity": row["last_activity"].timestamp() if row["last_activity"] else None,
            "turn_count": row["message_count"],
            "first_message": row["first_user_text"],
            "last_user_message": row["last_user_text"],
            "thread_name": row["thread_name"],
        })

    return {"threads": threads}


@router.get("/threads/detail", dependencies=[Depends(require_scope("agent:status"))])
async def thread_detail(request: Request, key: str):
    """Get detailed info for a single thread."""
    enforce_sandbox_thread_scope(request, key, write=False)
    pool = request.app.state.db_pool

    rows = await pool.fetch(
        """
        SELECT
            MIN(cm.created_at) AS created_at,
            MAX(cm.created_at) AS last_activity,
            COUNT(*)::int AS message_count,
            (SELECT parts FROM chat_messages cm2
             WHERE cm2.thread_key = $1 AND cm2.role = 'user'
             ORDER BY cm2.created_at DESC LIMIT 1
            ) AS last_user_parts,
            ss.thread_name,
            COALESCE(ss.harness, 'amp') AS harness,
            COALESCE(ss.state, 'stopped') AS state
        FROM chat_messages cm
        LEFT JOIN sandbox_sessions ss ON ss.thread_key = cm.thread_key
        WHERE cm.thread_key = $1
        GROUP BY ss.thread_name, ss.harness, ss.state
        """,
        key,
    )

    if not rows:
        raise HTTPException(status_code=404, detail=f"Thread not found: {key}")

    row = rows[0]

    def _extract_text(parts):
        if isinstance(parts, str):
            try:
                parts = _json.loads(parts)
            except Exception:
                return None
        if not isinstance(parts, list):
            return None
        for p in parts:
            if isinstance(p, dict) and isinstance(p.get("text"), str):
                return p["text"]
        return None

    detail = {
        "slack_thread_key": key,
        "harness": row["harness"],
        "state": row["state"],
        "created_at": row["created_at"].timestamp() if row["created_at"] else None,
        "last_activity": row["last_activity"].timestamp() if row["last_activity"] else None,
        "message_count": row["message_count"],
        "last_user_message": _extract_text(row["last_user_parts"]),
        "thread_name": row["thread_name"],
    }

    # Collect participants and token usage from message rows
    msg_rows = await pool.fetch(
        "SELECT parts, metadata FROM chat_messages "
        "WHERE thread_key = $1 ORDER BY created_at DESC LIMIT 200",
        key,
    )

    participants = {}
    total_tokens = 0
    total_input = 0
    total_output = 0
    total_cost = 0.0
    has_usage = False
    models = set()

    for mrow in msg_rows:
        parts = mrow["parts"]
        if isinstance(parts, str):
            try:
                parts = _json.loads(parts)
            except Exception:
                parts = []
        meta = mrow["metadata"]
        if isinstance(meta, str):
            try:
                meta = _json.loads(meta)
            except Exception:
                meta = None

        # Participants
        if meta and isinstance(meta, dict):
            uid = meta.get("user_id")
            if uid and isinstance(uid, str) and uid.strip():
                uid = uid.strip()
                if uid not in participants:
                    participants[uid] = {
                        "id": uid,
                        "name": meta.get("user_name") or meta.get("name") or uid,
                        "username": meta.get("username"),
                        "avatar_url": meta.get("avatar_url"),
                    }
            # Token usage from metadata
            tu = meta.get("token_usage")
            if tu and isinstance(tu, dict):
                t = tu.get("total_tokens", 0)
                if isinstance(t, (int, float)) and t > 0:
                    has_usage = True
                    total_tokens += int(t)
                    inp = tu.get("input_tokens")
                    if isinstance(inp, (int, float)):
                        total_input += int(inp)
                    out = tu.get("output_tokens")
                    if isinstance(out, (int, float)):
                        total_output += int(out)
                    c = tu.get("cost_usd")
                    if isinstance(c, (int, float)):
                        total_cost += c
                    for m in (tu.get("models") or []):
                        if isinstance(m, str):
                            models.add(m)

        if isinstance(parts, list):
            for part in parts:
                if not isinstance(part, dict):
                    continue
                if part.get("type") == "data-user-message" or part.get("type") == "data-context-message":
                    data = part.get("data") or {}
                    uid = data.get("user_id")
                    if uid and isinstance(uid, str) and uid.strip():
                        uid = uid.strip()
                        if uid not in participants:
                            participants[uid] = {
                                "id": uid,
                                "name": data.get("user_name") or data.get("name") or uid,
                                "username": data.get("username"),
                                "avatar_url": data.get("avatar_url"),
                            }
                if part.get("type") == "data-token-usage":
                    tu = part.get("data") or {}
                    t = tu.get("total_tokens", 0)
                    if isinstance(t, (int, float)) and t > 0:
                        has_usage = True
                        total_tokens += int(t)
                        inp = tu.get("input_tokens")
                        if isinstance(inp, (int, float)):
                            total_input += int(inp)
                        out = tu.get("output_tokens")
                        if isinstance(out, (int, float)):
                            total_output += int(out)
                        c = tu.get("cost_usd")
                        if isinstance(c, (int, float)):
                            total_cost += c
                        for m in (tu.get("models") or []):
                            if isinstance(m, str):
                                models.add(m)

    detail["participants"] = list(participants.values())
    detail["token_usage"] = {
        "total_tokens": total_tokens,
        "input_tokens": total_input or None,
        "output_tokens": total_output or None,
        "cost_usd": total_cost if total_cost > 0 else None,
        "models": sorted(models),
    } if has_usage else None

    return detail


# ── Internal DB query (read-only) ───────────────────────────────────────────

_ALLOWED_TABLES = {
    "chat_messages", "sandbox_sessions", "attachments", "api_keys",
    "agent_runtime_assignments", "agent_message_requests",
    "agent_execution_requests", "agent_execution_events",
    "agent_final_delivery_outbox", "agent_spawn_requests",
    "agent_release_requests",
}
_BLOCKED_PATTERNS = {"drop ", "delete ", "insert ", "update ", "alter ", "create ", "truncate ", "grant ", "revoke "}


@router.post("/query", dependencies=[Depends(verify_api_key)])
async def query_db(request: Request):
    """Run a read-only SQL query against Centaur's own database.

    Only SELECT on allowed tables. Returns rows as JSON.
    """
    claims = get_sandbox_claims(request)
    if claims is not None:
        raise HTTPException(status_code=403, detail="Sandbox tokens cannot run direct queries")
    body = await request.json()
    sql = (body.get("sql") or "").strip()
    if not sql:
        raise HTTPException(status_code=422, detail="sql is required")

    sql_lower = sql.lower()
    if not sql_lower.startswith("select"):
        raise HTTPException(status_code=400, detail="Only SELECT queries are allowed")
    for pat in _BLOCKED_PATTERNS:
        if pat in sql_lower:
            raise HTTPException(status_code=400, detail=f"Query contains blocked keyword: {pat.strip()}")

    pool = request.app.state.db_pool
    try:
        rows = await pool.fetch(sql)
    except Exception as exc:
        raise HTTPException(status_code=400, detail=str(exc))

    # Convert to JSON-serializable dicts
    results = []
    for row in rows[:500]:
        record = {}
        for key, val in row.items():
            if isinstance(val, (bytes, bytearray, memoryview)):
                record[key] = f"<{len(val)} bytes>"
            elif hasattr(val, "isoformat"):
                record[key] = val.isoformat()
            else:
                record[key] = val
        results.append(record)

    return {"rows": results, "count": len(results)}
