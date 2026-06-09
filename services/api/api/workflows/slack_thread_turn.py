"""Workflow: single agent turn in a Slack thread."""

from __future__ import annotations

from dataclasses import dataclass, field
import json
import re
from typing import Any

from api.runtime_control import ControlPlaneError
from api.workflow_engine import Delivery, WorkflowContext

WORKFLOW_NAME = "slack_thread_turn"

_EXECUTION_HARNESSES = frozenset({"amp", "claude-code", "codex", "pi-mono"})
_PROMPT_FLAG_ALIASES = {
    "claude": "claude-code",
    "pi": "pi-mono",
}
_PROMPT_FLAG_SKIP = frozenset({"engine", "model", "opus", "sonnet", "haiku"})
_PROMPT_FLAG_VALUE_SKIP = frozenset({"engine", "model"})
_PROMPT_FLAG_RE = re.compile(
    r"(^|\s)(`?)(--|[\u2013\u2014])([a-z][a-z0-9-]*)(?=\s|`|$)",
    re.IGNORECASE,
)
_BARE_PERSONA_PROMPT = (
    "Briefly introduce yourself using your active persona instructions and ask what "
    "we should work on."
)
_PROMPT_SWITCH_CONTEXT_NOTE = (
    "You are being invoked mid-thread with a new active persona. Use the preceding "
    "Slack thread history as context, then answer the latest user request in that persona."
)

_RECOVERY_COMMANDS = frozenset(
    {
        "again",
        "continue",
        "do it again",
        "finish the job",
        "go again",
        "look at the root of this thread",
        "look at the root of this thread and try again",
        "look at root of this thread",
        "look at root of this thread and try again",
        "please continue",
        "please rerun",
        "please resume",
        "please retry",
        "reread the thread",
        "reread the thread and try again",
        "rerun",
        "resume",
        "retry",
        "run it again",
        "try again",
    }
)
_RECOVERY_NORMALIZE_RE = re.compile(r"[^a-z0-9\s]+")
_SLACK_ID_MENTION_RE = re.compile(r"^<@[WU][A-Z0-9]+>\s*[:,;-]?\s*(.*)$", re.IGNORECASE)
_RECOVERY_CONTEXT_PREFIX = "Previous unresolved user request from this thread:\n"


@dataclass(frozen=True)
class PromptSelection:
    """Result of parsing ``--harness``/``--persona`` flags from a Slack turn.

    Both fields are optional and orthogonal: ``--invest`` sets only
    ``persona``, ``--claude`` sets only ``harness``, and ``--invest --claude``
    sets both. The downstream resolver applies ``harness`` as the engine
    override and ``persona`` as the system-prompt overlay.
    """

    harness: str | None
    persona: str | None
    parts: list[dict[str, Any]]


@dataclass
class Input:
    thread_key: str = ""
    parts: list[dict[str, Any]] = field(default_factory=list)
    text: str | None = None
    message_id: str | None = None
    user_id: str | None = None
    metadata: dict[str, Any] = field(default_factory=dict)
    history_messages: list[dict[str, Any]] = field(default_factory=list)
    delivery: Delivery = field(default_factory=Delivery)
    harness: str | None = None
    persona: str | None = None
    agents_md_override: str | None = None

    @property
    def effective_parts(self) -> list[dict[str, Any]]:
        if self.parts:
            return [p for p in self.parts if isinstance(p, dict)]
        if self.text and self.text.strip():
            return [{"type": "text", "text": self.text.strip()}]
        raise ControlPlaneError(
            "INVALID_WORKFLOW_INPUT",
            "workflow input must include non-empty parts or text",
            422,
        )


def _known_personas() -> set[str]:
    try:
        from api.app import get_tool_manager

        return set(get_tool_manager().personas)
    except Exception:
        # Workflow unit tests and early startup paths may not have the app-level
        # tool manager available. Harness selectors still work; persona
        # selectors will be validated once the app is fully loaded.
        return set()


def _strip_ranges(text: str, ranges: list[tuple[int, int]]) -> str:
    cleaned = text
    for start, end in sorted(ranges, reverse=True):
        cleaned = f"{cleaned[:start]} {cleaned[end:]}"
    return re.sub(r"\s+", " ", cleaned).strip()


def _extend_value_skip(text: str, end: int) -> int:
    match = re.match(r"\s+[A-Za-z0-9._/-]+", text[end:])
    return end + match.end() if match else end


