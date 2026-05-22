#!/usr/bin/env python3
"""hermes-app-wrapper — Centaur NDJSON bridge for the Hermes Agent ACP server.

Hermes Agent (NousResearch/hermes-agent) speaks the Agent Client Protocol over
stdio via ``hermes acp`` (entry point ``hermes-acp``). This wrapper acts as the
ACP *client*: it keeps a single Hermes ACP session alive, translates each
Centaur turn into an ACP ``session/prompt``, auto-approves tool permission
requests, and re-emits Hermes' streaming ``session/update`` notifications as
Centaur-shaped NDJSON for the API to normalize.

The stdin/stdout contract matches the other sandbox wrappers (codex/claude/amp):

* Read NDJSON turn envelopes from stdin:
  ``{"type":"user","message":{"content":[blocks]}, "steer"?, "trace_id"?}`` and
  ``{"type":"interrupt"}``.
* Emit Hermes-native NDJSON events on stdout. The matching ``"hermes"`` engine
  branch in ``api/sandbox/normalize.py`` + ``harness_protocol.py`` maps these to
  canonical Centaur events:
    - ``{"type":"system","subtype":"init","session_id":...}``  (thread id)
    - ``{"type":"agent_message_chunk","text":...}``            (streamed answer)
    - ``{"type":"agent_thought_chunk","text":...}``            (reasoning)
    - ``{"type":"tool_call","tool_call_id":...,"name":...,"input":...}``
    - ``{"type":"tool_call_update","tool_call_id":...,"status":...,"output":...}``
    - ``{"type":"plan","entries":[...]}``                      (todo/plan panel)
    - ``{"type":"turn.completed","stop_reason":...,"text":...,"usage":...}``
    - ``{"type":"error","message":...}``

Model/provider selection lives in ``~/.hermes/config.yaml`` (written by the
sandbox entrypoint from ``HERMES_PROVIDER``/``HERMES_MODEL``), so this wrapper
stays transport-only. Hermes inherits the firewall proxy + stubbed
``ANTHROPIC_API_KEY`` from the container env, so its model traffic flows through
iron-proxy like every other harness.
"""

from __future__ import annotations

import asyncio
import json
import os
import signal
import sys
import threading
from typing import Any

# NOTE: ``acp`` (agent-client-protocol) is only present in the sandbox image, so
# it is imported lazily inside the functions that need it. This keeps the module
# importable — and its pure helpers unit-testable — in environments without it.

# ── stdout (Centaur NDJSON) ──────────────────────────────────────────────────
# ACP talks to the Hermes subprocess over its own pipes, so our stdout is free
# for Centaur events. A lock keeps concurrent emits (loop callbacks + main)
# from interleaving partial lines.
_EMIT_LOCK = threading.Lock()


def emit(payload: dict[str, Any]) -> None:
    line = json.dumps(payload, separators=(",", ":"), ensure_ascii=False)
    with _EMIT_LOCK:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


def _hermes_acp_cmd() -> list[str]:
    """Resolve the Hermes ACP server command.

    Prefer the installed ``hermes-acp`` console script; fall back to the module
    entry point so the wrapper still works in editable/dev installs.
    """
    override = (os.environ.get("HERMES_ACP_COMMAND") or "").strip()
    if override:
        return override.split()
    from shutil import which

    if which("hermes-acp"):
        return ["hermes-acp"]
    if which("hermes"):
        return ["hermes", "acp"]
    return [sys.executable, "-m", "acp_adapter.entry"]


# ── content helpers ──────────────────────────────────────────────────────────

def _content_block_text(content: Any) -> str:
    """Extract text from an ACP content block (dict) or list of blocks."""
    if isinstance(content, list):
        return "".join(_content_block_text(c) for c in content)
    if isinstance(content, dict):
        if content.get("type") == "text":
            return str(content.get("text") or "")
        # tool-call ``content`` wrapper: {"type":"content","content":{...}}
        inner = content.get("content")
        if isinstance(inner, (dict, list)):
            return _content_block_text(inner)
    return ""


def _tool_output_text(update: dict[str, Any]) -> str:
    """Flatten a tool_call_update's content / rawOutput into a string."""
    text = _content_block_text(update.get("content"))
    if text:
        return text
    raw = update.get("rawOutput")
    if isinstance(raw, str):
        return raw
    if raw is not None:
        return json.dumps(raw, ensure_ascii=False)
    return ""


