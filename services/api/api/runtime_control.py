"""Durable runtime control-plane helpers for spawn/message/execute flows."""

from __future__ import annotations

import asyncio
import base64
import contextlib
import datetime as dt
import hashlib
import json
import os
import pathlib
import uuid
from typing import Any

import structlog
from opentelemetry import trace

from api.agent import (
    _drop_runtime,
    _get_runtime,
    _stream_stdout,
    get_or_spawn,
    inject_stdin,
    steer_stdin,
    stop_session,
)
from api import slackbot_client
from api.harness_config import default_harness
from api.otel import (
    add_span_event,
    context_from_serialized,
    current_traceparent,
    laminar_trace_metadata_attributes,
    mark_error,
    record_exception,
    set_laminar_trace_metadata,
    set_span_attributes,
    span_context_to_dict,
    start_span,
)
from api.observability import (
    ExecutionObservationAccumulator,
    extract_usage_metrics,
    payload_size_bytes,
    project_execution_observations,
    summarize_message_parts,
)
from api.vm_metrics import (
    record_agent_execution,
    record_execution_by_user,
    record_execution_claimed,
    record_execution_enqueued,
    record_execution_terminal,
    record_execution_watchdog_timeout,
    record_message_observation,
    record_oneshot,
    record_tool_error_category,
    record_ttft,
    record_usage_observation,
)
from api.sandbox.normalize import normalize_harness_event
from api.sandbox.harness_protocol import extract_result
from api.sandbox.registry import get_backend

log = structlog.get_logger()

_SLACKBOT_LIVE_DELIVERY_METADATA_KEY = "slackbot_live_delivery"
_LEGACY_SLACKBOT_LIVE_DELIVERY_METADATA_KEY = "slackbot" + "_v" + "2_live_delivery"

EXECUTION_SILENCE_TIMEOUT_S = int(os.getenv("EXECUTION_SILENCE_TIMEOUT_S", "600"))
EXECUTION_TOOL_SILENCE_TIMEOUT_S = int(
    os.getenv("EXECUTION_TOOL_SILENCE_TIMEOUT_S", "1800")
)
EXECUTION_HARD_TIMEOUT_S = int(os.getenv("EXECUTION_HARD_TIMEOUT_S", "3600"))
EXECUTION_WATCHDOG_POLL_S = float(os.getenv("EXECUTION_WATCHDOG_POLL_S", "1.0"))
EXECUTION_RECONCILE_INTERVAL_S = float(
    os.getenv("EXECUTION_RECONCILE_INTERVAL_S", "0.5")
)
THREAD_FAILURE_LOOP_WINDOW_S = int(os.getenv("THREAD_FAILURE_LOOP_WINDOW_S", "300"))
THREAD_FAILURE_LOOP_THRESHOLD = int(os.getenv("THREAD_FAILURE_LOOP_THRESHOLD", "3"))
EXECUTION_STREAM_EOF_RETRY_DELAY_S = max(
    float(os.getenv("EXECUTION_STREAM_EOF_RETRY_DELAY_S", "1.0")),
    0.0,
)
EXECUTION_STALE_RECOVERY_INTERVAL_S = float(
    os.getenv("EXECUTION_STALE_RECOVERY_INTERVAL_S", "5.0")
)
_RECONCILE_STARTUP_LIMIT = max(
    int(os.getenv("EXECUTION_RECONCILE_STARTUP_LIMIT", "500")),
    1,
)
EXECUTION_WORKER_CONCURRENCY = max(
    int(os.getenv("EXECUTION_WORKER_CONCURRENCY", "128")),
    1,
)
# Number of execution worker slots reserved for non-workflow (user-facing)
# requests.  Workflow-spawned executions cannot consume more than
# EXECUTION_WORKER_CONCURRENCY - EXECUTION_RESERVED_USER_SLOTS slots.
EXECUTION_RESERVED_USER_SLOTS = max(
    int(os.getenv("EXECUTION_RESERVED_USER_SLOTS", "16")),
    0,
)
_MAX_SLACKBOT_TEXT_CHARS = 12_000
_MAX_SLACKBOT_STEP_CHARS = 12_000
_MAX_WORKFLOW_EXECUTION_SLOTS = max(
    EXECUTION_WORKER_CONCURRENCY - EXECUTION_RESERVED_USER_SLOTS,
    1,
)
EXECUTION_WORKER_LEASE_S = max(
    float(os.getenv("EXECUTION_WORKER_LEASE_S", "60.0")),
    max(EXECUTION_WATCHDOG_POLL_S * 2, 1.0),
)
FINAL_DELIVERY_READY_GRACE_S = max(
    float(os.getenv("FINAL_DELIVERY_READY_GRACE_S", "10.0")),
    0.0,
)
WORKER_INSTANCE_ID = f"{os.getenv('HOSTNAME') or 'api'}:{uuid.uuid4().hex[:8]}"

_worker_tasks: list[asyncio.Task] = []
_worker_wake = asyncio.Event()
_recover_stale_running_lock = asyncio.Lock()
_last_recover_stale_running_at = 0.0

_RAW_HARNESS_AUTH_SIGNATURE = "Unauthorized Check your access token."
_RAW_HARNESS_AUTH_RETRY_LIMIT = 1
_RAW_HARNESS_AUTH_RETRY_METADATA_KEY = "control_plane_retry"
_RAW_HARNESS_AUTH_RETRY_REASON = "harness_auth"
_RAW_HARNESS_AUTH_SAFE_FAILURE_MESSAGE = (
    "The agent hit a temporary runtime startup issue and could not complete the turn. "
    "Please retry in a moment."
)
_OTEL_METADATA_KEY = "_otel"
_OTEL_EXECUTION_SPAN_CONTEXT_KEY = "execution_span_context"


class ControlPlaneError(RuntimeError):
    def __init__(self, code: str, message: str, status_code: int = 409):
        super().__init__(message)
        self.code = code
        self.message = message
        self.status_code = status_code


def canonical_json(value: Any) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True, ensure_ascii=False)


def request_hash(value: Any) -> str:
    return hashlib.sha256(canonical_json(value).encode("utf-8")).hexdigest()


def decode_jsonb(value: Any, fallback: Any) -> Any:
    if isinstance(value, (dict, list)):
        return value
    if isinstance(value, str):
        with contextlib.suppress(json.JSONDecodeError, TypeError):
            return json.loads(value)
    return fallback


def _matches_raw_harness_auth_failure(*values: str | None) -> bool:
    expected = _RAW_HARNESS_AUTH_SIGNATURE.casefold()
    for value in values:
        if not isinstance(value, str):
            continue
        normalized = " ".join(value.strip().casefold().split())
        if normalized == expected.casefold():
            return True
        if "unauthorized" in normalized and "access token" in normalized:
            return True
    return False


def _matches_user_cancelled(*values: str | None) -> bool:
    for value in values:
        if not isinstance(value, str):
            continue
        if "user cancelled (sigint/sigterm)" in value.casefold():
            return True
    return False


def _raw_harness_auth_retry_attempt(metadata: dict[str, Any]) -> int:
    retry_metadata = metadata.get(_RAW_HARNESS_AUTH_RETRY_METADATA_KEY)
    if not isinstance(retry_metadata, dict):
        return 0
    if str(retry_metadata.get("reason") or "") != _RAW_HARNESS_AUTH_RETRY_REASON:
        return 0
    attempt = retry_metadata.get("attempt")
    with contextlib.suppress(TypeError, ValueError):
        return max(int(attempt), 0)
    return 0


def prompt_identity(
    *, harness: str, persona_id: str | None, agents_md_override: str | None
) -> tuple[str, str]:
    prompt_ref = f"persona:{persona_id}" if persona_id else f"harness:{harness}"
    effective = agents_md_override if agents_md_override is not None else prompt_ref
    sha = hashlib.sha256(effective.encode("utf-8")).hexdigest()
    return prompt_ref, sha


def _agent_session_title(
    *, persona_id: str | None, engine: str | None, harness: str | None
) -> str:
    parts = ["Centaur"]
    persona = (persona_id or "").strip()
    runtime = (engine or harness or default_harness()).strip()
    if persona:
        parts.append(persona)
    if runtime and runtime != persona:
        parts.append(runtime)
    return " · ".join(parts)


# ── Per-message header (rendered italic at the top of every assistant message) ──

_CLAUDE_MODEL_ALIASES: dict[str, str] = {
    "opus": "claude-opus-4-7",
    "sonnet": "claude-sonnet-4-6",
    "haiku": "claude-haiku-4-5",
}


def _resolve_claude_model_label(model: str | None) -> str:
    raw = (model or os.getenv("CLAUDE_MODEL") or "opus").strip().lower()
    if raw.startswith("claude-"):
        return raw
    return _CLAUDE_MODEL_ALIASES.get(raw, raw or "claude-opus-4-7")


def _resolve_codex_model_label(model: str | None) -> str:
    raw = (model or os.getenv("CODEX_MODEL") or "").strip().lower()
    if not raw:
        return "codex"
    if raw.startswith("codex-"):
        return raw
    return f"codex-{raw}"


def _engine_model_label(
    *,
    engine: str | None,
    harness: str | None,
    model: str | None = None,
) -> str:
    """Return the `engine-model` segment for the per-message header.

    Falls back to the engine identifier when no model is known, so the
    header always renders something deterministic per (engine, persona) pair.
    """
    engine_name = (engine or harness or "").strip().lower()
    explicit = (model or "").strip().lower()
    if engine_name == "claude-code":
        return _resolve_claude_model_label(explicit or None)
    if engine_name == "codex":
        return _resolve_codex_model_label(explicit or None)
    if engine_name == "amp":
        return f"amp-{explicit}" if explicit else "amp"
    if explicit:
        return explicit
    return engine_name or "centaur"


def _agent_session_header(
    *,
    persona_id: str | None,
    engine: str | None,
    harness: str | None,
    model: str | None = None,
) -> str:
    """Return ``"<persona> · <engine-model>"`` for the per-message header line.

    Persona is normalized to the literal ``"base"`` when no persona is
    active so the header is always two segments. The slackbot wraps this
    in italics; we do not add markdown styling here.
    """
    persona = (persona_id or "").strip() or "base"
    engine_label = _engine_model_label(engine=engine, harness=harness, model=model)
    return f"{persona} · {engine_label}"


def flatten_event_parts(event: dict[str, Any]) -> list[dict[str, Any]]:
    message = event.get("message") if isinstance(event, dict) else None
    if not isinstance(message, dict):
        return []
    content = message.get("content")
    if isinstance(content, list):
        return [p for p in content if isinstance(p, dict)]
    return []


def event_role(event: dict[str, Any]) -> str:
    message = event.get("message") if isinstance(event, dict) else None
    if isinstance(message, dict):
        role = message.get("role")
        if isinstance(role, str) and role:
            return role
    return "user"


def _event_silence_timeout_s(event: dict[str, Any]) -> float:
    return _progress_silence_timeout_s(event, canonical_events=None, observations=None)


def _tool_silence_timeout_s() -> float:
    return float(max(EXECUTION_TOOL_SILENCE_TIMEOUT_S, EXECUTION_SILENCE_TIMEOUT_S))


def _canonical_event_extends_tool_timeout(event: dict[str, Any]) -> bool:
    event_type = str(event.get("type") or "")
    if event_type != "command_execution":
        return False
    status = str(event.get("status") or "").strip().lower()
    if status in {"working", "running", "in_progress", "progress"}:
        return True
    output = event.get("aggregated_output")
    return isinstance(output, str) and bool(output.strip())


def _progress_silence_timeout_s(
    event: dict[str, Any],
    *,
    canonical_events: list[dict[str, Any]] | None,
    observations: ExecutionObservationAccumulator | None,
) -> float:
    if observations and observations.active_tool_use_ids:
        return _tool_silence_timeout_s()
    parts = flatten_event_parts(event)
    if any(part.get("type") == "tool_use" for part in parts):
        return _tool_silence_timeout_s()
    if canonical_events and any(
        _canonical_event_extends_tool_timeout(canonical_event)
        for canonical_event in canonical_events
    ):
        return _tool_silence_timeout_s()
    return float(EXECUTION_SILENCE_TIMEOUT_S)


def _extract_repo_context(source: Any) -> dict[str, str]:
    payload = source if isinstance(source, dict) else {}
    repo_context: dict[str, str] = {}
    for key in ("cwd", "repo_owner", "repo_name", "git_ref", "git_commit"):
        value = payload.get(key)
        if isinstance(value, str) and value.strip():
            repo_context[key] = value.strip()
    return repo_context


async def _merge_execution_repo_context(
    pool,
    execution_id: str,
    repo_context: dict[str, str],
) -> None:
    if not repo_context:
        return
    await pool.execute(
        "UPDATE agent_execution_requests SET metadata = metadata || $1::jsonb, updated_at = NOW() "
        "WHERE execution_id = $2",
        canonical_json({"repo_context": repo_context}),
        execution_id,
    )


async def _merge_execution_metadata(
    pool,
    execution_id: str,
    metadata: dict[str, Any],
) -> None:
    if not metadata:
        return
    await pool.execute(
        "UPDATE agent_execution_requests SET metadata = metadata || $1::jsonb, updated_at = NOW() "
        "WHERE execution_id = $2",
        canonical_json(metadata),
        execution_id,
    )


def _attachment_name_from_source_path(
    source_path: str | None, attachment_id: str
) -> str:
    if not source_path:
        return f"{attachment_id}.bin"
    with contextlib.suppress(Exception):
        parsed = pathlib.PurePosixPath(source_path)
        if parsed.name:
            return parsed.name
    return f"{attachment_id}.bin"


def _metadata_platform(metadata: dict[str, Any]) -> str | None:
    platform = metadata.get("platform") if isinstance(metadata, dict) else None
    return platform if isinstance(platform, str) and platform else None


def _delivery_platform(delivery: dict[str, Any]) -> str | None:
    platform = delivery.get("platform") if isinstance(delivery, dict) else None
    return platform if isinstance(platform, str) and platform else None


def _deployment_environment() -> str:
    return (
        os.getenv("CENTAUR_ENVIRONMENT")
        or os.getenv("DEPLOY_ENV")
        or os.getenv("ENVIRONMENT")
        or "local"
    ).strip() or "local"


def _slack_thread_metadata(thread_key: str) -> dict[str, str]:
    parts = thread_key.split(":", 2)
    if len(parts) != 3 or parts[0] != "slack":
        return {}
    return {
        "slack_channel_id": parts[1],
        "slack_thread_ts": parts[2],
    }


