#!/usr/bin/env python3
"""hermes-app-wrapper — Centaur NDJSON bridge for the Hermes Agent.

Hermes Agent (NousResearch/hermes-agent) is integrated by driving its native
``AIAgent`` Python API *in process* — the same class the interactive CLI and
gateway use — with streaming callbacks wired to Centaur's event stream. We
deliberately do NOT use Hermes' Agent Client Protocol (ACP) adapter: ACP is
governed by an external committee and is an unstable seam to depend on. We also
do not use the oneshot CLI (``hermes chat -q``), which nulls the streaming
callbacks and only returns a final string. Driving ``AIAgent`` directly gives
1:1 parity with the codex/claude/amp wrappers (streamed text, reasoning, tool
start/complete, plan, interrupt, multi-turn) while coupling only to the
``hermes-agent`` package we already install + version-pin.

The stdin/stdout contract matches the other sandbox wrappers:

* Read NDJSON turn envelopes from stdin:
  ``{"type":"user","message":{"content":[blocks]}, ...}`` and
  ``{"type":"interrupt"}``.
* Emit Hermes-native NDJSON events on stdout. The ``"hermes"`` engine branch in
  ``api/sandbox/normalize.py`` + ``harness_protocol.py`` maps these to canonical
  Centaur events:
    - ``{"type":"system","subtype":"init","session_id":...}``  (thread id)
    - ``{"type":"agent_message_chunk","text":...}``            (streamed answer)
    - ``{"type":"agent_thought_chunk","text":...}``            (reasoning)
    - ``{"type":"tool_call","tool_call_id":...,"name":...,"input":...}``
    - ``{"type":"tool_call_update","tool_call_id":...,"status":...,"output":...}``
    - ``{"type":"plan","entries":[...]}``                      (todo/plan panel)
    - ``{"type":"turn.completed","stop_reason":...,"text":...}``
    - ``{"type":"error","message":...}``

Provider/model are resolved from ``~/.hermes/config.yaml`` (written by the
sandbox entrypoint from ``HERMES_PROVIDER``/``HERMES_MODEL``), defaulting to the
Anthropic provider + a Claude model so Hermes flows through iron-proxy using the
stubbed ``ANTHROPIC_API_KEY`` like every other harness.

Hermes runs in process, so its stray stdout would corrupt the NDJSON stream. We
dup the real stdout to a private "protocol" channel for events and redirect the
process's fd 1 to stderr, so any ``print`` from Hermes lands in pod logs.
"""

from __future__ import annotations

import json
import os
import queue
import signal
import sys
import threading
import uuid
from collections import defaultdict, deque
from typing import Any

# ── protocol channel ─────────────────────────────────────────────────────────
# ``_OUT`` is the stream Centaur reads NDJSON from. Until the runtime installs
# the dedicated channel (in ``main``), emit falls back to ``sys.stdout`` so the
# module's helpers stay importable + unit-testable without fd surgery.
_OUT: Any = None
_EMIT_LOCK = threading.Lock()


def emit(payload: dict[str, Any]) -> None:
    out = _OUT if _OUT is not None else sys.stdout
    line = json.dumps(payload, separators=(",", ":"), ensure_ascii=False)
    with _EMIT_LOCK:
        out.write(line + "\n")
        out.flush()


def _install_protocol_channel() -> None:
    """Reserve real stdout for NDJSON; route everything else to stderr.

    Hermes runs in this process and may print directly to fd 1. Dup fd 1 to a
    private channel for events, then point fd 1 at stderr so stray output can't
    corrupt the protocol stream.
    """
    global _OUT
    protocol_fd = os.dup(1)
    os.dup2(2, 1)
    _OUT = os.fdopen(protocol_fd, "w", encoding="utf-8", closefd=True)


# ── content helpers ──────────────────────────────────────────────────────────


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


def _stringify(value: Any) -> str:
    if isinstance(value, str):
        return value
    if value is None:
        return ""
    try:
        return json.dumps(value, ensure_ascii=False, default=str)
    except (TypeError, ValueError):
        return str(value)


_TODO_STATUS_MAP = {
    "pending": "pending",
    "in_progress": "in_progress",
    "completed": "completed",
    # Centaur plans only render pending/in_progress/completed; keep cancelled
    # tasks visible as terminal entries instead of dropping them.
    "cancelled": "completed",
}


def _plan_entries_from_todo(result: Any) -> list[dict] | None:
    """Translate Hermes' ``todo`` tool result into Centaur plan entries.

    The result is JSON shaped like ``{"todos": [{"content","status",...}]}``,
    sometimes with a human hint appended after the JSON object.
    """
    if not isinstance(result, str) or not result.strip():
        return None
    text = result.strip()
    try:
        data = json.loads(text)
    except (json.JSONDecodeError, ValueError):
        try:
            data, _ = json.JSONDecoder().raw_decode(text)
        except (json.JSONDecodeError, ValueError):
            return None
    if not isinstance(data, dict) or not isinstance(data.get("todos"), list):
        return None
    entries: list[dict] = []
    for item in data["todos"]:
        if not isinstance(item, dict):
            continue
        content = str(item.get("content") or item.get("id") or "").strip()
        if not content:
            continue
        raw_status = str(item.get("status") or "pending").strip()
        status = _TODO_STATUS_MAP.get(raw_status, "pending")
        if raw_status == "cancelled":
            content = f"[cancelled] {content}"
        entries.append({"content": content, "priority": "medium", "status": status})
    return entries


