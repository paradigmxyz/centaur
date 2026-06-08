"""Normalize raw harness events into canonical event dicts.

1:1 port of packages/harness-events/src/normalize.ts.  Pure functions — no I/O,
no globals, no imports from other api modules.

Public API:
    normalize_harness_event(engine, event) -> list[dict]
"""

from __future__ import annotations

import hashlib
import json
from typing import Any


# ---------------------------------------------------------------------------
# Parse helpers
# ---------------------------------------------------------------------------


def _as_str(value: Any) -> str:
    return value if isinstance(value, str) else ""


def _as_record(value: Any) -> dict:
    return value if isinstance(value, dict) else {}


def _as_list(value: Any) -> list:
    return value if isinstance(value, list) else []


def _parse_dictish(value: Any) -> dict:
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
            if isinstance(parsed, dict):
                return parsed
        except (json.JSONDecodeError, TypeError):
            pass
    return {}


# ---------------------------------------------------------------------------
# Stable tool call ID
# ---------------------------------------------------------------------------


def _stable_sorted_json(value: Any) -> str:
    """Produce a stable JSON string with sorted keys (matches TS version)."""
    return json.dumps(
        value, sort_keys=True, separators=(", ", ": "), ensure_ascii=False
    )


def _stable_tool_call_id(name: str, tool_input: Any, nonce: str = "") -> str:
    payload = {
        "input": tool_input if isinstance(tool_input, dict) else {},
        "name": name or "tool",
        "nonce": nonce or "",
    }
    h = hashlib.sha1(_stable_sorted_json(payload).encode()).hexdigest()[:12]
    return f"tool-call-{h}"


# ---------------------------------------------------------------------------
# Factory helpers
# ---------------------------------------------------------------------------


def _assistant_text_event(text: str) -> dict:
    return {
        "type": "assistant",
        "message": {"content": [{"type": "text", "text": text}]},
    }


def _assistant_tool_use_event(tool_call_id: str, name: str, tool_input: Any) -> dict:
    tool_name = _as_str(name) or "tool"
    normalized_input = tool_input if isinstance(tool_input, dict) else {}
    resolved_id = _as_str(tool_call_id).strip() or _stable_tool_call_id(
        tool_name, normalized_input
    )
    return {
        "type": "assistant",
        "message": {
            "content": [
                {
                    "type": "tool_use",
                    "id": resolved_id,
                    "name": tool_name,
                    "input": normalized_input,
                }
            ],
        },
    }


def _tool_result_event(tool_use_id: str, content: Any, is_error: bool = False) -> dict:
    return {
        "type": "tool",
        "content": [
            {"tool_use_id": tool_use_id, "content": content, "is_error": is_error}
        ],
    }


def _subagent_event(
    *,
    status: str,
    subagent_id: str,
    name: str | None = None,
    summary: str | None = None,
    error: str | None = None,
    activity: str | None = None,
    activities: list[dict] | None = None,
) -> dict:
    payload: dict[str, Any] = {
        "type": "subagent",
        "status": status,
        "subagent_id": subagent_id,
    }
    if name is not None:
        payload["name"] = name
    if summary is not None:
        payload["summary"] = summary
    if error is not None:
        payload["error"] = error
    if activity is not None:
        payload["activity"] = activity
    if activities:
        payload["activities"] = activities
    return payload


def _normalize_subagent_status(raw: str) -> str:
    s = raw.strip().lower()
    if s in ("started", "start", "starting"):
        return "started"
    if s in ("working", "running", "in_progress", "progress"):
        return "working"
    if s in ("completed", "done", "complete", "finished", "success"):
        return "completed"
    if s in ("failed", "error", "failure"):
        return "failed"
    return raw


def _first_non_empty(*values: Any) -> str | None:
    for v in values:
        s = _as_str(v).strip()
        if s:
            return s
    return None


def _make_activity(description: Any, tool_name: Any = None) -> dict | None:
    text = _as_str(description).strip()
    if not text:
        return None
    tool = _as_str(tool_name).strip()
    return {"description": text, "toolName": tool} if tool else {"description": text}