def _execution_laminar_metadata(
    *,
    thread_key: str,
    execution_id: str,
    execute_id: Any = None,
    assignment_generation: int | None = None,
    delivery: dict[str, Any] | None = None,
    execution_metadata: dict[str, Any] | None = None,
    harness: Any = None,
    engine: Any = None,
    persona_id: Any = None,
    prompt_ref: Any = None,
    runtime_id: Any = None,
    user_id: Any = None,
) -> dict[str, Any]:
    delivery = delivery if isinstance(delivery, dict) else {}
    execution_metadata = (
        execution_metadata if isinstance(execution_metadata, dict) else {}
    )
    effective_user_id = (
        user_id
        or delivery.get("recipient_user_id")
        or delivery.get("user_id")
        or execution_metadata.get("user_id")
    )
    metadata: dict[str, Any] = {
        "app": "centaur",
        "environment": _deployment_environment(),
        "thread_key": thread_key,
        "execution_id": execution_id,
        "execute_id": execute_id,
        "assignment_generation": assignment_generation,
        "platform": _delivery_platform(delivery),
        "harness": harness,
        "engine": engine,
        "persona_id": persona_id,
        "prompt_ref": prompt_ref,
        "runtime_id": runtime_id,
        "user_id": effective_user_id,
    }
    metadata.update(_slack_thread_metadata(thread_key))
    channel = (
        delivery.get("channel") if isinstance(delivery.get("channel"), str) else None
    )
    if channel and not metadata.get("slack_channel_id"):
        metadata["slack_channel_id"] = channel
    return metadata


async def _write_agents_override(runtime_id: str, agents_md_override: str) -> None:
    backend = get_backend()
    code, output = await backend.exec_run(
        runtime_id,
        [
            "sh",
            "-c",
            "mkdir -p /home/agent/workspace && printf '%s' \"$_CONTENT\" > /home/agent/workspace/AGENTS.md",
        ],
        environment={"_CONTENT": agents_md_override},
        user="agent",
    )
    if code != 0:
        raise ControlPlaneError(
            "AGENTS_OVERRIDE_FAILED",
            f"failed to apply AGENTS override: {output.decode('utf-8', errors='replace')[:200]}",
            500,
        )


async def get_active_assignment(pool, thread_key: str) -> dict[str, Any] | None:
    row = await pool.fetchrow(
        "SELECT thread_key, assignment_generation, runtime_id, harness, engine, persona_id, "
        "prompt_ref, effective_agents_md_sha256, agents_md_override, state "
        "FROM agent_runtime_assignments "
        "WHERE thread_key = $1 AND state = 'active' "
        "ORDER BY assignment_generation DESC LIMIT 1",
        thread_key,
    )
    return dict(row) if row else None


async def spawn_assignment(
    pool,
    *,
    thread_key: str,
    spawn_id: str,
    harness: str | None,
    engine: str | None,
    persona_id: str | None,
    agents_md_override: str | None,
) -> dict[str, Any]:
    persona_info = None
    harness_selector = (harness or "").strip() or None
    if harness_selector and persona_id is None:
        from api.app import get_tool_manager

        harness_persona = get_tool_manager().get_persona(harness_selector)
        if harness_persona is not None:
            persona_id = harness_persona.name
            harness = None
            persona_info = harness_persona

    if persona_id:
        from api.app import get_tool_manager

        persona_info = persona_info or get_tool_manager().get_persona(persona_id)
        if persona_info is None:
            raise ControlPlaneError(
                "UNKNOWN_PERSONA_ID",
                f"unknown persona_id: {persona_id}",
                422,
            )

    attach_active_assignment = (
        harness is None
        and engine is None
        and persona_id is None
        and agents_md_override is None
    )
    active_assignment = (
        await get_active_assignment(pool, thread_key)
        if attach_active_assignment
        else None
    )

    if active_assignment:
        effective_harness = active_assignment.get("harness") or default_harness()
        effective_engine = active_assignment.get("engine")
        effective_persona_id = active_assignment.get("persona_id")
        effective_agents_md_override = active_assignment.get("agents_md_override")
    else:
        # Explicit harness wins; otherwise inherit from the persona's declared
        # engine; otherwise use the deployment default.
        if harness:
            effective_harness = harness
        elif persona_info is not None:
            effective_harness = persona_info.engine
        else:
            effective_harness = default_harness()
        effective_engine = engine
        effective_persona_id = persona_id
        effective_agents_md_override = agents_md_override

    payload = {
        "thread_key": thread_key,
        "spawn_id": spawn_id,
        "harness": effective_harness,
        "engine": effective_engine,
        "persona_id": effective_persona_id,
        "agents_md_override": effective_agents_md_override,
    }
    req_hash = request_hash(payload)

    existing_idem = await pool.fetchrow(
        "SELECT request_hash, response_json FROM agent_spawn_requests "
        "WHERE thread_key = $1 AND spawn_id = $2",
        thread_key,
        spawn_id,
    )
    if existing_idem:
        if existing_idem["request_hash"] != req_hash:
            raise ControlPlaneError(
                "IDEMPOTENCY_PAYLOAD_MISMATCH",
                "spawn_id was already used with a different payload",
                409,
            )
        return decode_jsonb(existing_idem["response_json"], {})

    with start_span(
        "centaur.agent.spawn",
        attributes={
            "centaur.thread_key": thread_key,
            "centaur.harness": effective_harness,
            "centaur.engine": effective_engine,
            "centaur.persona_id": effective_persona_id,
            "centaur.spawn_id": spawn_id,
            "centaur.assignment.attach_active": attach_active_assignment,
        },
    ) as span:
        spawn_kwargs: dict[str, Any] = {"engine": effective_engine}
        if effective_persona_id is not None:
            spawn_kwargs["persona"] = effective_persona_id
        session = await get_or_spawn(
            thread_key,
            effective_harness,
            **spawn_kwargs,
        )
        set_span_attributes(
            span,
            {
                "centaur.runtime_id": session.sandbox_id,
                "centaur.trace_id": session.trace_id,
                "centaur.engine": session.engine,
                "centaur.harness": session.harness,
            },
        )
    if effective_agents_md_override is not None:
        await _write_agents_override(session.sandbox_id, effective_agents_md_override)

    prompt_ref, prompt_sha = prompt_identity(
        harness=session.harness,
        persona_id=effective_persona_id,
        agents_md_override=effective_agents_md_override,
    )

    async with pool.acquire() as conn:
        async with conn.transaction():
            await conn.execute("SELECT pg_advisory_xact_lock(hashtext($1))", thread_key)

            existing_idem = await conn.fetchrow(
                "SELECT request_hash, response_json FROM agent_spawn_requests "
                "WHERE thread_key = $1 AND spawn_id = $2",
                thread_key,
                spawn_id,
            )
            if existing_idem:
                if existing_idem["request_hash"] != req_hash:
                    raise ControlPlaneError(
                        "IDEMPOTENCY_PAYLOAD_MISMATCH",
                        "spawn_id was already used with a different payload",
                        409,
                    )
                return decode_jsonb(existing_idem["response_json"], {})

            active = await conn.fetchrow(
                "SELECT assignment_generation, runtime_id, harness, engine, persona_id, "
                "prompt_ref, effective_agents_md_sha256, agents_md_override "
                "FROM agent_runtime_assignments "
                "WHERE thread_key = $1 AND state = 'active' "
                "ORDER BY assignment_generation DESC LIMIT 1",
                thread_key,
            )

            if active:
                if (
                    active["effective_agents_md_sha256"] != prompt_sha
                    or active["harness"] != session.harness
                ):
                    raise ControlPlaneError(
                        "ACTIVE_ASSIGNMENT_PROMPT_MISMATCH",
                        "active assignment exists with different prompt identity",
                        409,
                    )
                generation = int(active["assignment_generation"])
                runtime_id = active["runtime_id"]
                if runtime_id != session.sandbox_id:
                    await conn.execute(
                        "UPDATE agent_runtime_assignments SET runtime_id = $1, updated_at = NOW() "
                        "WHERE thread_key = $2 AND assignment_generation = $3",
                        session.sandbox_id,
                        thread_key,
                        generation,
                    )
                    runtime_id = session.sandbox_id
                assignment_state = "assigned_idle"
                resolved_persona = active["persona_id"]
                resolved_prompt_ref = active["prompt_ref"]
                resolved_prompt_sha = active["effective_agents_md_sha256"]
            else:
                generation = (
                    int(
                        await conn.fetchval(
                            "SELECT COALESCE(MAX(assignment_generation), 0) "
                            "FROM agent_runtime_assignments WHERE thread_key = $1",
                            thread_key,
                        )
                    )
                    + 1
                )
                await conn.execute(
                    "INSERT INTO agent_runtime_assignments ("
                    "thread_key, assignment_generation, runtime_id, harness, engine, "
                    "persona_id, prompt_ref, effective_agents_md_sha256, agents_md_override, state"
                    ") VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'active')",
                    thread_key,
                    generation,
                    session.sandbox_id,
                    session.harness,
                    session.engine,
                    effective_persona_id,
                    prompt_ref,
                    prompt_sha,
                    effective_agents_md_override,
                )
                runtime_id = session.sandbox_id
                assignment_state = "assigned_idle"
                resolved_persona = effective_persona_id
                resolved_prompt_ref = prompt_ref
                resolved_prompt_sha = prompt_sha

            response = {
                "ok": True,
                "runtime_id": runtime_id,
                "thread_key": thread_key,
                "trace_id": session.trace_id,
                "assignment_state": assignment_state,
                "assignment_generation": generation,
                "persona_id": resolved_persona,
                "prompt_ref": resolved_prompt_ref,
                "effective_agents_md_sha256": resolved_prompt_sha,
            }
            await conn.execute(
                "INSERT INTO agent_spawn_requests (thread_key, spawn_id, request_hash, response_json) "
                "VALUES ($1, $2, $3, $4::jsonb)",
                thread_key,
                spawn_id,
                req_hash,
                canonical_json(response),
            )

            log.info(
                "spawn_completed",
                thread_key=thread_key,
                spawn_id=spawn_id,
                runtime_id=runtime_id,
                assignment_generation=generation,
                harness=session.harness,
                engine=session.engine,
                persona_id=resolved_persona,
                prompt_ref=resolved_prompt_ref,
                prompt_sha=resolved_prompt_sha,
            )
            return response


async def extract_inline_attachments(
    pool,
    *,
    thread_key: str,
    chat_message_id: str,
    parts: list[dict[str, Any]],
) -> tuple[list[dict[str, Any]], list[str]]:
    transformed: list[dict[str, Any]] = []
    attachment_ids: list[str] = []

    for part in parts:
        source = part.get("source") if isinstance(part, dict) else None
        if (
            isinstance(source, dict)
            and source.get("type") == "base64"
            and isinstance(source.get("data"), str)
        ):
            media_type = str(source.get("media_type") or "application/octet-stream")
            try:
                raw = base64.b64decode(source["data"])
            except Exception as exc:
                raise ControlPlaneError(
                    "INVALID_BASE64_ATTACHMENT",
                    f"invalid base64 attachment: {exc}",
                    422,
                ) from exc
            attachment_id = f"att-{uuid.uuid4().hex[:16]}"
            source_path = (
                part.get("source_path")
                if isinstance(part.get("source_path"), str)
                else None
            )
            name = (
                str(part.get("name"))
                if isinstance(part.get("name"), str) and part.get("name")
                else _attachment_name_from_source_path(source_path, attachment_id)
            )
            await pool.execute(
                "INSERT INTO attachments (id, thread_key, message_id, name, mime_type, data) "
                "VALUES ($1, $2, $3, $4, $5, $6)",
                attachment_id,
                thread_key,
                chat_message_id,
                name,
                media_type,
                raw,
            )
            transformed.append(
                {
                    "type": "attachment_ref",
                    "attachment_id": attachment_id,
                    "media_type": media_type,
                    "name": name,
                    **({"source_path": source_path} if source_path else {}),
                }
            )
            attachment_ids.append(attachment_id)
            continue

        transformed.append(part)

    return transformed, attachment_ids


def event_to_chat_parts(parts: list[dict[str, Any]]) -> list[dict[str, Any]]:
    chat_parts: list[dict[str, Any]] = []
    for part in parts:
        part_type = part.get("type")
        if part_type == "text":
            chat_parts.append({"type": "text", "text": str(part.get("text") or "")})
        elif part_type == "attachment_ref":
            attachment_id = str(part.get("attachment_id") or "")
            media_type = str(part.get("media_type") or "application/octet-stream")
            source_path = (
                part.get("source_path")
                if isinstance(part.get("source_path"), str)
                else None
            )
            name = (
                str(part.get("name"))
                if isinstance(part.get("name"), str) and part.get("name")
                else _attachment_name_from_source_path(source_path, attachment_id)
            )
            chat_parts.append(
                {
                    "type": "attachment_ref",
                    "id": attachment_id,
                    "name": name,
                    "mime_type": media_type,
                }
            )
        else:
            chat_parts.append(part)
    return chat_parts


async def append_message(
    pool,
    *,
    thread_key: str,
    assignment_generation: int,
    message_id: str,
    event: dict[str, Any],
    metadata: dict[str, Any],
) -> dict[str, Any]:
    payload = {
        "thread_key": thread_key,
        "assignment_generation": assignment_generation,
        "message_id": message_id,
        "event": event,
        "metadata": metadata,
    }
    req_hash = request_hash(payload)

    existing = await pool.fetchrow(
        "SELECT request_hash FROM agent_message_requests "
        "WHERE thread_key = $1 AND message_id = $2",
        thread_key,
        message_id,
    )
    if existing:
        if existing["request_hash"] != req_hash:
            raise ControlPlaneError(
                "IDEMPOTENCY_PAYLOAD_MISMATCH",
                "message_id was already used with a different payload",
                409,
            )
        return {
            "ok": True,
            "message_id": message_id,
            "stored_event_id": f"{thread_key}:{message_id}",
            "attachment_ids": [],
            "idempotent": True,
        }

    role = event_role(event)
    content_parts = flatten_event_parts(event)
    if not content_parts:
        raise ControlPlaneError(
            "INVALID_AMP_EVENT_ENVELOPE", "event.message.content is required", 400
        )

    chat_message_id = f"msg:{thread_key}:{message_id}"
    attachment_ids: list[str] = []

    async with pool.acquire() as conn:
        async with conn.transaction():
            await conn.execute("SELECT pg_advisory_xact_lock(hashtext($1))", thread_key)

            existing = await conn.fetchrow(
                "SELECT request_hash FROM agent_message_requests "
                "WHERE thread_key = $1 AND message_id = $2",
                thread_key,
                message_id,
            )
            if existing:
                if existing["request_hash"] != req_hash:
                    raise ControlPlaneError(
                        "IDEMPOTENCY_PAYLOAD_MISMATCH",
                        "message_id was already used with a different payload",
                        409,
                    )
                return {
                    "ok": True,
                    "message_id": message_id,
                    "stored_event_id": f"{thread_key}:{message_id}",
                    "attachment_ids": [],
                    "idempotent": True,
                }

            active = await conn.fetchrow(
                "SELECT assignment_generation, harness, engine, persona_id, prompt_ref, effective_agents_md_sha256 "
                "FROM agent_runtime_assignments "
                "WHERE thread_key = $1 AND state = 'active' "
                "ORDER BY assignment_generation DESC LIMIT 1",
                thread_key,
            )
            if not active:
                raise ControlPlaneError(
                    "NO_ACTIVE_ASSIGNMENT",
                    "No active runtime assignment for thread_key",
                    409,
                )
            active_generation = int(active["assignment_generation"])
            if active_generation != int(assignment_generation):
                raise ControlPlaneError(
                    "ASSIGNMENT_GENERATION_STALE",
                    "assignment_generation does not match the active assignment",
                    409,
                )

            await conn.execute(
                "INSERT INTO chat_messages (id, thread_key, role, parts, user_id, metadata) "
                "VALUES ($1, $2, $3, '[]'::jsonb, $4, $5::jsonb) "
                "ON CONFLICT (id) DO NOTHING",
                chat_message_id,
                thread_key,
                role,
                metadata.get("user_id") if isinstance(metadata, dict) else None,
                canonical_json(metadata),
            )

            normalized_parts, attachment_ids = await extract_inline_attachments(
                conn,
                thread_key=thread_key,
                chat_message_id=chat_message_id,
                parts=content_parts,
            )
            normalized_event = {
                "type": str(event.get("type") or "user"),
                "message": {
                    "role": role,
                    "content": normalized_parts,
                },
            }
            chat_parts = event_to_chat_parts(normalized_parts)

            await conn.execute(
                "INSERT INTO agent_message_requests ("
                "thread_key, message_id, assignment_generation, request_hash, event_json, metadata"
                ") VALUES ($1, $2, $3, $4, $5::jsonb, $6::jsonb)",
                thread_key,
                message_id,
                assignment_generation,
                req_hash,
                canonical_json(normalized_event),
                canonical_json(metadata),
            )
            await conn.execute(
                "UPDATE chat_messages SET parts = $1::jsonb, metadata = $2::jsonb WHERE id = $3",
                canonical_json(chat_parts),
                canonical_json(
                    {
                        **metadata,
                        "message_id": message_id,
                        "assignment_generation": assignment_generation,
                    }
                ),
                chat_message_id,
            )

    message_summary = summarize_message_parts(normalized_parts)
    record_message_observation(
        role=role,
        text_chars=message_summary["text_chars"],
        attachment_count=message_summary["attachment_ref_count"],
    )
    log.info(
        "message_stored",
        thread_key=thread_key,
        message_id=message_id,
        assignment_generation=assignment_generation,
        role=role,
        source_platform=_metadata_platform(metadata),
        event_size_bytes=payload_size_bytes(normalized_event),
        **message_summary,
    )

    return {
        "ok": True,
        "message_id": message_id,
        "stored_event_id": f"{thread_key}:{message_id}",
        "attachment_ids": attachment_ids,
    }