# ── AIAgent streaming callbacks → Centaur NDJSON ─────────────────────────────


class TurnEmitter:
    """Translates ``AIAgent`` streaming callbacks into Centaur NDJSON events.

    Tool starts and completions are correlated via a FIFO of generated ids per
    tool name (Hermes' callbacks don't carry a shared id), matching how the ACP
    adapter tracked parallel/duplicate same-name calls.
    """

    def __init__(self) -> None:
        self._tool_ids: dict[str, deque[str]] = defaultdict(deque)
        self.final_text_parts: list[str] = []

    def reset(self) -> None:
        self.final_text_parts = []
        self._tool_ids.clear()

    @property
    def final_text(self) -> str:
        return "".join(self.final_text_parts)

    # AIAgent.stream_delta_callback(text)
    def on_stream_delta(self, text: str) -> None:
        if not text:
            return
        self.final_text_parts.append(text)
        emit({"type": "agent_message_chunk", "text": text})

    # AIAgent.reasoning_callback(text)
    def on_reasoning(self, text: str) -> None:
        if text:
            emit({"type": "agent_thought_chunk", "text": text})

    # AIAgent.tool_progress_callback(event_type, name, preview, args, **kwargs)
    def on_tool_progress(
        self,
        event_type: str,
        name: str | None = None,
        preview: str | None = None,
        args: Any = None,
        **_kwargs: Any,
    ) -> None:
        if event_type != "tool.started":
            return
        if isinstance(args, str):
            try:
                args = json.loads(args)
            except (json.JSONDecodeError, TypeError):
                args = {"raw": args}
        if not isinstance(args, dict):
            args = {}
        tool_name = name or "tool"
        tc_id = "tool-" + uuid.uuid4().hex[:12]
        self._tool_ids[tool_name].append(tc_id)
        emit(
            {
                "type": "tool_call",
                "tool_call_id": tc_id,
                "name": tool_name,
                "input": args,
            }
        )

    # AIAgent.step_callback(api_call_count, prev_tools)
    def on_step(self, api_call_count: int, prev_tools: Any = None) -> None:
        if not isinstance(prev_tools, list):
            return
        for info in prev_tools:
            if isinstance(info, dict):
                name = info.get("name") or info.get("function_name") or "tool"
                result = info.get("result")
                if result is None:
                    result = info.get("output")
                is_error = bool(info.get("error") or info.get("is_error"))
            elif isinstance(info, str):
                name, result, is_error = info, None, False
            else:
                continue
            queue_for_name = self._tool_ids.get(name)
            tc_id = (
                queue_for_name.popleft()
                if queue_for_name
                else "tool-" + uuid.uuid4().hex[:12]
            )
            if queue_for_name is not None and not queue_for_name:
                self._tool_ids.pop(name, None)
            emit(
                {
                    "type": "tool_call_update",
                    "tool_call_id": tc_id,
                    "status": "failed" if is_error else "completed",
                    "output": _stringify(result),
                    "is_error": is_error,
                }
            )
            if name == "todo":
                entries = _plan_entries_from_todo(result)
                if entries is not None:
                    emit({"type": "plan", "entries": entries})


# ── agent construction (mirrors hermes_cli.oneshot._run_agent) ───────────────


def _clarify_callback(question: str, choices: Any = None) -> str:
    """Non-interactive clarify: instruct the agent to pick a sensible default.

    There is no user at a terminal in the sandbox, so a clarify prompt would
    otherwise stall the turn.
    """
    return (
        "No interactive user is available. Choose the most reasonable default "
        "and proceed without asking for clarification."
    )