def _merge_activities(*items: dict | None) -> list[dict] | None:
    merged: list[dict] = []
    seen: set[str] = set()
    for item in items:
        if item is None:
            continue
        key = f"{item.get('toolName', '')}::{item['description']}"
        if key in seen:
            continue
        seen.add(key)
        merged.append(item)
    return merged if merged else None


# ---------------------------------------------------------------------------
# Usage metadata
# ---------------------------------------------------------------------------


def _usage_payload_from_source(source: dict) -> tuple[dict | None, str | None]:
    message = _as_record(source.get("message"))
    usage = _as_record(message.get("usage"))
    if not usage:
        usage = _as_record(source.get("usage"))
    if not usage:
        return None, None
    model = _as_str(message.get("model")) or _as_str(source.get("model")) or None
    return usage, model


def _attach_usage_metadata(
    events: list[dict],
    source: dict,
    authoritative: bool = False,
) -> list[dict]:
    usage, model = _usage_payload_from_source(source)
    if not usage:
        return events

    if not events:
        passthrough: dict[str, Any] = {"type": "usage", "usage": usage}
        if model:
            passthrough["model"] = model
        if authoritative:
            passthrough["authoritative"] = True
        return [passthrough]

    first = dict(events[0])
    message = _as_record(first.get("message"))
    if message:
        updated: dict[str, Any] = dict(message)
        updated["usage"] = usage
        if model:
            updated["model"] = model
        first["message"] = updated
    else:
        first["usage"] = usage
        if model:
            first["model"] = model
    if authoritative:
        first["authoritative"] = True
    return [first, *events[1:]]


# ---------------------------------------------------------------------------
# Amp / Claude-Code normalizer
# ---------------------------------------------------------------------------