def execution_terminal(status: str) -> bool:
    return status in {"completed", "failed_permanent", "cancelled"}


def build_execution_state_payload(
    *,
    execution_id: str,
    thread_key: str,
    status: str,
    terminal_reason: str | None = None,
    result_text: str | None = None,
    error_text: str | None = None,
    extra: dict[str, Any] | None = None,
) -> dict[str, Any]:
    payload = {
        "type": "execution.state",
        "execution_id": execution_id,
        "thread_key": thread_key,
        "status": status,
    }
    if terminal_reason is not None:
        payload["terminal_reason"] = terminal_reason
    if result_text is not None:
        payload["result_text"] = result_text
    if error_text:
        payload["error_text"] = error_text
    if extra:
        payload.update(extra)
    return payload


def _clip_slackbot(value: Any, max_chars: int = _MAX_SLACKBOT_STEP_CHARS) -> str:
    text = (
        value
        if isinstance(value, str)
        else json.dumps(value, ensure_ascii=False, default=str)
    )
    text = text.strip()
    return text if len(text) <= max_chars else f"{text[: max_chars - 1]}…"


def _has_slackbot_live_delivery(metadata: dict[str, Any]) -> bool:
    return (
        metadata.get(_SLACKBOT_LIVE_DELIVERY_METADATA_KEY) is True
        or metadata.get(_LEGACY_SLACKBOT_LIVE_DELIVERY_METADATA_KEY) is True
    )


async def _mark_slackbot_live_delivery_failed(
    pool,
    execution_id: str,
    reason: str,
    *,
    streamed_answer_chars: int | None = None,
) -> None:
    await pool.execute(
        "UPDATE agent_execution_requests "
        "SET metadata = jsonb_set("
        "  jsonb_set(metadata, "
        "  '{slackbot_live_delivery_failed}', "
        "  to_jsonb($2::text), "
        "  true), "
        "  '{slackbot_streamed_answer_chars}', "
        "  to_jsonb($3::int), "
        "  true"
        "), updated_at = NOW() "
        "WHERE execution_id = $1",
        execution_id,
        reason,
        max(streamed_answer_chars or 0, 0),
    )


def _canonical_text_blocks(event: dict[str, Any]) -> list[str]:
    if event.get("type") == "assistant":
        message = event.get("message") if isinstance(event.get("message"), dict) else {}
        content = (
            message.get("content") if isinstance(message.get("content"), list) else []
        )
        return [
            str(block.get("text") or "")
            for block in content
            if isinstance(block, dict)
            and block.get("type") == "text"
            and str(block.get("text") or "").strip()
        ]
    if event.get("type") == "result":
        text = str(event.get("result") or event.get("text") or "")
        return [text] if text.strip() else []
    return []


def _slackbot_streamed_answer_chars(value: Any) -> int:
    if isinstance(value, bool):
        return 0
    if isinstance(value, int):
        return max(value, 0)
    return 0


def _slackbot_live_delivery_covers_result(result_text: str, streamed_chars: int) -> bool:
    text = result_text.strip()
    if not text:
        return True
    return streamed_chars >= len(text)


async def _send_slackbot_canonical_event(
    session_id: str, event: dict[str, Any]
) -> bool:
    sent_text = False
    for text in _canonical_text_blocks(event):
        await slackbot_client.session_text(
            session_id,
            _clip_slackbot(text, _MAX_SLACKBOT_TEXT_CHARS),
        )
        sent_text = True

    event_type = str(event.get("type") or "")
    if event_type == "assistant":
        message = event.get("message") if isinstance(event.get("message"), dict) else {}
        content = (
            message.get("content") if isinstance(message.get("content"), list) else []
        )
        for block in content:
            if not isinstance(block, dict) or block.get("type") != "tool_use":
                continue
            tool_id = str(block.get("id") or uuid.uuid4())
            tool_name = str(block.get("name") or "Tool")
            tool_input = (
                block.get("input") if isinstance(block.get("input"), dict) else {}
            )
            await slackbot_client.session_step(
                session_id,
                step_id=tool_id,
                title=tool_name,
                status="in_progress",
                details=_clip_slackbot(tool_input),
            )
    elif event_type == "tool":
        content = event.get("content") if isinstance(event.get("content"), list) else []
        for block in content:
            if not isinstance(block, dict):
                continue
            tool_id = str(block.get("tool_use_id") or uuid.uuid4())
            is_error = bool(block.get("is_error"))
            await slackbot_client.session_step(
                session_id,
                step_id=tool_id,
                title="Tool result",
                status="error" if is_error else "complete",
                output=_clip_slackbot(block.get("content")),
            )
    elif event_type == "command_execution":
        command = str(event.get("command") or "Command")
        await slackbot_client.session_step(
            session_id,
            step_id=f"command-{hashlib.sha256(command.encode()).hexdigest()[:12]}",
            title=command[:256],
            status="complete" if event.get("status") == "completed" else "in_progress",
            output=_clip_slackbot(event.get("aggregated_output") or ""),
        )
    elif event_type == "error":
        await slackbot_client.session_step(
            session_id,
            step_id=f"error-{uuid.uuid4().hex[:12]}",
            title="Execution error",
            status="error",
            output=_clip_slackbot(event.get("error") or event),
        )
    return sent_text


async def append_execution_event(
    pool,
    *,
    thread_key: str,
    execution_id: str | None,
    event_kind: str,
    event_json: dict[str, Any],
) -> int:
    return int(
        await pool.fetchval(
            "INSERT INTO agent_execution_events (thread_key, execution_id, event_kind, event_json) "
            "VALUES ($1, $2, $3, $4::jsonb) RETURNING event_id",
            thread_key,
            execution_id,
            event_kind,
            canonical_json(event_json),
        )
    )


async def append_execution_state(
    pool,
    *,
    execution_id: str,
    thread_key: str,
    status: str,
    extra: dict[str, Any] | None = None,
) -> int:
    payload = build_execution_state_payload(
        execution_id=execution_id,
        thread_key=thread_key,
        status=status,
        extra=extra,
    )
    return await append_execution_event(
        pool,
        thread_key=thread_key,
        execution_id=execution_id,
        event_kind="execution_state",
        event_json=payload,
    )


async def get_execution_terminal_snapshot(
    pool, execution_id: str
) -> dict[str, Any] | None:
    row = await pool.fetchrow(
        "SELECT thread_key, status, terminal_reason, result_text, error_text "
        "FROM agent_execution_requests WHERE execution_id = $1",
        execution_id,
    )
    if not row or not execution_terminal(str(row["status"] or "")):
        return None

    latest_event_id = await pool.fetchval(
        "SELECT COALESCE(MAX(event_id), 0) FROM agent_execution_events WHERE execution_id = $1",
        execution_id,
    )
    return {
        "event_id": int(latest_event_id or 0),
        "event_kind": "execution_state",
        "event_json": build_execution_state_payload(
            execution_id=execution_id,
            thread_key=str(row["thread_key"]),
            status=str(row["status"]),
            terminal_reason=(
                str(row["terminal_reason"])
                if row["terminal_reason"] is not None
                else None
            ),
            result_text=(
                str(row["result_text"]) if row["result_text"] is not None else None
            ),
            error_text=(
                str(row["error_text"]) if row["error_text"] is not None else None
            ),
        ),
    }


async def _completed_session_result_for_reclaimed_turn(
    pool,
    *,
    thread_key: str,
    durable_turn_id: str,
) -> str | None:
    """Return the persisted result for a reclaimed turn that already completed."""
    if not durable_turn_id:
        return None
    row = await pool.fetchrow(
        "SELECT inflight_turn_id, last_result FROM sandbox_sessions "
        "WHERE thread_key = $1",
        thread_key,
    )
    if row is None:
        return None
    if row["inflight_turn_id"]:
        return None
    last_result = row["last_result"]
    if last_result is None:
        return None
    return str(last_result)


async def enqueue_execution(
    pool,
    *,
    thread_key: str,
    assignment_generation: int,
    execute_id: str,
    harness: str | None,
    delivery: dict[str, Any],
    metadata: dict[str, Any],
) -> dict[str, Any]:
    with start_span(
        "centaur.agent.enqueue",
        attributes={
            "centaur.thread_key": thread_key,
            "centaur.assignment_generation": assignment_generation,
            "centaur.execute_id": execute_id,
            "centaur.harness": harness,
            "centaur.delivery.platform": _delivery_platform(delivery),
        },
    ) as span:
        span_context = span_context_to_dict(span)
        if span_context:
            metadata = {
                **metadata,
                _OTEL_METADATA_KEY: {
                    **decode_jsonb(metadata.get(_OTEL_METADATA_KEY), {}),
                    _OTEL_EXECUTION_SPAN_CONTEXT_KEY: span_context,
                },
            }
        try:
            result = await _enqueue_execution_impl(
                pool,
                thread_key=thread_key,
                assignment_generation=assignment_generation,
                execute_id=execute_id,
                harness=harness,
                delivery=delivery,
                metadata=metadata,
            )
        except ControlPlaneError as exc:
            set_span_attributes(span, {"centaur.error.code": exc.code})
            mark_error(span, exc.message)
            raise
        except Exception as exc:
            record_exception(span, exc)
            raise
        set_span_attributes(
            span,
            {
                "centaur.execution_id": result.get("execution_id"),
                "centaur.idempotent": bool(result.get("idempotent")),
                "centaur.execution.status": result.get("status"),
            },
        )
        return result


async def _enqueue_execution_impl(
    pool,
    *,
    thread_key: str,
    assignment_generation: int,
    execute_id: str,
    harness: str | None,
    delivery: dict[str, Any],
    metadata: dict[str, Any],
) -> dict[str, Any]:
    payload = {
        "thread_key": thread_key,
        "assignment_generation": assignment_generation,
        "execute_id": execute_id,
        "harness": harness,
        "delivery": delivery,
        "metadata": metadata,
    }
    req_hash = request_hash(payload)

    existing = await pool.fetchrow(
        "SELECT execution_id, request_hash, status, assignment_generation "
        "FROM agent_execution_requests WHERE thread_key = $1 AND execute_id = $2",
        thread_key,
        execute_id,
    )
    if existing:
        if existing["request_hash"] != req_hash:
            raise ControlPlaneError(
                "IDEMPOTENCY_PAYLOAD_MISMATCH",
                "execute_id was already used with a different payload",
                409,
            )
        return {
            "ok": True,
            "execution_id": existing["execution_id"],
            "execute_id": execute_id,
            "assignment_generation": int(existing["assignment_generation"]),
            "status": existing["status"],
            "final_key": existing["execution_id"],
            "delivery_token": existing["execution_id"],
            "idempotent": True,
        }

    execution_id = f"exe_{uuid.uuid4().hex[:16]}"
    now = dt.datetime.now(dt.timezone.utc)
    silence_deadline = now + dt.timedelta(seconds=EXECUTION_SILENCE_TIMEOUT_S)
    hard_deadline = now + dt.timedelta(seconds=EXECUTION_HARD_TIMEOUT_S)

    async with pool.acquire() as conn:
        async with conn.transaction():
            await conn.execute("SELECT pg_advisory_xact_lock(hashtext($1))", thread_key)

            existing = await conn.fetchrow(
                "SELECT execution_id, request_hash, status, assignment_generation "
                "FROM agent_execution_requests WHERE thread_key = $1 AND execute_id = $2",
                thread_key,
                execute_id,
            )
            if existing:
                if existing["request_hash"] != req_hash:
                    raise ControlPlaneError(
                        "IDEMPOTENCY_PAYLOAD_MISMATCH",
                        "execute_id was already used with a different payload",
                        409,
                    )
                return {
                    "ok": True,
                    "execution_id": existing["execution_id"],
                    "execute_id": execute_id,
                    "assignment_generation": int(existing["assignment_generation"]),
                    "status": existing["status"],
                    "final_key": existing["execution_id"],
                    "delivery_token": existing["execution_id"],
                    "idempotent": True,
                }

            active = await conn.fetchrow(
                "SELECT assignment_generation, harness, engine, persona_id, "
                "prompt_ref, effective_agents_md_sha256 "
                "FROM agent_runtime_assignments "
                "WHERE thread_key = $1 AND state = 'active' "
                "ORDER BY assignment_generation DESC LIMIT 1",
                thread_key,
            )
            if not active:
                raise ControlPlaneError(
                    "NO_ACTIVE_ASSIGNMENT",
                    "No active runtime assignment for thread_key",
                    409,
                )
            active_generation = int(active["assignment_generation"])
            if active_generation != int(assignment_generation):
                raise ControlPlaneError(
                    "ASSIGNMENT_GENERATION_STALE",
                    "assignment_generation does not match the active assignment",
                    409,
                )

            recent_failures = await conn.fetch(
                "SELECT terminal_reason, error_text, completed_at "
                "FROM agent_execution_requests "
                "WHERE thread_key = $1 "
                "  AND status = 'failed_permanent' "
                "  AND completed_at > NOW() - make_interval(secs => $2::double precision) "
                "ORDER BY completed_at DESC "
                "LIMIT $3",
                thread_key,
                float(THREAD_FAILURE_LOOP_WINDOW_S),
                THREAD_FAILURE_LOOP_THRESHOLD,
            )
            if len(recent_failures) >= THREAD_FAILURE_LOOP_THRESHOLD:
                reasons = [
                    str(row["terminal_reason"] or "unknown") for row in recent_failures
                ]
                log.warning(
                    "thread_failure_loop_detected",
                    thread_key=thread_key,
                    assignment_generation=assignment_generation,
                    recent_failure_count=len(recent_failures),
                    terminal_reasons=reasons,
                )
                raise ControlPlaneError(
                    "THREAD_FAILURE_LOOP",
                    (
                        "This thread failed repeatedly in the last few minutes, "
                        "so Centaur is pausing instead of retrying the same failure loop. "
                        f"Recent reasons: {', '.join(reasons)}"
                    ),
                    409,
                )

            await conn.execute(
                "INSERT INTO agent_execution_requests ("
                "execution_id, thread_key, assignment_generation, execute_id, request_hash, status, "
                "delivery, metadata, last_progress_at, silence_deadline_at, hard_deadline_at"
                ") VALUES ($1, $2, $3, $4, $5, 'queued', $6::jsonb, $7::jsonb, NOW(), $8, $9)",
                execution_id,
                thread_key,
                assignment_generation,
                execute_id,
                req_hash,
                canonical_json(delivery),
                canonical_json(metadata),
                silence_deadline,
                hard_deadline,
            )
            if _delivery_platform(
                delivery
            ) != "dev" and not _has_slackbot_live_delivery(metadata):
                await conn.execute(
                    "INSERT INTO agent_final_delivery_outbox ("
                    "execution_id, thread_key, delivery, state"
                    ") VALUES ($1, $2, $3::jsonb, 'awaiting_terminal') "
                    "ON CONFLICT (execution_id) DO NOTHING",
                    execution_id,
                    thread_key,
                    canonical_json(delivery),
                )
            await append_execution_state(
                conn,
                execution_id=execution_id,
                thread_key=thread_key,
                status="queued",
            )

    _worker_wake.set()

    resolved_harness = str(active["harness"] or harness or default_harness())
    log.info(
        "execute_queued",
        thread_key=thread_key,
        execution_id=execution_id,
        assignment_generation=assignment_generation,
        harness=resolved_harness,
        engine=active["engine"],
        persona_id=active["persona_id"],
        prompt_ref=active["prompt_ref"],
        prompt_sha=active["effective_agents_md_sha256"],
        delivery_platform=_delivery_platform(delivery),
    )
    record_execution_enqueued(resolved_harness)

    return {
        "ok": True,
        "execution_id": execution_id,
        "execute_id": execute_id,
        "assignment_generation": assignment_generation,
        "status": "queued",
        "final_key": execution_id,
        "delivery_token": execution_id,
    }