def _classify_flag(flag: str, personas: set[str]) -> tuple[str | None, str | None]:
    """Map a flag name to ``(harness, persona)``; ``(None, None)`` if unknown."""
    resolved = _PROMPT_FLAG_ALIASES.get(flag, flag)
    if resolved in _EXECUTION_HARNESSES:
        return resolved, None
    if resolved in personas or flag in personas:
        return None, resolved
    return None, None


def _extract_prompt_selection_from_text(
    text: str,
    *,
    personas: set[str],
) -> tuple[str | None, str | None, str]:
    """Strip known flags and return ``(harness, persona, cleaned_text)``."""

    harness: str | None = None
    persona: str | None = None
    ranges: list[tuple[int, int]] = []
    for match in _PROMPT_FLAG_RE.finditer(text):
        leading = match.group(1) or ""
        opening_tick = match.group(2) or ""
        marker = match.group(3) or ""
        flag = match.group(4).lower()

        flag_start = match.start() + len(leading) + len(opening_tick)
        flag_end = flag_start + len(marker) + len(flag)
        strip_start = flag_start - len(opening_tick) if opening_tick else flag_start
        strip_end = flag_end + 1 if flag_end < len(text) and text[flag_end] == "`" else flag_end
        if flag in _PROMPT_FLAG_VALUE_SKIP:
            strip_end = _extend_value_skip(text, strip_end)
        closing_tick = -1
        if opening_tick and strip_end < len(text):
            if text[strip_end] == "`":
                strip_end += 1
            else:
                closing_tick = text.find("`", strip_end)

        is_skip = flag in _PROMPT_FLAG_SKIP
        classified_harness, classified_persona = _classify_flag(flag, personas)
        recognized = is_skip or classified_harness or classified_persona
        if not recognized:
            continue

        ranges.append((strip_start, strip_end))
        if closing_tick > strip_end:
            ranges.append((closing_tick, closing_tick + 1))
        if classified_harness:
            harness = classified_harness
        if classified_persona:
            persona = classified_persona

    cleaned = _strip_ranges(text, ranges) if ranges else text.strip()
    return harness, persona, cleaned


def _extract_prompt_selection(
    parts: list[dict[str, Any]],
    *,
    explicit_harness: str | None = None,
    explicit_persona: str | None = None,
    personas: set[str] | None = None,
) -> PromptSelection:
    """Strip ``--harness``/``--persona`` flags and return what survived.

    Caller-supplied ``explicit_harness``/``explicit_persona`` win over any
    flag the user typed inline.
    """
    known_personas = personas if personas is not None else _known_personas()
    harness: str | None = None
    persona: str | None = None
    cleaned_parts: list[dict[str, Any]] = []
    has_non_text_part = False

    for part in parts:
        if part.get("type") != "text" or not isinstance(part.get("text"), str):
            cleaned_parts.append(part)
            has_non_text_part = True
            continue

        part_harness, part_persona, cleaned_text = _extract_prompt_selection_from_text(
            part["text"],
            personas=known_personas,
        )
        if part_harness:
            harness = part_harness
        if part_persona:
            persona = part_persona
        if cleaned_text:
            cleaned_parts.append({**part, "text": cleaned_text})

    harness = (explicit_harness or harness or "").strip().lower() or None
    persona = (explicit_persona or persona or "").strip().lower() or None
    if harness:
        harness = _PROMPT_FLAG_ALIASES.get(harness, harness)

    # A bare persona selector with no remaining prose deserves a friendly
    # intro turn instead of failing the workflow.
    if persona and not harness and not cleaned_parts and not has_non_text_part:
        cleaned_parts.append({"type": "text", "text": _BARE_PERSONA_PROMPT})

    # Do not turn a model-only hint like "--opus" into an invalid empty turn.
    if not cleaned_parts:
        cleaned_parts = parts

    return PromptSelection(harness=harness, persona=persona, parts=cleaned_parts)