def _normalize_amp_like_event(event: dict) -> list[dict]:
    event_type = _as_str(event.get("type"))

    if event_type == "user":
        message = _as_record(event.get("message"))
        tool_results = []
        for block in _as_list(message.get("content")):
            bd = _as_record(block)
            if _as_str(bd.get("type")) != "tool_result":
                continue
            tool_use_id = _as_str(bd.get("tool_use_id")) or _as_str(
                event.get("parent_tool_use_id")
            )
            if not tool_use_id:
                continue
            tool_results.append(
                {
                    "tool_use_id": tool_use_id,
                    "content": bd.get("content"),
                    "is_error": bool(bd.get("is_error")),
                }
            )
        if tool_results:
            return [{"type": "tool", "content": tool_results}]
        return []

    if event_type in (
        "assistant",
        "reasoning",
        "tool",
        "command_execution",
        "file_change",
    ):
        return [event]

    if event_type == "subagent":
        status = _as_str(event.get("status")).strip()
        subagent_id = _first_non_empty(
            event.get("subagent_id"),
            event.get("task_id"),
            event.get("tool_use_id"),
            event.get("id"),
        )
        if not status or not subagent_id:
            return [event]
        description = _first_non_empty(event.get("description"), event.get("message"))
        tool_name = _first_non_empty(
            event.get("tool_name"),
            event.get("toolName"),
            event.get("last_tool_name"),
            event.get("lastToolName"),
            event.get("active_tool_name"),
            event.get("activeToolName"),
        )
        return [
            _subagent_event(
                status=_normalize_subagent_status(status),
                subagent_id=subagent_id,
                name=_first_non_empty(
                    event.get("name"), event.get("task_name"), description
                ),
                summary=_first_non_empty(event.get("summary"), event.get("result")),
                error=_first_non_empty(event.get("error")),
                activity=description,
                activities=_merge_activities(_make_activity(description, tool_name)),
            )
        ]

    if event_type == "result":
        text = _as_str(event.get("result")) or _as_str(event.get("text"))
        return [{"type": "result", "text": text}] if text else []

    if event_type == "error":
        error_value = event.get("error")
        message = (
            _as_str(error_value)
            or _as_str(_as_record(error_value).get("message"))
            or _as_str(event.get("message"))
            or "Unknown error"
        )
        lowered = message.lower()
        if "restarting (" in lowered and "giving up" not in lowered:
            return []
        return [{"type": "error", "error": message}]

    if event_type == "system":
        subtype = _as_str(event.get("subtype")).strip().lower()
        subagent_id = _first_non_empty(
            event.get("task_id"),
            event.get("subagent_id"),
            event.get("tool_use_id"),
            event.get("parent_tool_use_id"),
            event.get("id"),
        )
        if not subagent_id:
            if subtype == "init":
                session_id = _first_non_empty(event.get("session_id"))
                return (
                    [{"type": "system", "subtype": "init", "session_id": session_id}]
                    if session_id
                    else []
                )
            return []
        description = _first_non_empty(
            event.get("description"), event.get("message"), event.get("text")
        )
        summary = _first_non_empty(
            event.get("summary"),
            event.get("result"),
            event.get("message"),
            event.get("text"),
        )
        name = _first_non_empty(
            event.get("name"), event.get("task_name"), event.get("title"), description
        )
        tool_name = _first_non_empty(
            event.get("tool_name"),
            event.get("toolName"),
            event.get("last_tool_name"),
            event.get("lastToolName"),
            event.get("active_tool_name"),
            event.get("activeToolName"),
        )
        activities = _merge_activities(_make_activity(description, tool_name))

        if subtype in ("task_started", "task_start", "started"):
            return [
                _subagent_event(
                    status="started",
                    subagent_id=subagent_id,
                    name=name or "Delegated task",
                    activity=description,
                    activities=activities,
                )
            ]
        if subtype in ("task_progress", "task_update", "progress", "working"):
            return [
                _subagent_event(
                    status="working",
                    subagent_id=subagent_id,
                    name=name,
                    activity=description,
                    activities=activities,
                )
            ]
        if subtype in (
            "task_notification",
            "task_completed",
            "task_done",
            "completed",
            "done",
        ):
            return [
                _subagent_event(
                    status="completed",
                    subagent_id=subagent_id,
                    name=name,
                    summary=summary or description,
                    activity=description,
                    activities=activities,
                )
            ]
        if subtype in ("task_failed", "task_error", "failed", "error"):
            return [
                _subagent_event(
                    status="failed",
                    subagent_id=subagent_id,
                    name=name,
                    error=_first_non_empty(event.get("error"), event.get("message"))
                    or "Task failed",
                )
            ]
        return []

    if event_type == "stream_event":
        nested = _as_record(event.get("event"))
        nested_type = _as_str(nested.get("type"))
        if nested_type == "error":
            msg = (
                _as_str(_as_record(nested.get("error")).get("message"))
                or "Unknown error"
            )
            return [{"type": "error", "error": msg}]
        if nested_type == "content_block_start":
            block = _as_record(nested.get("content_block"))
            if _as_str(block.get("type")) == "tool_use":
                tool_id = _as_str(block.get("id"))
                name = _as_str(block.get("name")) or "tool"
                return [_assistant_tool_use_event(tool_id, name, block.get("input"))]
        if nested_type == "content_block_delta":
            delta = _as_record(nested.get("delta"))
            delta_type = _as_str(delta.get("type"))
            if delta_type == "text_delta":
                text = _as_str(delta.get("text"))
                return [_assistant_text_event(text)] if text else []
            if delta_type == "thinking_delta":
                text = _as_str(delta.get("thinking"))
                return [{"type": "reasoning", "text": text}] if text else []
        return []

    return []


# ---------------------------------------------------------------------------
# Codex normalizer
# ---------------------------------------------------------------------------


def _codex_tool_name(item: dict) -> str:
    return (
        _as_str(item.get("tool"))
        or _as_str(item.get("toolName"))
        or _as_str(item.get("name"))
        or _as_str(item.get("tool_name"))
        or "tool"
    )


def _codex_tool_input(item: dict) -> dict:
    for key in ("arguments", "input", "args"):
        v = _parse_dictish(item.get(key))
        if v:
            return v
    return {}


def _codex_tool_call_id(item: dict) -> str:
    direct = (
        _as_str(item.get("id"))
        or _as_str(item.get("tool_call_id"))
        or _as_str(item.get("tool_use_id"))
        or _as_str(item.get("toolUseId"))
        or _as_str(item.get("toolCallId"))
        or _as_str(item.get("call_id"))
    )
    if direct:
        return direct
    nonce = (
        _as_str(item.get("index"))
        or _as_str(item.get("position"))
        or _as_str(item.get("ordinal"))
        or _as_str(item.get("event_seq"))
        or _as_str(item.get("timestamp"))
        or _as_str(item.get("created_at"))
    )
    return _stable_tool_call_id(_codex_tool_name(item), _codex_tool_input(item), nonce)