async def get_execution(pool, execution_id: str) -> dict[str, Any] | None:
    row = await pool.fetchrow(
        "SELECT e.execution_id, e.thread_key, e.assignment_generation, e.execute_id, e.status, "
        "e.durable_turn_id, e.terminal_reason, e.result_text, e.error_text, e.metadata, "
        "e.created_at, e.started_at, e.completed_at, e.updated_at, s.agent_thread_id "
        "FROM agent_execution_requests e "
        "LEFT JOIN sandbox_sessions s ON s.thread_key = e.thread_key "
        "WHERE e.execution_id = $1",
        execution_id,
    )
    if not row:
        return None
    return {
        "execution_id": row["execution_id"],
        "thread_key": row["thread_key"],
        "assignment_generation": int(row["assignment_generation"]),
        "execute_id": row["execute_id"],
        "status": row["status"],
        "durable_turn_id": row["durable_turn_id"],
        "terminal_reason": row["terminal_reason"],
        "result_text": row["result_text"],
        "error_text": row["error_text"],
        "agent_thread_id": row["agent_thread_id"] or "",
        "metadata": decode_jsonb(row["metadata"], {}),
        "created_at": row["created_at"].isoformat() if row["created_at"] else None,
        "started_at": row["started_at"].isoformat() if row["started_at"] else None,
        "completed_at": row["completed_at"].isoformat()
        if row["completed_at"]
        else None,
        "updated_at": row["updated_at"].isoformat() if row["updated_at"] else None,
    }


async def list_thread_executions(
    pool, thread_key: str, limit: int = 20
) -> list[dict[str, Any]]:
    rows = await pool.fetch(
        "SELECT execution_id, execute_id, status, created_at, started_at, completed_at "
        "FROM agent_execution_requests WHERE thread_key = $1 "
        "ORDER BY created_at DESC LIMIT $2",
        thread_key,
        max(1, min(limit, 100)),
    )
    return [
        {
            "execution_id": r["execution_id"],
            "execute_id": r["execute_id"],
            "status": r["status"],
            "created_at": r["created_at"].isoformat() if r["created_at"] else None,
            "started_at": r["started_at"].isoformat() if r["started_at"] else None,
            "completed_at": r["completed_at"].isoformat()
            if r["completed_at"]
            else None,
        }
        for r in rows
    ]


async def cancel_execution(pool, execution_id: str) -> dict[str, Any] | None:
    row = await pool.fetchrow(
        "SELECT execution_id, thread_key, assignment_generation, status FROM agent_execution_requests "
        "WHERE execution_id = $1",
        execution_id,
    )
    if not row:
        return None

    status = row["status"]
    thread_key = row["thread_key"]
    if execution_terminal(status):
        return {
            "ok": True,
            "execution_id": execution_id,
            "thread_key": thread_key,
            "status": status,
            "idempotent": True,
        }

    if status == "cancel_requested":
        return {
            "ok": True,
            "execution_id": execution_id,
            "thread_key": thread_key,
            "status": status,
            "idempotent": True,
        }

    if status == "queued":
        await _mark_execution_terminal(
            pool,
            execution_id=execution_id,
            thread_key=thread_key,
            status="cancelled",
            terminal_reason="cancelled",
            result_text="",
            error_text="cancelled",
        )
    else:
        await pool.execute(
            "UPDATE agent_execution_requests SET status = 'cancel_requested', updated_at = NOW() "
            "WHERE execution_id = $1",
            execution_id,
        )
        await append_execution_state(
            pool,
            execution_id=execution_id,
            thread_key=thread_key,
            status="cancel_requested",
        )
        # A cancelled turn may still emit buffered stdout after SIGUSR1. If we
        # immediately mark the session idle, the next execution can attach to
        # the same process and attribute the previous turn's tail output to the
        # new turn. Stop the sandbox instead; the next execution will spawn or
        # claim a clean runtime while preserving the durable message cursor.
        await _stop_execution_session(thread_key, reason="cancel_requested")
        await pool.execute(
            "UPDATE sandbox_sessions SET state = 'stopped', "
            "inflight_turn_id = NULL, inflight_turn_input = NULL, "
            "inflight_started_at = NULL, inflight_attempts = 0, updated_at = NOW() "
            "WHERE thread_key = $1",
            thread_key,
        )

    _worker_wake.set()
    return {
        "ok": True,
        "execution_id": execution_id,
        "thread_key": thread_key,
        "status": "cancel_requested" if status != "queued" else "cancelled",
    }


async def steer_execution(
    pool,
    execution_id: str,
    *,
    content_blocks: list[dict] | None = None,
    message_id: str | None = None,
    metadata: dict[str, Any] | None = None,
) -> dict[str, Any] | None:
    """Steer a running execution by injecting a steer message.

    If content_blocks is provided, they are sent as the steer message.
    Otherwise, pending buffered messages for the thread are flushed and sent.

    Falls back to cancel_execution() if steering fails.
    """
    from api.agent import (
        _db_get_session,
        _flush_pending,
        _flushed_to_messages,
        _get_last_delivered_id,
    )
    from api.sandbox.harness_protocol import messages_to_content_blocks

    row = await pool.fetchrow(
        "SELECT execution_id, thread_key, assignment_generation, status, started_at "
        "FROM agent_execution_requests WHERE execution_id = $1",
        execution_id,
    )
    if not row:
        return None

    status = row["status"]
    thread_key = row["thread_key"]
    assignment_generation = int(row["assignment_generation"])

    if status != "running":
        log.info(
            "steer_skipped_not_running",
            execution_id=execution_id,
            thread_key=thread_key,
            status=status,
        )
        return await cancel_execution(pool, execution_id)

    if content_blocks is not None:
        content_blocks = [part for part in content_blocks if isinstance(part, dict)]
        if message_id and content_blocks:
            if metadata and metadata.get("steer_replacement") is True:
                await _merge_execution_metadata(
                    pool,
                    execution_id,
                    {
                        "steer_replacement": {
                            "message_id": message_id,
                            "suppress_cancellation_delivery": True,
                        },
                    },
                )

    # Build content blocks from pending messages if not provided
    if content_blocks is None:
        last_delivered_id = await _get_last_delivered_id(thread_key)
        flushed = await _flush_pending(thread_key, last_delivered_id)
        started_at = row["started_at"]
        if started_at is not None:
            flushed = [
                item
                for item in flushed
                if item.get("role") == "user"
                and item.get("created_at") is not None
                and item["created_at"] > started_at
            ]
        if flushed:
            msgs = _flushed_to_messages(flushed)
            content_blocks = messages_to_content_blocks(msgs)

    if not content_blocks:
        log.info(
            "steer_no_content",
            execution_id=execution_id,
            thread_key=thread_key,
        )
        return await cancel_execution(pool, execution_id)

    # Get the sandbox session
    session = await _db_get_session(thread_key)
    if not session or session.db_state not in ("running", "idle"):
        log.warning(
            "steer_no_session",
            execution_id=execution_id,
            thread_key=thread_key,
        )
        return await cancel_execution(pool, execution_id)

    # Attach to the sandbox and inject the steer message
    backend = get_backend()
    try:
        await backend.attach(session)
    except Exception:
        log.warning(
            "steer_attach_failed",
            execution_id=execution_id,
            thread_key=thread_key,
            exc_info=True,
        )
        return await cancel_execution(pool, execution_id)

    result = await steer_stdin(session, content_blocks)
    if not result.get("ok"):
        log.warning(
            "steer_fallback_to_cancel",
            execution_id=execution_id,
            thread_key=thread_key,
        )
        return await cancel_execution(pool, execution_id)

    await asyncio.sleep(0.05)
    current = await pool.fetchrow(
        "SELECT status, terminal_reason FROM agent_execution_requests WHERE execution_id = $1",
        execution_id,
    )
    current_status = current["status"] if current else None
    if current_status != "running":
        log.info(
            "steer_completed_during_inject",
            execution_id=execution_id,
            thread_key=thread_key,
            status=current_status,
            terminal_reason=current["terminal_reason"] if current else None,
        )
        return {
            "ok": True,
            "execution_id": execution_id,
            "thread_key": thread_key,
            "status": "cancel_requested"
            if current_status == "cancelled"
            else current_status,
        }

    if message_id and content_blocks:
        event = {
            "type": "user",
            "message": {"role": "user", "content": content_blocks},
        }
        await append_message(
            pool,
            thread_key=thread_key,
            assignment_generation=assignment_generation,
            message_id=message_id,
            event=event,
            metadata=metadata or {},
        )

    log.info(
        "execution_steered",
        execution_id=execution_id,
        thread_key=thread_key,
    )
    return {
        "ok": True,
        "execution_id": execution_id,
        "thread_key": thread_key,
        "status": "steered",
    }


async def release_assignment(
    pool,
    *,
    thread_key: str,
    release_id: str,
    cancel_inflight: bool,
    stop_runtime: bool = True,
    stop_runtime_background: bool = False,
) -> dict[str, Any]:
    payload = {
        "thread_key": thread_key,
        "release_id": release_id,
        "cancel_inflight": cancel_inflight,
    }
    req_hash = request_hash(payload)

    response: dict[str, Any]
    async with pool.acquire() as conn:
        async with conn.transaction():
            await conn.execute("SELECT pg_advisory_xact_lock(hashtext($1))", thread_key)

            existing = await conn.fetchrow(
                "SELECT request_hash, response_json FROM agent_release_requests "
                "WHERE thread_key = $1 AND release_id = $2",
                thread_key,
                release_id,
            )
            if existing:
                if existing["request_hash"] != req_hash:
                    raise ControlPlaneError(
                        "IDEMPOTENCY_PAYLOAD_MISMATCH",
                        "release_id was already used with a different payload",
                        409,
                    )
                return decode_jsonb(existing["response_json"], {})

            active = await conn.fetchrow(
                "SELECT assignment_generation, runtime_id FROM agent_runtime_assignments "
                "WHERE thread_key = $1 AND state = 'active' "
                "ORDER BY assignment_generation DESC LIMIT 1",
                thread_key,
            )
            if not active:
                response = {
                    "ok": True,
                    "thread_key": thread_key,
                    "released": False,
                    "reason": "no_active_assignment",
                }
            else:
                generation = int(active["assignment_generation"])
                await conn.execute(
                    "UPDATE agent_runtime_assignments SET state = 'released', released_at = NOW(), updated_at = NOW() "
                    "WHERE thread_key = $1 AND assignment_generation = $2",
                    thread_key,
                    generation,
                )
                if cancel_inflight:
                    await conn.execute(
                        "UPDATE agent_execution_requests SET status = 'cancelled', terminal_reason = 'released', "
                        "completed_at = NOW(), updated_at = NOW() "
                        "WHERE thread_key = $1 AND status IN ('queued', 'running', 'cancel_requested', 'retry_wait')",
                        thread_key,
                    )
                response = {
                    "ok": True,
                    "thread_key": thread_key,
                    "released": True,
                    "assignment_generation": generation,
                    "runtime_id": active["runtime_id"],
                }

            await conn.execute(
                "INSERT INTO agent_release_requests (thread_key, release_id, request_hash, response_json) "
                "VALUES ($1, $2, $3, $4::jsonb)",
                thread_key,
                release_id,
                req_hash,
                canonical_json(response),
            )
    if response.get("released") and stop_runtime:
        with contextlib.suppress(Exception):
            runtime_id = response.get("runtime_id")
            if isinstance(runtime_id, str) and runtime_id:
                if stop_runtime_background:
                    from api.agent import stop_session_by_id

                    async def _stop_released_runtime() -> None:
                        try:
                            await stop_session_by_id(runtime_id, thread_key=thread_key)
                        except Exception:
                            log.warning(
                                "released_runtime_stop_failed",
                                thread_key=thread_key,
                                runtime_id=runtime_id,
                                release_id=release_id,
                                exc_info=True,
                            )

                    asyncio.create_task(_stop_released_runtime())
                else:
                    from api.agent import stop_session_by_id

                    await stop_session_by_id(runtime_id, thread_key=thread_key)
            else:
                await stop_session(thread_key)
    log.info(
        "thread_released",
        thread_key=thread_key,
        release_id=release_id,
        released=response.get("released"),
        assignment_generation=response.get("assignment_generation"),
        runtime_id=response.get("runtime_id"),
        cancel_inflight=cancel_inflight,
    )
    return response