def _build_agent(emitter: TurnEmitter) -> Any:
    """Construct an ``AIAgent`` like a normal CLI chat turn, with callbacks wired."""
    from hermes_cli.config import load_config
    from hermes_cli.runtime_provider import resolve_runtime_provider
    from hermes_cli.tools_config import _get_platform_tools
    from run_agent import AIAgent

    try:
        from hermes_state import SessionDB

        session_db = SessionDB()
    except Exception:
        session_db = None

    cfg = load_config()
    model_cfg = cfg.get("model") or {}
    if isinstance(model_cfg, str):
        cfg_model = model_cfg
    else:
        cfg_model = model_cfg.get("default") or model_cfg.get("model") or ""

    effective_model = (
        (os.getenv("HERMES_MODEL") or "").strip()
        or os.getenv("HERMES_INFERENCE_MODEL", "").strip()
        or cfg_model
    )
    requested_provider = (os.getenv("HERMES_PROVIDER") or "").strip() or None
    runtime = resolve_runtime_provider(
        requested=requested_provider, target_model=effective_model or None
    )

    try:
        toolsets = sorted(_get_platform_tools(cfg, "cli"))
    except Exception:
        toolsets = None

    resume = (
        os.getenv("HERMES_CONTINUE_SESSION_ID")
        or os.getenv("AMP_CONTINUE_THREAD_ID")
        or ""
    ).strip() or None

    agent = AIAgent(
        api_key=runtime.get("api_key"),
        base_url=runtime.get("base_url"),
        provider=runtime.get("provider"),
        api_mode=runtime.get("api_mode"),
        model=effective_model,
        enabled_toolsets=toolsets,
        quiet_mode=True,
        platform="cli",
        session_db=session_db,
        credential_pool=runtime.get("credential_pool"),
        session_id=resume,
        clarify_callback=_clarify_callback,
    )
    agent.suppress_status_output = True
    # Local "kawaii" status updates are noise; route real provider reasoning
    # deltas to the thought stream instead.
    agent.thinking_callback = None
    agent.reasoning_callback = emitter.on_reasoning
    agent.stream_delta_callback = emitter.on_stream_delta
    agent.tool_progress_callback = emitter.on_tool_progress
    agent.step_callback = emitter.on_step
    return agent


# ── main driver ──────────────────────────────────────────────────────────────


class Wrapper:
    def __init__(self) -> None:
        self.emitter = TurnEmitter()
        self.agent: Any = None
        self.inputs: queue.Queue[dict[str, Any] | None] = queue.Queue()
        self._interrupted = False
        self._pending_interrupt = False

    def _stdin_reader(self) -> None:
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
            self.inputs.put(msg)
        self.inputs.put(None)

    def request_interrupt(self, *_args: Any) -> None:
        self._interrupted = True
        self._pending_interrupt = True
        if self.agent is None:
            return
        try:
            self.agent.interrupt()
        except Exception as exc:
            emit({"type": "error", "message": f"interrupt failed: {exc}"})

    def _run_turn(self, turn_input: dict[str, Any]) -> None:
        self.emitter.reset()
        self._interrupted = self._pending_interrupt
        self._pending_interrupt = False
        prompt = _prompt_text(turn_input)

        # Run the (blocking) turn on a worker thread so the stdin reader can
        # deliver an interrupt to AIAgent.interrupt() from another thread.
        result: dict[str, Any] = {}

        def _go() -> None:
            try:
                result["text"] = self.agent.chat(prompt)
            except Exception as exc:  # pragma: no cover - provider/tool errors
                result["error"] = str(exc)

        worker = threading.Thread(target=_go)
        worker.start()
        worker.join()

        if "error" in result:
            if self._interrupted:
                emit({"type": "system", "subtype": "turn_interrupted"})
                return
            emit({"type": "error", "message": result["error"]})
            emit({"type": "turn.failed", "error": {"message": result["error"]}})
            return
        text = result.get("text") or self.emitter.final_text
        if not str(text or "").strip():
            if self._interrupted:
                emit({"type": "system", "subtype": "turn_interrupted"})
                return
            message = "hermes returned no assistant output"
            emit({"type": "error", "message": message})
            emit({"type": "turn.failed", "error": {"message": message}})
            return
        emit(
            {
                "type": "turn.completed",
                "stop_reason": "interrupted" if self._interrupted else "end_turn",
                "text": text,
            }
        )

    def run(self) -> None:
        _install_protocol_channel()
        # Non-interactive: auto-approve shell/tool/hook gates (the sandbox is the
        # trust boundary, matching claude's --dangerously-skip-permissions).
        os.environ.setdefault("HERMES_YOLO_MODE", "1")
        os.environ.setdefault("HERMES_ACCEPT_HOOKS", "1")

        emit({"type": "system", "subtype": "wrapper_heartbeat", "phase": "startup"})
        try:
            self.agent = _build_agent(self.emitter)
        except Exception as exc:
            emit({"type": "error", "message": f"failed to start hermes: {exc}"})
            emit({"type": "turn.failed", "error": {"message": str(exc)}})
            return

        session_id = getattr(self.agent, "session_id", None) or ""
        if session_id:
            emit({"type": "system", "subtype": "init", "session_id": session_id})
        emit(
            {
                "type": "system",
                "subtype": "wrapper_heartbeat",
                "phase": "session_started",
            }
        )

        threading.Thread(target=self._stdin_reader, daemon=True).start()

        while True:
            item = self.inputs.get()
            if item is None:
                break
            if item.get("type") != "user":
                continue
            try:
                self._run_turn(item)
            except Exception as exc:  # pragma: no cover - defensive
                emit({"type": "error", "message": str(exc)})


def main() -> None:
    wrapper = Wrapper()
    signal.signal(signal.SIGUSR1, wrapper.request_interrupt)
    try:
        wrapper.run()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