def _normalize_codex_item(item: dict, phase: str) -> list[dict]:
    item_type = _as_str(item.get("type"))

    if item_type in ("agent_message", "agentMessage") and phase == "completed":
        # Codex app-server streams text through item/agentMessage/delta, which
        # the wrapper emits as assistant text events. Keep completed items for
        # result extraction only; re-emitting them duplicates Slack output.
        return []

    if item_type == "reasoning" and phase in ("updated", "completed"):
        text = _as_str(item.get("text")) or _as_str(item.get("thinking"))
        return [{"type": "reasoning", "text": text}] if text else []

    if item_type in (
        "mcp_tool_call",
        "mcpToolCalls",
        "tool_call",
        "toolCall",
        "function_call",
        "functionCall",
        "custom_tool_call",
        "customToolCall",
        "dynamicToolCalls",
        "collabToolCalls",
    ):
        tool_id = _codex_tool_call_id(item)
        tool_name = _codex_tool_name(item)
        if tool_name.strip().lower() == "subagent":
            tool_input = _codex_tool_input(item)
            label = (
                _as_str(tool_input.get("description"))
                or _as_str(tool_input.get("name"))
                or "Delegated subagent"
            )
            if phase == "started":
                return [
                    _subagent_event(status="started", subagent_id=tool_id, name=label)
                ]
            if phase == "updated":
                activity = _first_non_empty(
                    item.get("message"),
                    item.get("status_message"),
                    item.get("progress_message"),
                    item.get("summary"),
                )
                if not activity:
                    return []
                return [
                    _subagent_event(
                        status="working",
                        subagent_id=tool_id,
                        name=label,
                        activity=activity,
                        activities=_merge_activities(
                            _make_activity(
                                activity,
                                _first_non_empty(
                                    item.get("active_tool_name"),
                                    item.get("activeToolName"),
                                    item.get("last_tool_name"),
                                    item.get("lastToolName"),
                                ),
                            )
                        ),
                    )
                ]
            if phase == "completed":
                if item.get("error") is not None:
                    return [
                        _subagent_event(
                            status="failed",
                            subagent_id=tool_id,
                            name=label,
                            error=_as_str(item.get("error")) or "Subagent failed",
                        )
                    ]
                result_summary = _as_str(item.get("result"))
                return [
                    _subagent_event(
                        status="completed",
                        subagent_id=tool_id,
                        name=label,
                        summary=result_summary[:220],
                    )
                ]
            return []
        if phase == "started":
            return [
                _assistant_tool_use_event(tool_id, tool_name, _codex_tool_input(item))
            ]
        if phase == "completed":
            output = item.get("result")
            if output is None and item.get("error") is not None:
                output = item.get("error")
            return [_tool_result_event(tool_id, output, bool(item.get("error")))]
        return []

    if item_type in ("command_execution", "commandExecution"):
        if phase == "completed":
            return [
                {
                    "type": "command_execution",
                    "command": _as_str(item.get("command")),
                    "aggregated_output": _as_str(item.get("aggregated_output"))
                    or _as_str(item.get("output")),
                    "exit_code": item.get("exit_code"),
                    "status": item.get("status"),
                }
            ]
        return []

    if item_type in ("file_change", "fileChange") and phase == "completed":
        changes = item.get("changes")
        return [
            {
                "type": "file_change",
                "changes": changes if isinstance(changes, list) else [],
            }
        ]

    if item_type == "error":
        return [
            {"type": "error", "error": _as_str(item.get("message")) or "Unknown error"}
        ]

    return []