async def _mark_execution_terminal(
    pool,
    *,
    execution_id: str,
    thread_key: str,
    status: str,
    terminal_reason: str,
    result_text: str,
    error_text: str | None,
    slackbot_streamed_answer_chars_override: int | None = None,
) -> None:
    next_attempt_at = dt.datetime.now(dt.timezone.utc) + dt.timedelta(
        seconds=FINAL_DELIVERY_READY_GRACE_S,
    )
    completed_at = dt.datetime.now(dt.timezone.utc)
    row = await pool.fetchrow(
        "UPDATE agent_execution_requests SET status = $1, terminal_reason = $2, "
        "result_text = $3, error_text = $4, completed_at = $6, "
        "worker_id = NULL, worker_lease_expires_at = NULL, updated_at = NOW() "
        "WHERE execution_id = $5 AND status NOT IN ('completed', 'failed_permanent', 'cancelled') "
        "RETURNING started_at, assignment_generation, metadata, delivery",
        status,
        terminal_reason,
        result_text,
        error_text,
        execution_id,
        completed_at,
    )
    if row is None:
        current = await pool.fetchrow(
            "SELECT status, terminal_reason FROM agent_execution_requests WHERE execution_id = $1",
            execution_id,
        )
        log.info(
            "execution_terminal_update_skipped",
            execution_id=execution_id,
            thread_key=thread_key,
            attempted_status=status,
            attempted_terminal_reason=terminal_reason,
            current_status=current["status"] if current else None,
            current_terminal_reason=current["terminal_reason"] if current else None,
        )
        return
    harness = None
    engine = None
    persona_id = None
    prompt_ref = None
    prompt_sha = None
    repo_context: dict[str, str] = {}
    slackbot_streamed_answer_chars = 0
    suppress_final_delivery = False
    suppress_legacy_delivery = False
    metadata: dict[str, Any] = {}
    raw_agent_thread_id = await pool.fetchval(
        "SELECT agent_thread_id FROM sandbox_sessions WHERE thread_key = $1",
        thread_key,
    )
    agent_thread_id = (
        raw_agent_thread_id.strip() if isinstance(raw_agent_thread_id, str) else ""
    )
    if row:
        ag = row["assignment_generation"]
        metadata = decode_jsonb(row["metadata"], {})
        if isinstance(metadata, dict):
            repo_context = _extract_repo_context(
                decode_jsonb(metadata.get("repo_context"), {})
            )
            steer_replacement = decode_jsonb(metadata.get("steer_replacement"), {})
            suppress_final_delivery = bool(
                isinstance(steer_replacement, dict)
                and steer_replacement.get("suppress_cancellation_delivery") is True
            )
            slackbot_live_delivery_failed = bool(
                metadata.get("slackbot_live_delivery_failed")
            )
            raw_streamed_answer_chars = metadata.get("slackbot_streamed_answer_chars")
            if (
                isinstance(raw_streamed_answer_chars, int)
                and not slackbot_live_delivery_failed
            ):
                slackbot_streamed_answer_chars = max(raw_streamed_answer_chars, 0)
            if (
                slackbot_streamed_answer_chars_override is not None
                and not slackbot_live_delivery_failed
            ):
                slackbot_streamed_answer_chars = max(
                    slackbot_streamed_answer_chars,
                    slackbot_streamed_answer_chars_override,
                    0,
                )
            result_has_text = bool(result_text.strip())
            suppress_legacy_delivery = (
                _has_slackbot_live_delivery(metadata)
                and not slackbot_live_delivery_failed
                and (
                    not result_has_text
                    or _slackbot_live_delivery_covers_result(
                        result_text,
                        slackbot_streamed_answer_chars,
                    )
                )
            )
        assignment_row = await pool.fetchrow(
            "SELECT harness, engine, persona_id, prompt_ref, effective_agents_md_sha256 "
            "FROM agent_runtime_assignments WHERE thread_key = $1 AND assignment_generation = $2",
            thread_key,
            ag,
        )
        if assignment_row:
            harness = assignment_row["harness"]
            engine = assignment_row["engine"]
            persona_id = assignment_row["persona_id"]
            prompt_ref = assignment_row["prompt_ref"]
            prompt_sha = assignment_row["effective_agents_md_sha256"]
    duration_s = 0.0
    if row and row.get("started_at") and completed_at:
        duration_s = (completed_at - row["started_at"]).total_seconds()
    log.info(
        "execute_completed",
        execution_id=execution_id,
        thread_key=thread_key,
        status=status,
        terminal_reason=terminal_reason,
        duration_s=round(duration_s, 2),
        harness=harness,
        engine=engine,
        persona_id=persona_id,
        prompt_ref=prompt_ref,
        prompt_sha=prompt_sha,
        result_size_bytes=payload_size_bytes(result_text),
        error_size_bytes=payload_size_bytes(error_text) if error_text else 0,
    )
    add_span_event(
        "centaur.agent.execution_completed",
        {
            "centaur.execution_id": execution_id,
            "centaur.thread_key": thread_key,
            "centaur.execution.status": status,
            "centaur.terminal_reason": terminal_reason,
            "centaur.execution.duration_s": round(duration_s, 3),
            "centaur.result_size_bytes": payload_size_bytes(result_text),
            "centaur.error_size_bytes": payload_size_bytes(error_text)
            if error_text
            else 0,
        },
    )
    record_agent_execution(harness, status, duration_s)
    record_execution_terminal(harness or "unknown", status, terminal_reason)
    suppress_final_delivery_payload = (
        suppress_final_delivery
        and status == "cancelled"
        and terminal_reason == "cancel_requested"
    )
    await append_execution_state(
        pool,
        execution_id=execution_id,
        thread_key=thread_key,
        status=status,
        extra={
            "terminal_reason": terminal_reason,
            "result_text": result_text,
            **({"error_text": error_text} if error_text else {}),
            **({"agent_thread_id": agent_thread_id} if agent_thread_id else {}),
            **({"repo_context": repo_context} if repo_context else {}),
            **(
                {"suppress_final_delivery": True}
                if suppress_final_delivery_payload
                else {}
            ),
        },
    )
    delivery_platform = _delivery_platform(
        decode_jsonb(row["delivery"], {}) if row else {}
    )
    if delivery_platform == "dev" or suppress_legacy_delivery:
        slackbot_agent_session_id = str(metadata.get("slackbot_agent_session_id") or "")
        result_size = payload_size_bytes(result_text)
        if suppress_legacy_delivery and result_size > 0 and slackbot_streamed_answer_chars <= 0:
            log.warning(
                "final_delivery_skipped_without_live_answer",
                execution_id=execution_id,
                thread_key=thread_key,
                status=status,
                terminal_reason=terminal_reason,
                agent_thread_id=agent_thread_id or None,
                slackbot_agent_session_id=slackbot_agent_session_id or None,
                slackbot_streamed_answer_chars=slackbot_streamed_answer_chars,
                result_size_bytes=result_size,
                slackbot_live_delivery_failed=bool(
                    metadata.get("slackbot_live_delivery_failed")
                ),
            )
        log.info(
            "final_delivery_skipped"
            if suppress_legacy_delivery
            else "final_delivery_skipped_dev",
            execution_id=execution_id,
            thread_key=thread_key,
            status=status,
            terminal_reason=terminal_reason,
            reason="slackbot_live_delivery"
            if suppress_legacy_delivery
            else "dev_delivery",
            agent_thread_id=agent_thread_id or None,
            slackbot_agent_session_id=slackbot_agent_session_id or None,
            slackbot_streamed_answer_chars=slackbot_streamed_answer_chars,
            result_size_bytes=result_size,
        )
        try:
            from api.workflow_engine import notify_execution_terminal

            await notify_execution_terminal(pool, execution_id)
        except Exception:
            log.warning(
                "workflow_terminal_notify_failed",
                execution_id=execution_id,
                thread_key=thread_key,
                exc_info=True,
            )
        return

    with start_span(
        "centaur.final_delivery.ready",
        attributes={
            "centaur.execution_id": execution_id,
            "centaur.thread_key": thread_key,
            "centaur.delivery.platform": delivery_platform,
            "centaur.execution.status": status,
            "centaur.terminal_reason": terminal_reason,
        },
    ):
        await pool.execute(
            "INSERT INTO agent_final_delivery_outbox (execution_id, thread_key, delivery, state) "
            "VALUES ($1, $2, $3::jsonb, 'awaiting_terminal') "
            "ON CONFLICT (execution_id) DO NOTHING",
            execution_id,
            thread_key,
            canonical_json(decode_jsonb(row["delivery"], {}) if row else {}),
        )
        session_header = _agent_session_header(
            persona_id=persona_id,
            engine=engine,
            harness=harness,
        )
        await pool.execute(
            "UPDATE agent_final_delivery_outbox SET state = 'pending', final_payload = $1::jsonb, "
            "next_attempt_at = $2, lease_owner = NULL, lease_expires_at = NULL, updated_at = NOW() "
            "WHERE execution_id = $3",
            canonical_json(
                {
                    "execution_id": execution_id,
                    "thread_key": thread_key,
                    "status": status,
                    "terminal_reason": terminal_reason,
                    "session_title": _agent_session_title(
                        persona_id=persona_id,
                        engine=engine,
                        harness=harness,
                    ),
                    "session_header": session_header,
                    "result_text": result_text,
                    **({"error_text": error_text} if error_text else {}),
                    **(
                        {"slackbot_streamed_answer_chars": slackbot_streamed_answer_chars}
                        if slackbot_streamed_answer_chars
                        else {}
                    ),
                    **({"agent_thread_id": agent_thread_id} if agent_thread_id else {}),
                    **({"repo_context": repo_context} if repo_context else {}),
                    **(
                        {"suppress_final_delivery": True}
                        if suppress_final_delivery_payload
                        else {}
                    ),
                }
            ),
            next_attempt_at,
            execution_id,
        )
    await append_execution_event(
        pool,
        thread_key=thread_key,
        execution_id=execution_id,
        event_kind="final_delivery_ready",
        event_json={
            "type": "final_delivery.ready",
            "execution_id": execution_id,
            "thread_key": thread_key,
            "status": status,
            "terminal_reason": terminal_reason,
            "session_title": _agent_session_title(
                persona_id=persona_id,
                engine=engine,
                harness=harness,
            ),
            "session_header": session_header,
            "result_text": result_text,
            **({"error_text": error_text} if error_text else {}),
            **({"repo_context": repo_context} if repo_context else {}),
            **(
                {"suppress_final_delivery": True}
                if suppress_final_delivery_payload
                else {}
            ),
        },
    )
    log.info(
        "final_delivery_ready",
        execution_id=execution_id,
        thread_key=thread_key,
        status=status,
        terminal_reason=terminal_reason,
    )

    try:
        from api.workflow_engine import notify_execution_terminal

        await notify_execution_terminal(pool, execution_id)
    except Exception:
        log.warning(
            "workflow_terminal_notify_failed",
            execution_id=execution_id,
            thread_key=thread_key,
            exc_info=True,
        )


async def _touch_execution_progress(
    pool,
    execution_id: str,
    *,
    timeout_s: float | None = None,
) -> dt.datetime:
    silence_timeout_s = float(
        EXECUTION_SILENCE_TIMEOUT_S if timeout_s is None else timeout_s
    )
    row = await pool.fetchrow(
        "UPDATE agent_execution_requests SET last_progress_at = NOW(), "
        "silence_deadline_at = NOW() + make_interval(secs => $1::double precision), "
        "worker_lease_expires_at = NOW() + make_interval(secs => $2::double precision), "
        "updated_at = NOW() WHERE execution_id = $3 RETURNING silence_deadline_at",
        silence_timeout_s,
        float(EXECUTION_WORKER_LEASE_S),
        execution_id,
    )
    if row and row["silence_deadline_at"]:
        return row["silence_deadline_at"]
    return dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=silence_timeout_s)


async def _heartbeat_execution_lease(pool, execution_id: str) -> None:
    await pool.execute(
        "UPDATE agent_execution_requests SET "
        "worker_lease_expires_at = NOW() + make_interval(secs => $1::double precision), "
        "updated_at = NOW() "
        "WHERE execution_id = $2 AND status IN ('running', 'cancel_requested', 'retry_wait')",
        float(EXECUTION_WORKER_LEASE_S),
        execution_id,
    )


async def _stop_execution_session(thread_key: str, *, reason: str) -> None:
    try:
        await stop_session(thread_key)
    except Exception:
        log.warning(
            "execution_session_stop_failed",
            thread_key=thread_key,
            reason=reason,
            exc_info=True,
        )


async def _pause_execution_session(pool, thread_key: str, *, reason: str) -> bool:
    row = await pool.fetchrow(
        "SELECT sandbox_id FROM sandbox_sessions WHERE thread_key = $1",
        thread_key,
    )
    if row is None:
        log.info(
            "execution_session_pause_skipped_missing",
            thread_key=thread_key,
            reason=reason,
        )
        return False

    sandbox_id = str(row["sandbox_id"])
    backend = get_backend()
    if not getattr(backend, "supports_stateful_pause", False):
        log.warning(
            "execution_session_pause_skipped_unsupported_backend",
            thread_key=thread_key,
            sandbox=sandbox_id[:12],
            reason=reason,
            backend=getattr(backend, "name", ""),
        )
        return False

    try:
        await backend.pause_by_id(sandbox_id)
        _drop_runtime(sandbox_id)
        await pool.execute(
            "UPDATE sandbox_sessions SET state = 'suspended', updated_at = NOW() "
            "WHERE sandbox_id = $1",
            sandbox_id,
        )
        log.info(
            "execution_session_paused",
            thread_key=thread_key,
            sandbox=sandbox_id[:12],
            reason=reason,
        )
        return True
    except Exception:
        log.warning(
            "execution_session_pause_failed",
            thread_key=thread_key,
            sandbox=sandbox_id[:12],
            reason=reason,
            exc_info=True,
        )
        return False