def _with_prompt_switch_context_note(
    parts: list[dict[str, Any]],
    *,
    switched: bool,
    history_messages: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    if not switched or not history_messages:
        return parts
    return [{"type": "text", "text": _PROMPT_SWITCH_CONTEXT_NOTE}, *parts]


async def _release_and_reset_session(
    ctx: WorkflowContext,
    *,
    thread_key: str,
    release_id: str,
) -> None:
    from api.runtime_control import release_assignment

    await release_assignment(
        ctx._pool,
        thread_key=thread_key,
        release_id=release_id,
        cancel_inflight=True,
        stop_runtime_background=True,
    )
    await ctx._pool.execute(
        "UPDATE sandbox_sessions SET "
        "state = 'stopped', "
        "agent_thread_id = NULL, last_delivered_id = NULL, "
        "inflight_turn_id = NULL, inflight_turn_input = NULL, inflight_attempts = 0, "
        "last_result = NULL, last_result_at = NULL, updated_at = NOW() "
        "WHERE thread_key = $1",
        thread_key,
    )


async def _release_for_prompt_switch(
    ctx: WorkflowContext,
    *,
    thread_key: str,
    message_id: str | None,
) -> None:
    await _release_and_reset_session(
        ctx,
        thread_key=thread_key,
        release_id=f"prompt-switch:{message_id or ctx.run_id}",
    )


async def _reset_session_for_recovery(
    ctx: WorkflowContext,
    *,
    thread_key: str,
    message_id: str | None,
) -> None:
    """Reset a broken session when the user explicitly asks for a retry.

    The harness session id (agent_thread_id) survives sandbox replacement, but
    harness-local state (e.g. codex rollout files) does not — a thread whose
    last execution failed permanently can be stuck re-attempting an impossible
    resume forever. An explicit recovery command after a permanent failure
    releases the assignment and clears the session so the turn runs on a fresh
    runtime with history backfill instead.
    """
    row = await ctx._pool.fetchrow(
        "SELECT status FROM agent_execution_requests "
        "WHERE thread_key = $1 ORDER BY created_at DESC LIMIT 1",
        thread_key,
    )
    if row is None or row["status"] != "failed_permanent":
        return
    await _release_and_reset_session(
        ctx,
        thread_key=thread_key,
        release_id=f"recovery-reset:{message_id or ctx.run_id}",
    )


async def _should_backfill_history(
    ctx: WorkflowContext,
    *,
    thread_key: str,
    switched: bool,
    history_messages: list[dict[str, Any]],
) -> bool:
    if not history_messages:
        return False
    if switched:
        return True

    from api.runtime_control import get_active_assignment

    return await get_active_assignment(ctx._pool, thread_key) is None


def _normalize_recovery_command(text: str) -> str:
    normalized = " ".join(_RECOVERY_NORMALIZE_RE.sub(" ", text.lower()).split())
    if normalized in _RECOVERY_COMMANDS:
        return normalized

    # Slack app_mention event text uses ID mentions such as "<@U123> retry".
    # Strip only that protocol shape so display-name prose stays conversational.
    stripped = text.lstrip()
    match = _SLACK_ID_MENTION_RE.match(stripped)
    if match:
        candidate = " ".join(_RECOVERY_NORMALIZE_RE.sub(" ", match.group(1).lower()).split())
        if candidate in _RECOVERY_COMMANDS:
            return candidate

    return normalized


def _extract_text_parts(parts: Any) -> str | None:
    if isinstance(parts, str):
        try:
            parts = json.loads(parts)
        except json.JSONDecodeError:
            return None
    if not isinstance(parts, list):
        return None
    snippets = [
        part["text"].strip()
        for part in parts
        if isinstance(part, dict)
        and part.get("type") == "text"
        and isinstance(part.get("text"), str)
        and part["text"].strip()
    ]
    if not snippets:
        return None
    return "\n\n".join(snippets)


def _is_recovery_turn(parts: list[dict[str, Any]]) -> bool:
    text = _extract_text_parts(parts)
    if text is None or len(parts) != 1:
        return False
    return _normalize_recovery_command(text) in _RECOVERY_COMMANDS


def _lookup_last_unresolved_ask_from_history(
    history_messages: list[dict[str, Any]],
    *,
    user_id: str | None,
    current_message_id: str | None,
) -> tuple[str | None, dict[str, Any]]:
    for item in reversed(history_messages):
        if not isinstance(item, dict):
            continue
        message_id = str(item.get("message_id") or item.get("messageId") or "").strip()
        if current_message_id and message_id == current_message_id:
            continue
        history_user_id = item.get("user_id") or item.get("userId")
        if user_id and history_user_id and history_user_id != user_id:
            continue
        text = _extract_text_parts(item.get("parts"))
        if not text:
            continue
        if _normalize_recovery_command(text) in _RECOVERY_COMMANDS:
            continue
        return text, {
            "hydrated_from_message_id": message_id or None,
            "hydrated_from_user_id": history_user_id,
            "hydrated_from_source": "workflow_history",
        }
    return None, {}


async def _lookup_last_unresolved_ask(
    ctx: WorkflowContext,
    *,
    thread_key: str,
    user_id: str | None,
    before_message_id: str | None,
) -> tuple[str | None, dict[str, Any]]:
    """Find the latest substantive prior user ask in this thread.

    Bounded by:
    - the current retry message's created_at (so a delayed/replayed workflow
      cannot pull in a later user's substantive ask), and
    - the same user_id when one is provided (so retries by user A don't
      hydrate from user B's request in the same Slack thread).

    Returns (text, provenance_meta) so the caller can persist where the
    context came from.
    """

    cursor_ts = None
    if before_message_id:
        cursor_row = await ctx._pool.fetchrow(
            "SELECT created_at FROM chat_messages WHERE thread_key = $1 AND id = $2",
            thread_key,
            before_message_id,
        )
        if cursor_row:
            cursor_ts = cursor_row["created_at"]

    where_clauses = ["thread_key = $1", "role = 'user'"]
    params: list[Any] = [thread_key]
    if cursor_ts is not None:
        params.append(cursor_ts)
        where_clauses.append(f"created_at < ${len(params)}")
    if user_id:
        params.append(user_id)
        where_clauses.append(f"user_id = ${len(params)}")

    sql = (
        "SELECT id, parts, created_at, user_id FROM chat_messages "
        f"WHERE {' AND '.join(where_clauses)} "
        "ORDER BY created_at DESC LIMIT 25"
    )
    rows = await ctx._pool.fetch(sql, *params)
    for row in rows:
        text = _extract_text_parts(row["parts"])
        if not text:
            continue
        if _normalize_recovery_command(text) in _RECOVERY_COMMANDS:
            continue
        return text, {
            "hydrated_from_message_id": row["id"],
            "hydrated_from_user_id": row["user_id"],
            "hydrated_from_created_at": (
                row["created_at"].isoformat() if row["created_at"] is not None else None
            ),
        }
    return None, {}


async def _hydrate_recovery_turn(
    ctx: WorkflowContext,
    *,
    thread_key: str,
    parts: list[dict[str, Any]],
    user_id: str | None,
    message_id: str | None,
    metadata: dict[str, Any],
    history_messages: list[dict[str, Any]] | None = None,
) -> list[dict[str, Any]]:
    if not _is_recovery_turn(parts):
        return parts

    prior_ask, provenance = _lookup_last_unresolved_ask_from_history(
        history_messages or [],
        user_id=user_id,
        current_message_id=message_id,
    )
    if prior_ask is None:
        prior_ask, provenance = await _lookup_last_unresolved_ask(
            ctx,
            thread_key=thread_key,
            user_id=user_id,
            before_message_id=message_id,
        )
    if not prior_ask:
        return parts

    if isinstance(metadata, dict):
        metadata.setdefault("recovery_hydration", provenance)

    return [
        {"type": "text", "text": f"{_RECOVERY_CONTEXT_PREFIX}{prior_ask}"},
        *parts,
    ]


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    """Spawn → message → execute → wait for terminal result."""
    from api.workflow_engine import do_agent_turn

    thread_key = inp.thread_key.strip()
    if not thread_key:
        raise ControlPlaneError(
            "INVALID_WORKFLOW_INPUT",
            "slack_thread_turn requires thread_key",
            422,
        )

    selection = _extract_prompt_selection(
        inp.effective_parts,
        explicit_harness=inp.harness,
        explicit_persona=inp.persona,
    )
    selection_changed = bool(selection.harness or selection.persona)
    if selection_changed:
        await _release_for_prompt_switch(
            ctx,
            thread_key=thread_key,
            message_id=inp.message_id,
        )
    elif _is_recovery_turn(selection.parts):
        await _reset_session_for_recovery(
            ctx,
            thread_key=thread_key,
            message_id=inp.message_id,
        )

    parts = await _hydrate_recovery_turn(
        ctx,
        thread_key=thread_key,
        parts=selection.parts,
        user_id=inp.user_id,
        message_id=inp.message_id,
        metadata=inp.metadata,
        history_messages=inp.history_messages,
    )
    parts = _with_prompt_switch_context_note(
        parts,
        switched=selection_changed,
        history_messages=inp.history_messages,
    )
    history_messages = (
        inp.history_messages
        if await _should_backfill_history(
            ctx,
            thread_key=thread_key,
            switched=selection_changed,
            history_messages=inp.history_messages,
        )
        else []
    )

    return await do_agent_turn(
        ctx,
        thread_key=thread_key,
        parts=parts,
        history_messages=history_messages,
        message_id=inp.message_id,
        user_id=inp.user_id,
        metadata=inp.metadata,
        delivery=inp.delivery,
        harness=selection.harness,
        persona=selection.persona,
        agents_md_override=inp.agents_md_override,
    )