def _normalize_codex_event(event: dict) -> list[dict]:
    event_type = _as_str(event.get("type"))

    if event_type == "thread.started":
        thread_id = _as_str(event.get("thread_id"))
        return (
            [{"type": "system", "subtype": "init", "session_id": thread_id}]
            if thread_id
            else []
        )

    if event_type == "assistant":
        return [event]

    if event_type == "error":
        error = event.get("error")
        if isinstance(error, dict):
            message = _as_str(error.get("message"))
        else:
            message = _as_str(error)
        message = message or _as_str(event.get("message")) or "Unknown error"
        return [
            {"type": "error", "error": message}
        ]

    if event_type == "turn.failed":
        error = _as_record(event.get("error"))
        message = (
            _as_str(error.get("message"))
            or _as_str(event.get("message"))
            or "Turn failed"
        )
        return [{"type": "error", "error": message}]

    if event_type == "turn.completed":
        return _attach_usage_metadata([], event, authoritative=True)

    if event_type in {"item.started", "item.updated", "item.completed"}:
        item = _as_record(event.get("item"))
        if _as_str(item.get("type")) == "error":
            return _normalize_codex_item(item, event_type.rsplit(".", 1)[-1])

    if event_type in {
        "turn.plan.updated",
        "item.started",
        "item.updated",
        "item.completed",
        "item.agentMessage.delta",
        "item.plan.delta",
        "item.commandExecution.outputDelta",
        "item.fileChange.outputDelta",
        "item.fileChange.patchUpdated",
        "item.reasoning.summaryTextDelta",
        "item.reasoning.summaryPartAdded",
        "item.reasoning.textDelta",
    }:
        return [event]

    return []


# ---------------------------------------------------------------------------
# Pi-mono normalizer
# ---------------------------------------------------------------------------


def _normalize_pi_message_content(message: dict) -> list[dict]:
    content = _as_list(message.get("content"))
    normalized: list[dict] = []
    for block in content:
        bd = _as_record(block)
        block_type = _as_str(bd.get("type"))
        if block_type == "text":
            text = _as_str(bd.get("text"))
            if text:
                normalized.append(_assistant_text_event(text))
        elif block_type == "thinking":
            text = _as_str(bd.get("text")) or _as_str(bd.get("thinking"))
            if text:
                normalized.append({"type": "reasoning", "text": text})
        elif block_type in ("tool_call", "toolcall"):
            tc = _as_record(bd.get("toolCall"))
            if not tc:
                tc = bd
            tool_name = _as_str(tc.get("name")) or _as_str(bd.get("name")) or "tool"
            tool_input = _as_record(tc.get("input")) or _as_record(bd.get("input"))
            tool_id = _as_str(tc.get("id")) or _as_str(bd.get("id"))
            normalized.append(_assistant_tool_use_event(tool_id, tool_name, tool_input))
    return normalized