async def _requeue_execution_after_raw_harness_auth_failure(
    pool,
    *,
    execution_id: str,
    thread_key: str,
    metadata: dict[str, Any],
    combined_error: str,
) -> bool:
    next_attempt = _raw_harness_auth_retry_attempt(metadata) + 1
    retry_metadata = {
        _RAW_HARNESS_AUTH_RETRY_METADATA_KEY: {
            "reason": _RAW_HARNESS_AUTH_RETRY_REASON,
            "attempt": next_attempt,
            "max_attempts": _RAW_HARNESS_AUTH_RETRY_LIMIT,
            "fresh_runtime": True,
            "last_error": combined_error,
        }
    }
    await _stop_execution_session(
        thread_key,
        reason="raw_harness_auth_retry",
    )
    update_result = await pool.execute(
        "UPDATE agent_execution_requests SET "
        "status = 'queued', "
        # Force the replacement runtime to receive the original user turn; the
        # previous runtime failed before it could complete the turn.
        "durable_turn_id = NULL, "
        "terminal_reason = NULL, "
        "result_text = NULL, "
        "error_text = NULL, "
        "completed_at = NULL, "
        "claimed_at = NULL, "
        "worker_id = NULL, "
        "worker_lease_expires_at = NULL, "
        "last_progress_at = NOW(), "
        "silence_deadline_at = NOW() + make_interval(secs => $1::double precision), "
        "metadata = COALESCE(metadata, '{}'::jsonb) || $2::jsonb, "
        "updated_at = NOW() "
        "WHERE execution_id = $3 AND status = 'running'",
        float(EXECUTION_SILENCE_TIMEOUT_S),
        canonical_json(retry_metadata),
        execution_id,
    )
    try:
        updated_rows = int(str(update_result).split()[-1])
    except (IndexError, ValueError):
        updated_rows = 0
    if updated_rows != 1:
        log.warning(
            "execution_raw_harness_auth_requeue_skipped",
            execution_id=execution_id,
            thread_key=thread_key,
            update_result=update_result,
        )
        return False
    await pool.execute(
        "UPDATE agent_message_requests SET delivered_execution_id = NULL "
        "WHERE thread_key = $1 AND delivered_execution_id = $2",
        thread_key,
        execution_id,
    )
    await pool.execute(
        "UPDATE sandbox_sessions SET last_delivered_id = NULL, updated_at = NOW() "
        "WHERE thread_key = $1",
        thread_key,
    )
    await append_execution_state(
        pool,
        execution_id=execution_id,
        thread_key=thread_key,
        status="queued",
        extra={"retry": retry_metadata[_RAW_HARNESS_AUTH_RETRY_METADATA_KEY]},
    )
    log.warning(
        "execution_requeued_after_raw_harness_auth_failure",
        execution_id=execution_id,
        thread_key=thread_key,
        retry_attempt=next_attempt,
    )
    _worker_wake.set()
    return True


async def _claim_next_execution(pool) -> dict[str, Any] | None:
    async with pool.acquire() as conn:
        async with conn.transaction():
            candidates = await conn.fetch(
                "SELECT er.execution_id, er.thread_key "
                "FROM agent_execution_requests er "
                "WHERE ("
                "  er.status = 'queued' "
                "  OR ("
                "    er.status IN ('cancel_requested', 'retry_wait') "
                "    AND (er.worker_lease_expires_at IS NULL OR er.worker_lease_expires_at <= NOW())"
                "  )"
                ") "
                "AND NOT EXISTS ("
                "  SELECT 1 FROM agent_execution_requests active "
                "  WHERE active.thread_key = er.thread_key "
                "  AND active.execution_id <> er.execution_id "
                "  AND ("
                "    active.status IN ('running', 'retry_wait') "
                "    OR ("
                "      active.status = 'cancel_requested' "
                "      AND active.worker_lease_expires_at > NOW()"
                "    )"
                "  )"
                ") "
                "ORDER BY er.created_at ASC "
                "LIMIT 32 "
                "FOR UPDATE SKIP LOCKED"
            )
            # Count how many workflow-linked executions are currently running
            # to enforce the slot reservation for user-facing requests.
            workflow_running_count = await conn.fetchval(
                "SELECT COUNT(*) FROM agent_execution_requests er "
                "WHERE er.status IN ('running', 'retry_wait') "
                "AND EXISTS ("
                "  SELECT 1 FROM workflow_checkpoints wc "
                "  WHERE wc.execution_id = er.execution_id"
                ")"
            )
            for candidate in candidates:
                thread_key = str(candidate["thread_key"])
                lock_acquired = await conn.fetchval(
                    "SELECT pg_try_advisory_xact_lock("
                    "hashtext('agent_execution_thread'), hashtext($1)"
                    ")",
                    thread_key,
                )
                if not lock_acquired:
                    continue
                has_active = await conn.fetchval(
                    "SELECT 1 FROM agent_execution_requests "
                    "WHERE thread_key = $1 "
                    "AND execution_id <> $2 "
                    "AND ("
                    "  status IN ('running', 'retry_wait') "
                    "  OR (status = 'cancel_requested' AND worker_lease_expires_at > NOW())"
                    ") "
                    "LIMIT 1",
                    thread_key,
                    candidate["execution_id"],
                )
                if has_active:
                    continue
                # If this execution is workflow-linked, check the slot cap.
                is_workflow_linked = await conn.fetchval(
                    "SELECT 1 FROM workflow_checkpoints "
                    "WHERE execution_id = $1 LIMIT 1",
                    candidate["execution_id"],
                )
                if (
                    is_workflow_linked
                    and workflow_running_count >= _MAX_WORKFLOW_EXECUTION_SLOTS
                ):
                    continue
                row = await conn.fetchrow(
                    "UPDATE agent_execution_requests er "
                    "SET status = CASE WHEN er.status IN ('queued', 'retry_wait') THEN 'running' ELSE er.status END, "
                    "claimed_at = NOW(), "
                    "started_at = COALESCE(er.started_at, NOW()), "
                    "last_progress_at = NOW(), "
                    "silence_deadline_at = NOW() + make_interval(secs => $1::double precision), "
                    "hard_deadline_at = CASE "
                    "  WHEN er.claimed_at IS NULL THEN NOW() + make_interval(secs => $5::double precision) "
                    "  ELSE er.hard_deadline_at "
                    "END, "
                    "worker_id = $2, "
                    "worker_lease_expires_at = NOW() + make_interval(secs => $3::double precision), "
                    "updated_at = NOW() "
                    "WHERE er.execution_id = $4 AND er.status IN ('queued', 'cancel_requested', 'retry_wait') "
                    "RETURNING er.execution_id, er.thread_key, er.assignment_generation, "
                    "er.execute_id, er.durable_turn_id, er.status, er.delivery, er.metadata, er.silence_deadline_at, "
                    "er.hard_deadline_at, er.created_at, er.claimed_at",
                    float(EXECUTION_SILENCE_TIMEOUT_S),
                    WORKER_INSTANCE_ID,
                    float(EXECUTION_WORKER_LEASE_S),
                    candidate["execution_id"],
                    float(EXECUTION_HARD_TIMEOUT_S),
                )
                if row:
                    return dict(row)
    return None


async def _process_execution(pool, row: dict[str, Any]) -> None:
    execution_id = str(row["execution_id"])
    thread_key = str(row["thread_key"])
    assignment_generation = int(row["assignment_generation"])
    delivery = decode_jsonb(row.get("delivery"), {})
    metadata = decode_jsonb(row.get("metadata"), {})
    with start_span(
        "centaur.agent.execution",
        parent_context=execution_span_context_from_metadata(metadata),
        attributes={
            "centaur.thread_key": thread_key,
            "centaur.execution_id": execution_id,
            "centaur.assignment_generation": assignment_generation,
            "centaur.delivery.platform": _delivery_platform(delivery),
            "centaur.worker_id": WORKER_INSTANCE_ID,
            "centaur.execute_id": row.get("execute_id"),
        },
    ) as span:
        span_context = span_context_to_dict(span)
        if span_context:
            try:
                await _store_execution_span_context(pool, execution_id, span_context)
            except Exception:
                log.warning(
                    "execution_span_context_store_failed",
                    execution_id=execution_id,
                    thread_key=thread_key,
                    exc_info=True,
                )
        if isinstance(metadata, dict):
            set_laminar_trace_metadata(
                span,
                _execution_laminar_metadata(
                    thread_key=thread_key,
                    execution_id=execution_id,
                    execute_id=row.get("execute_id"),
                    assignment_generation=assignment_generation,
                    delivery=delivery,
                    execution_metadata=metadata,
                ),
            )
            set_span_attributes(
                span,
                {
                    "centaur.user_id": metadata.get("user_id"),
                    "centaur.slackbot.live_delivery": bool(
                        _has_slackbot_live_delivery(metadata)
                    ),
                },
            )
        input_text = await _execution_input_text(
            pool, execution_id, thread_key, assignment_generation
        )
        if input_text:
            clipped_input = _clip_slackbot(input_text)
            set_span_attributes(
                span,
                {
                    "centaur.llm.input": clipped_input,
                },
            )
        try:
            await _process_execution_impl(pool, row)
        except Exception as exc:
            record_exception(span, exc)
            raise
        finally:
            terminal = await pool.fetchrow(
                "SELECT status, terminal_reason, result_text, error_text FROM agent_execution_requests "
                "WHERE execution_id = $1",
                execution_id,
            )
            if terminal:
                status = str(terminal["status"] or "")
                terminal_reason = terminal["terminal_reason"]
                set_span_attributes(
                    span,
                    {
                        "centaur.execution.status": status,
                        "centaur.terminal_reason": terminal_reason,
                    },
                )
                result_text = terminal["result_text"]
                if result_text:
                    clipped_output = _clip_slackbot(result_text)
                    set_span_attributes(
                        span,
                        {
                            "centaur.llm.output": clipped_output,
                        },
                    )
                if status in {"failed_permanent", "cancelled"}:
                    mark_error(span, str(terminal_reason or terminal["error_text"] or status))


async def _execution_input_text(
    pool,
    execution_id: str,
    thread_key: str,
    assignment_generation: int,
) -> str:
    rows = await pool.fetch(
        "SELECT event_json FROM agent_message_requests "
        "WHERE delivered_execution_id = $1 "
        "OR (thread_key = $2 AND assignment_generation = $3 AND delivered_execution_id IS NULL) "
        "ORDER BY created_at, message_id",
        execution_id,
        thread_key,
        assignment_generation,
    )
    texts: list[str] = []
    for row in rows:
        event = decode_jsonb(row["event_json"], {})
        message = event.get("message") if isinstance(event, dict) else None
        content = message.get("content") if isinstance(message, dict) else None
        if not isinstance(content, list):
            continue
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                text = str(block.get("text") or "").strip()
                if text:
                    texts.append(text)
    return "\n\n".join(texts)


async def _store_execution_span_context(
    pool,
    execution_id: str,
    span_context: dict[str, Any],
) -> None:
    await pool.execute(
        "UPDATE agent_execution_requests "
        "SET metadata = metadata || $1::jsonb, updated_at = NOW() "
        "WHERE execution_id = $2",
        canonical_json({_OTEL_METADATA_KEY: {_OTEL_EXECUTION_SPAN_CONTEXT_KEY: span_context}}),
        execution_id,
    )


def execution_span_context_from_metadata(metadata: dict[str, Any] | None):
    if not isinstance(metadata, dict):
        return None
    otel = decode_jsonb(metadata.get(_OTEL_METADATA_KEY), {})
    if not isinstance(otel, dict):
        return None
    return context_from_serialized(otel.get(_OTEL_EXECUTION_SPAN_CONTEXT_KEY))