def _prompt_text(turn_input: dict[str, Any]) -> str:
    """Flatten a Centaur turn envelope's content blocks into prompt text."""
    message = turn_input.get("message")
    blocks = message.get("content") if isinstance(message, dict) else None
    parts: list[str] = []
    for block in blocks or []:
        if not isinstance(block, dict):
            continue
        btype = block.get("type")
        if btype == "text":
            parts.append(str(block.get("text") or ""))
        elif btype == "image":
            parts.append(
                "[User sent an image attachment; ask them to upload it as a file "
                "reference if you need it.]"
            )
        else:
            parts.append(json.dumps(block, ensure_ascii=False))
    return "\n".join(p for p in parts if p).strip() or "continue"


def _prompt_blocks(turn_input: dict[str, Any]) -> list[Any]:
    """Build ACP prompt content blocks from a Centaur turn envelope."""
    import acp

    return [acp.text_block(_prompt_text(turn_input))]


# ── ACP client implementation ────────────────────────────────────────────────

class CentaurHermesClient:
    """ACP client that re-emits Hermes session updates as Centaur NDJSON."""

    def __init__(self) -> None:
        # Accumulated agent message text for the in-flight turn, so the API's
        # extract_result() gets the full final answer (streamed deltas only
        # carry fragments).
        self.final_text_parts: list[str] = []

    def reset_turn(self) -> None:
        self.final_text_parts = []

    @property
    def final_text(self) -> str:
        return "".join(self.final_text_parts)

    async def session_update(self, session_id: str, update: Any, **_kwargs: Any) -> None:
        try:
            data = update.model_dump(by_alias=True, exclude_none=True)
        except AttributeError:
            data = update if isinstance(update, dict) else {}
        kind = data.get("sessionUpdate")

        if kind == "agent_message_chunk":
            text = _content_block_text(data.get("content"))
            if text:
                self.final_text_parts.append(text)
                emit({"type": "agent_message_chunk", "text": text})
        elif kind == "agent_thought_chunk":
            text = _content_block_text(data.get("content"))
            if text:
                emit({"type": "agent_thought_chunk", "text": text})
        elif kind == "tool_call":
            emit(
                {
                    "type": "tool_call",
                    "tool_call_id": data.get("toolCallId") or "",
                    "name": data.get("title") or data.get("kind") or "tool",
                    "kind": data.get("kind"),
                    "input": data.get("rawInput") or {},
                }
            )
        elif kind == "tool_call_update":
            status = data.get("status")
            # Only the terminal update carries the result Centaur renders.
            if status in ("completed", "failed"):
                emit(
                    {
                        "type": "tool_call_update",
                        "tool_call_id": data.get("toolCallId") or "",
                        "status": status,
                        "output": _tool_output_text(data),
                        "is_error": status == "failed",
                    }
                )
        elif kind == "plan":
            emit({"type": "plan", "entries": data.get("entries") or []})
        # usage_update / available_commands_update / mode updates carry no
        # Centaur-facing payload; usage is surfaced via turn.completed instead.

    async def request_permission(
        self, options: Any, session_id: str, tool_call: Any, **_kwargs: Any
    ) -> Any:
        """Auto-approve tool use (the sandbox is the trust boundary)."""
        from acp.schema import (
            AllowedOutcome,
            DeniedOutcome,
            RequestPermissionResponse,
        )

        chosen = _pick_allow_option(options)
        if chosen is not None:
            return RequestPermissionResponse(
                outcome=AllowedOutcome(outcome="selected", option_id=chosen)
            )
        return RequestPermissionResponse(outcome=DeniedOutcome(outcome="cancelled"))

    # Minimal no-op extension hooks so the connection never errors on them.
    async def ext_method(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        return {}

    async def ext_notification(self, method: str, params: dict[str, Any]) -> None:
        return None

    def on_connect(self, conn: Any) -> None:
        return None


def _pick_allow_option(options: Any) -> str | None:
    """Pick an 'allow' permission option id, preferring allow_always."""
    fallback: str | None = None
    for opt in options or []:
        kind = getattr(opt, "kind", None)
        option_id = getattr(opt, "option_id", None) or getattr(opt, "optionId", None)
        if not option_id:
            continue
        if kind == "allow_always":
            return option_id
        if kind == "allow_once" and fallback is None:
            fallback = option_id
        if fallback is None and kind not in ("reject_once", "reject_always"):
            fallback = option_id
    return fallback


# ── session lifecycle ────────────────────────────────────────────────────────

async def _start_session(conn: acp.ClientSideConnection, cwd: str) -> str:
    """Create or resume a Hermes ACP session; return its id."""
    resume = (
        os.environ.get("HERMES_CONTINUE_SESSION_ID")
        or os.environ.get("AMP_CONTINUE_THREAD_ID")
        or ""
    ).strip()
    if resume:
        try:
            result = await conn.load_session(cwd=cwd, session_id=resume)
            if result is not None:
                return resume
        except Exception:
            # Fall through to a fresh session if resume isn't supported / valid.
            emit(
                {
                    "type": "system",
                    "subtype": "wrapper_heartbeat",
                    "phase": "resume_failed",
                }
            )
    result = await conn.new_session(cwd=cwd, mcp_servers=[])
    return getattr(result, "session_id", None) or resume or ""


# ── main driver ──────────────────────────────────────────────────────────────

class Wrapper:
    def __init__(self) -> None:
        self.loop: asyncio.AbstractEventLoop | None = None
        self.conn: acp.ClientSideConnection | None = None
        self.session_id: str = ""
        self.inputs: asyncio.Queue[dict[str, Any] | None] = asyncio.Queue()
        self.client = CentaurHermesClient()

    # stdin runs in a background thread; it can't touch the loop directly.
    def _stdin_reader(self) -> None:
        assert self.loop is not None
        for raw in sys.stdin:
            line = raw.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                emit({"type": "error", "message": "invalid stdin JSON"})
                continue
            if not isinstance(msg, dict):
                continue
            if msg.get("type") == "interrupt":
                self.request_interrupt()
                continue
            self.loop.call_soon_threadsafe(self.inputs.put_nowait, msg)
        self.loop.call_soon_threadsafe(self.inputs.put_nowait, None)

    def request_interrupt(self, *_args: Any) -> None:
        if self.loop is None or self.conn is None or not self.session_id:
            return
        self.loop.call_soon_threadsafe(
            lambda: asyncio.ensure_future(self._cancel())
        )

    async def _cancel(self) -> None:
        if self.conn is None or not self.session_id:
            return
        try:
            await self.conn.cancel(session_id=self.session_id)
        except Exception as exc:
            emit({"type": "error", "message": f"interrupt failed: {exc}"})

    async def _run_turn(self, turn_input: dict[str, Any]) -> None:
        assert self.conn is not None
        self.client.reset_turn()
        try:
            resp = await self.conn.prompt(
                prompt=_prompt_blocks(turn_input), session_id=self.session_id
            )
        except Exception as exc:
            emit({"type": "error", "message": str(exc)})
            return
        stop_reason = getattr(resp, "stop_reason", None) or "end_turn"
        usage = getattr(resp, "usage", None)
        payload: dict[str, Any] = {
            "type": "turn.completed",
            "stop_reason": stop_reason,
            "text": self.client.final_text,
        }
        if usage is not None:
            try:
                payload["usage"] = usage.model_dump(by_alias=True, exclude_none=True)
            except AttributeError:
                payload["usage"] = usage
        emit(payload)

    async def run(self) -> None:
        import acp
        from acp.schema import (
            ClientCapabilities,
            FileSystemCapabilities,
            Implementation,
        )

        self.loop = asyncio.get_running_loop()
        cwd = os.getcwd()
        env = dict(os.environ)
        cmd, *args = _hermes_acp_cmd()

        capabilities = ClientCapabilities(
            fs=FileSystemCapabilities(read_text_file=False, write_text_file=False),
            terminal=False,
        )
        client_info = Implementation(name="centaur", title="Centaur", version="0.1.0")

        emit({"type": "system", "subtype": "wrapper_heartbeat", "phase": "startup"})
        async with acp.spawn_agent_process(
            self.client, cmd, *args, env=env, cwd=cwd, use_unstable_protocol=True
        ) as (conn, _proc):
            self.conn = conn
            await conn.initialize(
                acp.PROTOCOL_VERSION,
                client_capabilities=capabilities,
                client_info=client_info,
            )
            self.session_id = await _start_session(conn, cwd)
            if self.session_id:
                emit({"type": "system", "subtype": "init", "session_id": self.session_id})
            emit(
                {
                    "type": "system",
                    "subtype": "wrapper_heartbeat",
                    "phase": "session_started",
                }
            )

            threading.Thread(target=self._stdin_reader, daemon=True).start()

            while True:
                item = await self.inputs.get()
                if item is None:
                    break
                if item.get("type") != "user":
                    continue
                try:
                    await self._run_turn(item)
                except Exception as exc:  # pragma: no cover - defensive
                    emit({"type": "error", "message": str(exc)})


def main() -> None:
    wrapper = Wrapper()

    def _stop(*_args: Any) -> None:
        # Best-effort: stop the loop so the spawn context manager tears the
        # Hermes subprocess down.
        if wrapper.loop is not None:
            wrapper.loop.call_soon_threadsafe(wrapper.loop.stop)

    signal.signal(signal.SIGTERM, _stop)
    signal.signal(signal.SIGINT, _stop)
    signal.signal(signal.SIGUSR1, wrapper.request_interrupt)

    try:
        asyncio.run(wrapper.run())
    except (KeyboardInterrupt, RuntimeError):
        pass


if __name__ == "__main__":
    main()