def _normalize_pi_event(event: dict) -> list[dict]:
    event_type = _as_str(event.get("type"))

    if event_type == "session":
        session_id = _as_str(event.get("id"))
        return (
            [{"type": "system", "subtype": "init", "session_id": session_id}]
            if session_id
            else []
        )

    if event_type == "tool_execution_start":
        tool_name = _as_str(event.get("toolName")) or "tool"
        tool_input = _as_record(event.get("args"))
        tool_id = _as_str(event.get("toolCallId"))
        if not tool_id:
            nonce = (
                _as_str(event.get("toolExecutionId"))
                or _as_str(event.get("executionId"))
                or _as_str(event.get("id"))
            )
            tool_id = _stable_tool_call_id(tool_name, tool_input, nonce)
        if tool_name.strip().lower() == "subagent":
            label = (
                _as_str(tool_input.get("description"))
                or _as_str(tool_input.get("name"))
                or "Delegated subagent"
            )
            return [_subagent_event(status="started", subagent_id=tool_id, name=label)]
        return [_assistant_tool_use_event(tool_id, tool_name, tool_input)]

    if event_type == "tool_execution_end":
        tool_id = _as_str(event.get("toolCallId"))
        if not tool_id:
            tool_name = _as_str(event.get("toolName")) or "tool"
            tool_input = _as_record(event.get("args"))
            nonce = (
                _as_str(event.get("toolExecutionId"))
                or _as_str(event.get("executionId"))
                or _as_str(event.get("id"))
            )
            if not nonce:
                return []
            tool_id = _stable_tool_call_id(tool_name, tool_input, nonce)
        if _as_str(event.get("toolName")).strip().lower() == "subagent":
            if event.get("isError"):
                return [
                    _subagent_event(
                        status="failed",
                        subagent_id=tool_id,
                        error=_as_str(event.get("error")) or "Subagent failed",
                    )
                ]
            result_summary = _as_str(event.get("result"))
            return [
                _subagent_event(
                    status="completed",
                    subagent_id=tool_id,
                    summary=result_summary[:220],
                )
            ]
        return [
            _tool_result_event(tool_id, event.get("result"), bool(event.get("isError")))
        ]

    if event_type == "tool_execution_update":
        tool_name = _as_str(event.get("toolName")) or "tool"
        tool_id = _as_str(event.get("toolCallId"))
        if not tool_id:
            tool_input = _as_record(event.get("args"))
            nonce = (
                _as_str(event.get("toolExecutionId"))
                or _as_str(event.get("executionId"))
                or _as_str(event.get("id"))
            )
            tool_id = _stable_tool_call_id(tool_name, tool_input, nonce)
        if tool_name.strip().lower() != "subagent":
            return []
        tool_input = _as_record(event.get("args"))
        label = (
            _as_str(tool_input.get("description"))
            or _as_str(tool_input.get("name"))
            or "Delegated subagent"
        )
        activity = _first_non_empty(
            event.get("message"),
            event.get("statusMessage"),
            event.get("progress_message"),
            event.get("summary"),
        )
        if not activity:
            return []
        return [
            _subagent_event(
                status="working",
                subagent_id=tool_id,
                name=label,
                activity=activity,
                activities=_merge_activities(
                    _make_activity(
                        activity,
                        _first_non_empty(
                            event.get("active_tool_name"),
                            event.get("activeToolName"),
                            event.get("last_tool_name"),
                            event.get("lastToolName"),
                        ),
                    )
                ),
            )
        ]

    if event_type == "message_end":
        message = _as_record(event.get("message"))
        if _as_str(message.get("role")) != "assistant":
            return []
        normalized = _attach_usage_metadata(
            _normalize_pi_message_content(message),
            message,
        )
        stop_reason = _as_str(message.get("stopReason"))
        if stop_reason in ("error", "aborted"):
            error_text = _as_str(message.get("errorMessage")) or "Assistant run failed"
            return [*normalized, {"type": "error", "error": error_text}]
        return normalized

    if event_type == "agent_end":
        messages = _as_list(event.get("messages"))
        if not messages:
            return []
        assistant_msgs = [
            m for m in messages if _as_str(_as_record(m).get("role")) == "assistant"
        ]
        if not assistant_msgs:
            return []
        last = _as_record(assistant_msgs[-1])
        stop_reason = _as_str(last.get("stopReason"))
        if stop_reason in ("error", "aborted"):
            error_text = _as_str(last.get("errorMessage")) or "Assistant run failed"
            return [{"type": "error", "error": error_text}]
        return []

    return []


# ---------------------------------------------------------------------------
# Main dispatcher
# ---------------------------------------------------------------------------

_ENGINE_HARNESSES = {"amp", "claude-code", "codex", "pi-mono"}


def normalize_harness_event(engine: str, event: dict) -> list[dict]:
    """Normalize a raw harness event into canonical event dicts.

    Parameters
    ----------
    engine:
        The engine name (``amp``, ``claude-code``, ``codex``, ``pi-mono``).
        Persona names (e.g. ``legal``, ``eng``) are treated as amp-like.
    event:
        A single raw JSON dict from harness stdout.

    Returns
    -------
    list[dict]
        Zero or more canonical event dicts.
    """
    normalized = (engine or "").strip().lower()

    if not normalized:
        event_type = _as_str(event.get("type"))
        if (
            event_type.startswith("item.")
            or event_type.startswith("turn.")
            or event_type == "thread.started"
        ):
            normalized = "codex"
        elif event_type in (
            "session",
            "agent_start",
            "agent_end",
            "message_start",
            "message_update",
            "message_end",
            "tool_execution_start",
            "tool_execution_update",
            "tool_execution_end",
        ):
            normalized = "pi-mono"
        else:
            normalized = "amp"

    if normalized == "codex":
        return _normalize_codex_event(event)
    if normalized == "pi-mono":
        return _normalize_pi_event(event)
    # Personas (legal, eng, etc.) use amp/claude-code format
    return _normalize_amp_like_event(event)