async def _process_execution_impl(pool, row: dict[str, Any]) -> None:
    execution_id = row["execution_id"]
    thread_key = row["thread_key"]
    assignment_generation = int(row["assignment_generation"])
    execution_status = str(row.get("status") or "running")
    delivery = decode_jsonb(row.get("delivery"), {})
    execution_metadata = decode_jsonb(row.get("metadata"), {})
    if not isinstance(execution_metadata, dict):
        execution_metadata = {}

    if execution_status == "cancel_requested":
        await _stop_execution_session(
            thread_key,
            reason="cancel_requested",
        )
        await _mark_execution_terminal(
            pool,
            execution_id=execution_id,
            thread_key=thread_key,
            status="cancelled",
            terminal_reason="cancel_requested",
            result_text="",
            error_text="cancel_requested",
        )
        return

    await append_execution_state(
        pool,
        execution_id=execution_id,
        thread_key=thread_key,
        status="running",
    )

    assignment = await pool.fetchrow(
        "SELECT harness, engine, runtime_id, agents_md_override, persona_id, prompt_ref, effective_agents_md_sha256 "
        "FROM agent_runtime_assignments "
        "WHERE thread_key = $1 AND assignment_generation = $2",
        thread_key,
        assignment_generation,
    )
    if not assignment:
        await _mark_execution_terminal(
            pool,
            execution_id=execution_id,
            thread_key=thread_key,
            status="failed_permanent",
            terminal_reason="assignment_missing",
            result_text="",
            error_text="assignment not found",
        )
        return

    harness = str(assignment["harness"])
    engine = str(assignment["engine"] or harness)
    persona_id = assignment["persona_id"]
    prompt_ref = assignment["prompt_ref"]
    prompt_sha = assignment["effective_agents_md_sha256"]
    claimed_at = row.get("claimed_at")
    queue_delay_s = (
        (claimed_at - row["created_at"]).total_seconds()
        if claimed_at and row.get("created_at")
        else 0
    )
    log.info(
        "execute_claimed",
        execution_id=execution_id,
        thread_key=thread_key,
        harness=harness,
        engine=engine,
        persona_id=persona_id,
        prompt_ref=prompt_ref,
        prompt_sha=prompt_sha,
        queue_delay_s=round(queue_delay_s, 2),
    )
    record_execution_claimed(harness, queue_delay_s)

    if row.get("hard_deadline_at"):
        hard_deadline = row["hard_deadline_at"]
    else:
        hard_deadline = dt.datetime.now(dt.timezone.utc) + dt.timedelta(
            seconds=EXECUTION_HARD_TIMEOUT_S
        )
    durable_turn_id = str(row.get("durable_turn_id") or "")
    silence_deadline = row.get("silence_deadline_at")
    if not isinstance(silence_deadline, dt.datetime):
        silence_deadline = dt.datetime.now(dt.timezone.utc) + dt.timedelta(
            seconds=EXECUTION_SILENCE_TIMEOUT_S
        )
    reclaiming_existing_turn = bool(durable_turn_id)
    now = dt.datetime.now(dt.timezone.utc)
    if now >= hard_deadline:
        await _mark_execution_terminal(
            pool,
            execution_id=execution_id,
            thread_key=thread_key,
            status="failed_permanent",
            terminal_reason="hard_deadline_exceeded",
            result_text="",
            error_text="execution exceeded hard deadline",
        )
        await _stop_execution_session(
            thread_key,
            reason="hard_deadline_exceeded",
        )
        record_execution_watchdog_timeout(harness, "hard_deadline_exceeded")
        return
    if not durable_turn_id and now >= silence_deadline:
        await _mark_execution_terminal(
            pool,
            execution_id=execution_id,
            thread_key=thread_key,
            status="failed_permanent",
            terminal_reason="silence_deadline_exceeded",
            result_text="",
            error_text="execution made no progress before silence deadline",
        )
        await _pause_execution_session(
            pool,
            thread_key,
            reason="silence_deadline_exceeded",
        )
        record_execution_watchdog_timeout(harness, "silence_deadline_exceeded")
        return

    session = await get_or_spawn(
        thread_key,
        assignment["harness"],
        engine=assignment["engine"],
        persona=assignment["persona_id"],
    )
    if session.sandbox_id != assignment["runtime_id"]:
        await pool.execute(
            "UPDATE agent_runtime_assignments SET runtime_id = $1, updated_at = NOW() "
            "WHERE thread_key = $2 AND assignment_generation = $3",
            session.sandbox_id,
            thread_key,
            assignment_generation,
        )

    assignment_override = assignment["agents_md_override"]
    if assignment_override:
        await _write_agents_override(session.sandbox_id, str(assignment_override))

    if durable_turn_id:
        await _heartbeat_execution_lease(pool, execution_id)
        completed_result = await _completed_session_result_for_reclaimed_turn(
            pool,
            thread_key=thread_key,
            durable_turn_id=durable_turn_id,
        )
        if completed_result is not None:
            log.info(
                "execution_reclaimed_completed_session_result",
                execution_id=execution_id,
                thread_key=thread_key,
                durable_turn_id=durable_turn_id,
                result_size_bytes=payload_size_bytes(completed_result),
            )
            await _mark_execution_terminal(
                pool,
                execution_id=execution_id,
                thread_key=thread_key,
                status="completed",
                terminal_reason="completed_reclaimed_session_result",
                result_text=completed_result,
                error_text=None,
            )
            return
        if silence_deadline <= dt.datetime.now(dt.timezone.utc):
            silence_deadline = await _touch_execution_progress(pool, execution_id)
    else:
        requester_user_id = None
        if isinstance(delivery, dict):
            requester_user_id = delivery.get("recipient_user_id") or delivery.get("user_id")
        requester_user_id = requester_user_id or execution_metadata.get("user_id")
        trace_metadata = _execution_laminar_metadata(
            thread_key=thread_key,
            execution_id=execution_id,
            execute_id=row.get("execute_id"),
            assignment_generation=assignment_generation,
            delivery=delivery,
            execution_metadata=execution_metadata,
            harness=harness,
            engine=engine,
            persona_id=persona_id,
            prompt_ref=prompt_ref,
            runtime_id=session.sandbox_id,
            user_id=requester_user_id,
        )
        with start_span(
            "centaur.sandbox.inject",
            attributes={
                "centaur.execution_id": execution_id,
                "centaur.thread_key": thread_key,
                "centaur.runtime_id": session.sandbox_id,
                "centaur.delivery.platform": delivery.get("platform")
                if isinstance(delivery, dict)
                else None,
                "centaur.user_id": requester_user_id,
                **laminar_trace_metadata_attributes(trace_metadata),
            },
        ) as span:
            inject_span_context = span_context_to_dict(span) or {}
            inject_result = await inject_stdin(
                session,
                "",
                platform=delivery.get("platform") if isinstance(delivery, dict) else None,
                user_id=requester_user_id,
                trace_id=inject_span_context.get("trace_id"),
                traceparent=current_traceparent(span),
                trace_metadata=trace_metadata,
            )
            set_span_attributes(
                span,
                {
                    "centaur.durable_turn_id": inject_result.get("durable_turn_id"),
                    "centaur.sandbox.injected": bool(inject_result.get("injected")),
                },
            )
        durable_turn_id = str(inject_result.get("durable_turn_id") or "")
        await pool.execute(
            "UPDATE agent_execution_requests SET durable_turn_id = $1, updated_at = NOW() "
            "WHERE execution_id = $2",
            durable_turn_id or None,
            execution_id,
        )
        if inject_result.get("injected"):
            await pool.execute(
                "UPDATE agent_message_requests SET delivered_execution_id = $1 "
                "WHERE thread_key = $2 AND assignment_generation = $3 AND delivered_execution_id IS NULL",
                execution_id,
                thread_key,
                assignment_generation,
            )
            silence_deadline = await _touch_execution_progress(pool, execution_id)

    execution_sequence = await pool.fetchval(
        "SELECT COUNT(*) FROM agent_execution_requests "
        "WHERE thread_key = $1 AND execution_id <> $2",
        thread_key,
        execution_id,
    )
    execution_sequence = int(execution_sequence or 0)

    user_id_row = await pool.fetchval(
        "SELECT metadata->>'user_id' FROM agent_message_requests "
        "WHERE thread_key = $1 AND assignment_generation = $2 "
        "ORDER BY created_at DESC LIMIT 1",
        thread_key,
        assignment_generation,
    )
    user_id: str | None = str(user_id_row) if user_id_row else None

    backend = get_backend()
    await backend.attach(session, logs=reclaiming_existing_turn)
    rt = _get_runtime(session.sandbox_id)
    observations = ExecutionObservationAccumulator()
    started_at = claimed_at or row.get("created_at")
    first_token_at: dt.datetime | None = None
    slackbot_session_id = str(execution_metadata.get("slackbot_agent_session_id") or "")
    slackbot_forward_live = True
    slackbot_text_sent = False
    slackbot_done = False
    harness_thread_id = ""
    # Tolerate up to 5 consecutive harness_event failures before falling back
    # to the outbox path; the streak resets on the next success.
    slackbot_live_failure_streak = 0
    slackbot_live_failure_limit = 5

    async def _finalize_execution(
        *,
        status: str,
        terminal_reason: str,
        result_text: str,
        error_text: str | None,
    ) -> None:
        current_status = await pool.fetchval(
            "SELECT status FROM agent_execution_requests WHERE execution_id = $1",
            execution_id,
        )
        if execution_terminal(str(current_status or "")):
            log.info(
                "execution_finalize_skipped_terminal",
                execution_id=execution_id,
                thread_key=thread_key,
                attempted_status=status,
                attempted_terminal_reason=terminal_reason,
                current_status=current_status,
            )
            return

        duration_s = 0.0
        completed_at = dt.datetime.now(dt.timezone.utc)
        if isinstance(started_at, dt.datetime):
            duration_s = max((completed_at - started_at).total_seconds(), 0.0)
        ttft_ms: float | None = None
        if first_token_at is not None and isinstance(started_at, dt.datetime):
            ttft_ms = max((first_token_at - started_at).total_seconds() * 1000, 0.0)
            record_ttft(harness, ttft_ms / 1000)
        summary_payload = observations.build_summary(
            execution_id=execution_id,
            thread_key=thread_key,
            assignment_generation=assignment_generation,
            harness=harness,
            engine=engine,
            persona_id=persona_id,
            prompt_ref=prompt_ref,
            prompt_sha=prompt_sha,
            status=status,
            terminal_reason=terminal_reason,
            duration_s=duration_s,
            ttft_ms=ttft_ms,
            execution_sequence=execution_sequence,
            user_id=user_id,
        )
        await append_execution_event(
            pool,
            thread_key=thread_key,
            execution_id=execution_id,
            event_kind="execution_summary",
            event_json=summary_payload,
        )
        log.info("execution_summary", **summary_payload)
        if execution_sequence == 1:
            record_oneshot(harness, status == "completed")
        if user_id:
            record_execution_by_user(user_id, harness, status)
        nonlocal slackbot_session_id, slackbot_text_sent, slackbot_done
        finalize_session_id = slackbot_session_id or str(
            execution_metadata.get("slackbot_agent_session_id") or ""
        )
        if finalize_session_id and not slackbot_done and slackbot_forward_live:
            try:
                terminal_result_sent_to_slackbot = False
                await slackbot_client.session_done(
                    finalize_session_id, harness_thread_id or None
                )
                slackbot_done = True
                log.info(
                    "slackbot_live_delivery_finalized",
                    execution_id=execution_id,
                    thread_key=thread_key,
                    slackbot_agent_session_id=finalize_session_id,
                    harness_thread_id=harness_thread_id or None,
                    streamed_answer_chars=slackbot_streamed_answer_chars,
                    terminal_result_sent_to_slackbot=terminal_result_sent_to_slackbot,
                    result_size_bytes=payload_size_bytes(result_text),
                    slackbot_text_sent=slackbot_text_sent,
                )
            except Exception:
                log.warning(
                    "slackbot_live_delivery_finalize_failed",
                    execution_id=execution_id,
                    thread_key=thread_key,
                    exc_info=True,
                )
                await _mark_slackbot_live_delivery_failed(
                    pool,
                    execution_id,
                    "finalize_failed",
                )
                slackbot_session_id = ""
        await _mark_execution_terminal(
            pool,
            execution_id=execution_id,
            thread_key=thread_key,
            status=status,
            terminal_reason=terminal_reason,
            result_text=result_text,
            error_text=error_text,
            slackbot_streamed_answer_chars_override=slackbot_streamed_answer_chars,
        )

    execution_started_payload = {
        "type": "obs.execution_started",
        "execution_id": execution_id,
        "thread_key": thread_key,
        "assignment_generation": assignment_generation,
        "harness": harness,
        "engine": engine,
        "persona_id": persona_id,
        "prompt_ref": prompt_ref,
        "prompt_sha": prompt_sha,
        "runtime_id": session.sandbox_id,
        "queue_delay_s": round(queue_delay_s, 3),
        "delivery_platform": _delivery_platform(delivery),
        "execution_sequence": execution_sequence,
        "user_id": user_id,
    }
    await append_execution_event(
        pool,
        thread_key=thread_key,
        execution_id=execution_id,
        event_kind="execution_started",
        event_json=execution_started_payload,
    )
    execution_trace_metadata = _execution_laminar_metadata(
        thread_key=thread_key,
        execution_id=execution_id,
        execute_id=row.get("execute_id"),
        assignment_generation=assignment_generation,
        delivery=delivery,
        execution_metadata=execution_metadata,
        harness=harness,
        engine=engine,
        persona_id=persona_id,
        prompt_ref=prompt_ref,
        runtime_id=session.sandbox_id,
        user_id=user_id,
    )
    set_span_attributes(
        trace.get_current_span(),
        {
            "centaur.harness": harness,
            "centaur.engine": engine,
            "centaur.persona_id": persona_id,
            "centaur.runtime_id": session.sandbox_id,
            "centaur.prompt_ref": prompt_ref,
            "centaur.prompt_sha": prompt_sha,
            "centaur.execution_sequence": execution_sequence,
            "centaur.user_id": user_id,
            **laminar_trace_metadata_attributes(execution_trace_metadata),
        },
    )
    add_span_event("centaur.agent.execution_started", execution_started_payload)
    log.info("execute_started", **execution_started_payload)
    await _touch_execution_progress(pool, execution_id)

    turn_done_event: dict[str, Any] | None = None
    latest_terminal_result_text = ""
    slackbot_streamed_answer_chars = 0
    pending_event: asyncio.Task | None = None
    stream = _stream_stdout(
        session,
        backend,
        rt,
        rt.turn_counter,
        dt.datetime.now(dt.timezone.utc).timestamp(),
    )
    try:
        stream_iter = stream.__aiter__()
        while True:
            now = dt.datetime.now(dt.timezone.utc)
            if now >= hard_deadline:
                await _finalize_execution(
                    status="failed_permanent",
                    terminal_reason="hard_deadline_exceeded",
                    result_text="",
                    error_text="execution exceeded hard deadline",
                )
                await _stop_execution_session(
                    thread_key,
                    reason="hard_deadline_exceeded",
                )
                record_execution_watchdog_timeout(harness, "hard_deadline_exceeded")
                return
            if now >= silence_deadline:
                await _finalize_execution(
                    status="failed_permanent",
                    terminal_reason="silence_deadline_exceeded",
                    result_text="",
                    error_text="execution made no progress before silence deadline",
                )
                await _pause_execution_session(
                    pool,
                    thread_key,
                    reason="silence_deadline_exceeded",
                )
                record_execution_watchdog_timeout(harness, "silence_deadline_exceeded")
                return

            if pending_event is None:
                pending_event = asyncio.create_task(stream_iter.__anext__())

            wait_s = min(
                max((hard_deadline - now).total_seconds(), 0.0),
                max((silence_deadline - now).total_seconds(), 0.0),
                EXECUTION_WATCHDOG_POLL_S,
            )
            done, _ = await asyncio.wait({pending_event}, timeout=wait_s)
            if not done:
                await _heartbeat_execution_lease(pool, execution_id)
                status_row = await pool.fetchrow(
                    "SELECT status FROM agent_execution_requests WHERE execution_id = $1",
                    execution_id,
                )
                if status_row and status_row["status"] == "cancel_requested":
                    await _stop_execution_session(
                        thread_key,
                        reason="cancel_requested",
                    )
                    await _finalize_execution(
                        status="cancelled",
                        terminal_reason="cancel_requested",
                        result_text="",
                        error_text="cancel_requested",
                    )
                    return
                continue

            try:
                evt = await pending_event
            except StopAsyncIteration:
                break
            finally:
                pending_event = None

            payload = decode_jsonb(evt.get("data"), {})
            if not isinstance(payload, dict):
                continue
            if payload.get("session_id"):
                harness_thread_id = str(payload.get("session_id") or "")
                add_span_event(
                    "centaur.harness.session",
                    {
                        "centaur.execution_id": execution_id,
                        "centaur.thread_key": thread_key,
                        "centaur.harness_thread_id": harness_thread_id,
                    },
                )
            canonical_events = normalize_harness_event(engine, payload)
            for canonical_event in canonical_events:
                if canonical_event.get("type") != "result":
                    continue
                extracted = extract_result(engine, canonical_event)
                if extracted:
                    latest_terminal_result_text = extracted
            if slackbot_session_id and slackbot_forward_live:
                slack_events = canonical_events or [payload]
                for slack_event in slack_events:
                    if harness_thread_id and isinstance(slack_event, dict):
                        slack_event.setdefault("session_id", harness_thread_id)
                    if isinstance(slack_event, dict):
                        slack_event.setdefault("centaur_thread_key", thread_key)
                        slack_event.setdefault("centaur_execution_id", execution_id)
                        slack_event.setdefault(
                            "centaur_assignment_generation", assignment_generation
                        )
                    harness_result = await slackbot_client.harness_event(
                        slackbot_session_id, slack_event
                    )
                    if harness_result is None:
                        slackbot_live_failure_streak += 1
                        log.warning(
                            "slackbot_live_delivery_failed",
                            execution_id=execution_id,
                            thread_key=thread_key,
                            event_type=slack_event.get("type")
                            if isinstance(slack_event, dict)
                            else None,
                            consecutive_failures=slackbot_live_failure_streak,
                            failure_limit=slackbot_live_failure_limit,
                        )
                        if slackbot_live_failure_streak >= slackbot_live_failure_limit:
                            log.error(
                                "slackbot_live_delivery_disabled",
                                execution_id=execution_id,
                                thread_key=thread_key,
                                consecutive_failures=slackbot_live_failure_streak,
                            )
                            await _mark_slackbot_live_delivery_failed(
                                pool,
                                execution_id,
                                "harness_event_failed",
                                streamed_answer_chars=slackbot_streamed_answer_chars,
                            )
                            with contextlib.suppress(Exception):
                                await slackbot_client.set_status(delivery, "")
                            slackbot_forward_live = False
                        break
                    if isinstance(harness_result, dict):
                        slackbot_live_failure_streak = 0
                        harness_thread_id = str(
                            harness_result.get("threadId") or harness_thread_id
                        )
                        slackbot_done = bool(harness_result.get("done"))
                        streamed_chars = harness_result.get("streamedAnswerChars")
                        previous_streamed_answer_chars = slackbot_streamed_answer_chars
                        streamed_chars = _slackbot_streamed_answer_chars(streamed_chars)
                        if streamed_chars:
                            slackbot_streamed_answer_chars = max(
                                slackbot_streamed_answer_chars,
                                streamed_chars,
                            )
                        if (
                            slack_event.get("type")
                            in {
                                "assistant",
                                "item.agentMessage.delta",
                                "result",
                                "turn.done",
                            }
                            and slackbot_streamed_answer_chars
                            > previous_streamed_answer_chars
                        ):
                            slackbot_text_sent = True
            observations.raw_event_count += 1
            # ``amp_raw_event`` is a HISTORICAL label preserved for API and
            # dashboard back-compat. Despite the name, this row stores the
            # raw stream payload from WHICHEVER harness produced the event
            # (amp, codex, claude-code, pi-mono). To filter by actual engine
            # use the projected observation events emitted below, or join
            # this row's thread_key against ``agent_runtime_assignments``.
            # If you rename this label, update at least:
            #   - packages/api-client/test/client.test.ts (asserts "amp_raw_event")
            #   - _VALID_STDOUT_EVENT_TYPES in services/api/api/agent.py
            #   - any Grafana / VictoriaLogs dashboards keyed on it.
            await append_execution_event(
                pool,
                thread_key=thread_key,
                execution_id=execution_id,
                event_kind="amp_raw_event",
                event_json=payload,
            )
            for canonical_event in canonical_events:
                projected = project_execution_observations(
                    canonical_event,
                    execution_id=execution_id,
                    thread_key=thread_key,
                    assignment_generation=assignment_generation,
                    harness=harness,
                    engine=engine,
                    persona_id=persona_id,
                    prompt_ref=prompt_ref,
                    prompt_sha=prompt_sha,
                )
                for event_kind, observation_payload in projected:
                    await append_execution_event(
                        pool,
                        thread_key=thread_key,
                        execution_id=execution_id,
                        event_kind=event_kind,
                        event_json=observation_payload,
                    )
                    log.info(event_kind, **observation_payload)
                    add_span_event(
                        f"centaur.observation.{event_kind}",
                        {
                            "centaur.execution_id": execution_id,
                            "centaur.thread_key": thread_key,
                            "centaur.event_kind": event_kind,
                            "centaur.tool.name": observation_payload.get("tool_name"),
                            "centaur.model": observation_payload.get("model"),
                            "centaur.error_category": observation_payload.get(
                                "error_category"
                            ),
                        },
                    )
                    was_first_token = not observations.first_token_seen
                    observations.observe(event_kind, observation_payload)
                    if was_first_token and observations.first_token_seen:
                        first_token_at = dt.datetime.now(dt.timezone.utc)
                    if event_kind in (
                        "assistant_tool_use_observed",
                        "tool_result_observed",
                    ):
                        tool_name = observation_payload.get("tool_name")
                        error_category = observation_payload.get("error_category")
                        if error_category and tool_name is None:
                            tool_name = observations.tool_use_to_name.get(
                                observation_payload.get("tool_use_id", "")
                            )
                        if error_category and tool_name:
                            record_tool_error_category(tool_name, error_category)
                    if event_kind == "usage_observed":
                        usage_metrics = extract_usage_metrics(
                            {
                                "input_tokens": observation_payload.get("input_tokens"),
                                "output_tokens": observation_payload.get(
                                    "output_tokens"
                                ),
                                "cache_creation_input_tokens": observation_payload.get(
                                    "cache_creation_input_tokens"
                                ),
                                "cache_read_input_tokens": observation_payload.get(
                                    "cache_read_input_tokens"
                                ),
                                "cost_usd": observation_payload.get("cost_usd"),
                            },
                            model=observation_payload.get("model")
                            if isinstance(observation_payload.get("model"), str)
                            else None,
                        )
                        record_usage_observation(
                            harness=harness,
                            model=usage_metrics.get("model"),
                            input_tokens=usage_metrics["input_tokens"],
                            output_tokens=usage_metrics["output_tokens"],
                            cache_creation_input_tokens=usage_metrics[
                                "cache_creation_input_tokens"
                            ],
                            cache_read_input_tokens=usage_metrics[
                                "cache_read_input_tokens"
                            ],
                            cost_usd=usage_metrics["cost_usd"],
                        )
            silence_deadline = await _touch_execution_progress(
                pool,
                execution_id,
                timeout_s=_progress_silence_timeout_s(
                    payload,
                    canonical_events=canonical_events,
                    observations=observations,
                ),
            )

            status_row = await pool.fetchrow(
                "SELECT status FROM agent_execution_requests WHERE execution_id = $1",
                execution_id,
            )
            if status_row and status_row["status"] == "cancel_requested":
                await _finalize_execution(
                    status="cancelled",
                    terminal_reason="cancel_requested",
                    result_text="",
                    error_text="cancel_requested",
                )
                return

            if payload.get("type") == "turn.done":
                turn_done_event = payload
                break
    except Exception as exc:
        status_row = await pool.fetchrow(
            "SELECT status FROM agent_execution_requests WHERE execution_id = $1",
            execution_id,
        )
        if status_row and status_row["status"] == "cancel_requested":
            await _finalize_execution(
                status="cancelled",
                terminal_reason="cancel_requested",
                result_text="",
                error_text="cancel_requested",
            )
            return
        await _finalize_execution(
            status="failed_permanent",
            terminal_reason="execution_error",
            result_text="",
            error_text=str(exc),
        )
        return
    finally:
        if pending_event is not None:
            pending_event.cancel()
            with contextlib.suppress(
                asyncio.CancelledError, StopAsyncIteration, Exception
            ):
                await pending_event
        with contextlib.suppress(Exception):
            await stream.aclose()
        with contextlib.suppress(Exception):
            await backend.close_streams(session)

    if turn_done_event is None:
        completed_result = await _completed_session_result_for_reclaimed_turn(
            pool,
            thread_key=thread_key,
            durable_turn_id=durable_turn_id,
        )
        if completed_result is not None:
            log.info(
                "execution_stream_recovered_completed_session_result",
                execution_id=execution_id,
                thread_key=thread_key,
                durable_turn_id=durable_turn_id,
                result_size_bytes=payload_size_bytes(completed_result),
            )
            await _finalize_execution(
                status="completed",
                terminal_reason="completed_reclaimed_session_result",
                result_text=completed_result,
                error_text=None,
            )
            return
        status_row = await pool.fetchrow(
            "SELECT status FROM agent_execution_requests WHERE execution_id = $1",
            execution_id,
        )
        if status_row and execution_terminal(str(status_row["status"] or "")):
            log.info(
                "execution_stream_end_ignored_after_terminal",
                execution_id=execution_id,
                thread_key=thread_key,
                status=status_row["status"],
            )
            return
        if status_row and status_row["status"] == "cancel_requested":
            await _stop_execution_session(
                thread_key,
                reason="cancel_requested",
            )
            await _finalize_execution(
                status="cancelled",
                terminal_reason="cancel_requested",
                result_text="",
                error_text="cancel_requested",
            )
            return

        session_status = "gone"
        with contextlib.suppress(Exception):
            session_status = await backend.status(session)

        if session_status in {"running", "created"}:
            await pool.execute(
                "UPDATE agent_execution_requests SET "
                "status = 'retry_wait', "
                "worker_id = NULL, "
                "worker_lease_expires_at = NOW() + make_interval(secs => $1::double precision), "
                "silence_deadline_at = GREATEST("
                "  COALESCE(silence_deadline_at, NOW()), "
                "  NOW() + make_interval(secs => $2::double precision)"
                "), "
                "stream_break_count = stream_break_count + 1, "
                "last_stream_break_at = NOW(), "
                "updated_at = NOW() "
                "WHERE execution_id = $3 AND status = 'running'",
                float(EXECUTION_STREAM_EOF_RETRY_DELAY_S),
                float(
                    max(EXECUTION_TOOL_SILENCE_TIMEOUT_S, EXECUTION_SILENCE_TIMEOUT_S)
                ),
                execution_id,
            )
            log.warning(
                "execution_stream_interrupted_retry_wait",
                execution_id=execution_id,
                thread_key=thread_key,
                runtime_id=session.sandbox_id,
                session_status=session_status,
                retry_delay_s=EXECUTION_STREAM_EOF_RETRY_DELAY_S,
            )
            return

        await _finalize_execution(
            status="failed_permanent",
            terminal_reason="stream_ended_without_turn_done",
            result_text="",
            error_text="stream ended before terminal turn.done",
        )
        await _stop_execution_session(
            thread_key,
            reason="stream_ended_without_turn_done",
        )
        return

    result_text = extract_result(engine, turn_done_event) or latest_terminal_result_text
    repo_context = _extract_repo_context(turn_done_event)
    if repo_context:
        await _merge_execution_repo_context(pool, execution_id, repo_context)
    error_text = turn_done_event.get("error")
    if not isinstance(error_text, str):
        error_text = ""
    is_error = bool(turn_done_event.get("is_error")) or bool(error_text)
    if is_error:
        terminal_reason = "harness_error"
        combined_error = (error_text or result_text or "harness_error").strip()
        if _matches_user_cancelled(error_text, result_text, combined_error):
            await _finalize_execution(
                status="cancelled",
                terminal_reason="cancel_requested",
                result_text="",
                error_text="cancel_requested",
            )
            return
        if _matches_raw_harness_auth_failure(error_text, result_text, combined_error):
            retry_attempt = _raw_harness_auth_retry_attempt(execution_metadata)
            if retry_attempt < _RAW_HARNESS_AUTH_RETRY_LIMIT:
                await _requeue_execution_after_raw_harness_auth_failure(
                    pool,
                    execution_id=execution_id,
                    thread_key=thread_key,
                    metadata=execution_metadata,
                    combined_error=combined_error,
                )
                return
            terminal_reason = "harness_auth_failed"
            await _stop_execution_session(
                thread_key,
                reason="raw_harness_auth_failed",
            )
            log.warning(
                "execution_raw_harness_auth_failure_retry_exhausted",
                execution_id=execution_id,
                thread_key=thread_key,
                retry_attempt=retry_attempt,
            )
            await _finalize_execution(
                status="failed_permanent",
                terminal_reason=terminal_reason,
                result_text=_RAW_HARNESS_AUTH_SAFE_FAILURE_MESSAGE,
                error_text=combined_error,
            )
            return
        if "timed out while reconnecting" in combined_error.lower():
            terminal_reason = "amp_reconnect_timeout"
        await _finalize_execution(
            status="failed_permanent",
            terminal_reason=terminal_reason,
            result_text=result_text,
            error_text=combined_error,
        )
        return

    await _finalize_execution(
        status="completed",
        terminal_reason="completed",
        result_text=result_text,
        error_text=None,
    )


def _updated_count(command_tag: str) -> int:
    with contextlib.suppress(Exception):
        return int(str(command_tag).rsplit(" ", 1)[-1])
    return 0


async def _recover_stale_running(pool) -> int:
    result = await pool.execute(
        "UPDATE agent_execution_requests SET "
        "status = CASE WHEN status IN ('running', 'retry_wait') THEN 'queued' ELSE status END, "
        "worker_id = NULL, worker_lease_expires_at = NULL, updated_at = NOW() "
        "WHERE status IN ('running', 'retry_wait', 'cancel_requested') "
        "AND (worker_lease_expires_at IS NULL OR worker_lease_expires_at <= NOW())",
    )
    # claimed_at is intentionally preserved across requeue so the next claim
    # treats this row as a reclaim (preserving hard_deadline_at) rather than a
    # first claim (which would re-anchor the deadline and lose the watchdog).
    recovered = _updated_count(result)
    if recovered:
        log.warning(
            "execution_requeued_after_stale_lease",
            recovered=recovered,
        )
    return recovered


async def recover_interrupted_executions_on_startup(pool) -> int:
    """Recover interrupted executions whose worker lease is already stale."""
    recovered = await _recover_stale_running(pool)
    if recovered:
        log.warning("startup_execution_requeued", recovered=recovered)
    reconciled = await _reconcile_orphaned_executions(
        pool, limit=_RECONCILE_STARTUP_LIMIT
    )
    if reconciled:
        log.warning("startup_orphaned_executions_reconciled", reconciled=reconciled)
    return recovered


async def _recover_stale_running_if_due(pool) -> None:
    global _last_recover_stale_running_at
    now = asyncio.get_running_loop().time()
    if now - _last_recover_stale_running_at < EXECUTION_STALE_RECOVERY_INTERVAL_S:
        return
    async with _recover_stale_running_lock:
        now = asyncio.get_running_loop().time()
        if now - _last_recover_stale_running_at < EXECUTION_STALE_RECOVERY_INTERVAL_S:
            return
        await _recover_stale_running(pool)
        await _reconcile_orphaned_executions(pool)
        _last_recover_stale_running_at = now


async def _reconcile_orphaned_executions(pool, *, limit: int = 0) -> int:
    grace_seconds = float(EXECUTION_HARD_TIMEOUT_S) * 2.0
    sql = (
        "SELECT execution_id, thread_key FROM agent_execution_requests "
        "WHERE status IN ('queued', 'running', 'retry_wait', 'cancel_requested') "
        "AND COALESCE(hard_deadline_at, created_at + make_interval(secs => $1::double precision)) < NOW() "
        "ORDER BY COALESCE(hard_deadline_at, created_at + make_interval(secs => $1::double precision)) ASC"
    )
    args: list = [grace_seconds]
    if limit > 0:
        sql += " LIMIT $2"
        args.append(limit)
    rows = await pool.fetch(sql, *args)
    if not rows:
        return 0
    reconciled = 0
    for row in rows:
        execution_id = str(row["execution_id"])
        thread_key = str(row["thread_key"])
        try:
            await _mark_execution_terminal(
                pool,
                execution_id=execution_id,
                thread_key=thread_key,
                status="failed_permanent",
                terminal_reason="hard_deadline_reaped",
                result_text="",
                error_text="execution outlived hard deadline without finalizing",
            )
            reconciled += 1
        except Exception:
            log.warning(
                "hard_deadline_reap_failed",
                execution_id=execution_id,
                thread_key=thread_key,
                exc_info=True,
            )
    if reconciled:
        log.warning("hard_deadline_reaped", reconciled=reconciled)
    return reconciled


async def _execution_worker_loop(pool) -> None:
    while True:
        try:
            await _recover_stale_running_if_due(pool)
            row = await _claim_next_execution(pool)
            if row is None:
                _worker_wake.clear()
                try:
                    await asyncio.wait_for(
                        _worker_wake.wait(), timeout=EXECUTION_RECONCILE_INTERVAL_S
                    )
                except TimeoutError:
                    pass
                continue
            await _process_execution(pool, row)
        except asyncio.CancelledError:
            raise
        except Exception:
            log.warning("execution_worker_tick_error", exc_info=True)
            await asyncio.sleep(0.5)


async def start_execution_worker(pool) -> None:
    global _last_recover_stale_running_at, _worker_tasks
    if any(not task.done() for task in _worker_tasks):
        return
    _last_recover_stale_running_at = 0.0
    _worker_tasks = [
        asyncio.create_task(
            _execution_worker_loop(pool),
            name=f"execution-worker-{index + 1}",
        )
        for index in range(EXECUTION_WORKER_CONCURRENCY)
    ]


async def stop_execution_worker() -> None:
    global _last_recover_stale_running_at, _worker_tasks
    if not _worker_tasks:
        return
    tasks = _worker_tasks
    _worker_tasks = []
    _last_recover_stale_running_at = 0.0
    for task in tasks:
        task.cancel()
    for task in tasks:
        with contextlib.suppress(asyncio.CancelledError):
            await task


def wake_execution_worker() -> None:
    _worker_wake.set()
