#!/usr/bin/env python3
"""Centaur NDJSON bridge for the Claude Code CLI.

Spawns ``claude -p --input-format stream-json --output-format stream-json``
and pipes Centaur's Anthropic-shaped envelopes
(``{"type":"user","message":{...}}`` plus optional ``steer``/``trace_id``)
straight through. Layers three Centaur-specific behaviours on top:

* ``/goal X`` is intercepted (slash commands do not run in ``-p`` mode) and
  replayed as a synthetic user instruction, matching codex's
  ``thread/goal/set`` parity.
* SIGUSR1 and ``{"type":"interrupt"}`` cancel the active turn by SIGINT-ing
  the ``claude`` process group.
* ``AGENTS.md`` is appended as a system prompt so Claude receives the same
  Centaur context codex reads from the workspace.
"""

from __future__ import annotations

import json
import os
import queue
import re
import signal
import subprocess
import sys
import threading
from typing import Any

DEFAULT_CLAUDE_MODEL = "claude-opus-4-8"

APP: subprocess.Popen[str] | None = None
WRITE_LOCK = threading.Lock()
SHUTTING_DOWN = False
INPUTS: queue.Queue[dict[str, Any] | None] = queue.Queue()
PENDING_INPUTS: list[dict[str, Any] | None] = []
TURN_DONE = threading.Event()
TURN_DONE.set()

_GOAL_RE = re.compile(r"^\s*/goal\b\s*(.*)$", re.DOTALL)


def emit(payload: dict[str, Any]) -> None:
    sys.stdout.write(
        json.dumps(payload, separators=(",", ":"), ensure_ascii=False) + "\n"
    )
    sys.stdout.flush()


def send_to_claude(payload: dict[str, Any]) -> None:
    assert APP is not None and APP.stdin is not None
    with WRITE_LOCK:
        APP.stdin.write(
            json.dumps(payload, separators=(",", ":"), ensure_ascii=False) + "\n"
        )
        APP.stdin.flush()


def api_stdin_reader() -> None:
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        try:
            INPUTS.put(json.loads(line))
        except json.JSONDecodeError:
            emit({"type": "error", "message": "invalid stdin JSON"})
    INPUTS.put(None)


def claude_stdout_reader() -> None:
    assert APP is not None and APP.stdout is not None
    for raw in APP.stdout:
        line = raw.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            emit({"type": "system", "subtype": "raw_stdout", "line": line[:1000]})
            continue
        emit(event)
        if event.get("type") in ("result", "error"):
            TURN_DONE.set()
    TURN_DONE.set()


def _user_envelope(blocks: list[Any]) -> dict[str, Any]:
    return {"type": "user", "message": {"role": "user", "content": blocks}}


def _rewrite_goal(blocks: list[Any]) -> list[Any]:
    """Rewrite a single ``/goal X`` text block into an English instruction.

    Slash commands do not run in ``-p`` mode, so we translate ``/goal`` into
    plain prose. Claude's response becomes the turn result, matching codex's
    ``thread/goal/set`` UX without burning extra events.
    """
    if len(blocks) != 1:
        return blocks
    block = blocks[0]
    if not isinstance(block, dict) or block.get("type") != "text":
        return blocks
    text = block.get("text")
    if not isinstance(text, str):
        return blocks
    match = _GOAL_RE.match(text)
    if not match:
        return blocks
    goal = match.group(1).strip()
    if not goal:
        return blocks
    return [
        {
            "type": "text",
            "text": (
                f"Set this thread's working goal: {goal}\n\n"
                "Acknowledge briefly, then keep this goal in mind for "
                "subsequent turns. Do not run tools until the user follows up."
            ),
        }
    ]


def interrupt_active_turn(*_args: object) -> None:
    active = not TURN_DONE.is_set()
    if APP is None or APP.poll() is not None:
        return
    try:
        os.killpg(os.getpgid(APP.pid), signal.SIGINT)
    except (ProcessLookupError, PermissionError) as exc:
        emit({"type": "error", "message": f"interrupt failed: {exc}"})
    if active:
        emit(
            {
                "type": "result",
                "subtype": "error",
                "is_error": True,
                "result": "interrupted",
                "stop_reason": "interrupted",
                "terminal_reason": "interrupted",
            }
        )
        TURN_DONE.set()


def next_input() -> dict[str, Any] | None:
    if PENDING_INPUTS:
        return PENDING_INPUTS.pop(0)
    return INPUTS.get()


def wait_for_turn_done() -> None:
    while not TURN_DONE.wait(timeout=0.1):
        try:
            item = INPUTS.get_nowait()
        except queue.Empty:
            continue
        if isinstance(item, dict) and item.get("type") == "interrupt":
            interrupt_active_turn()
        else:
            PENDING_INPUTS.append(item)


def handle_input(turn_input: dict[str, Any]) -> None:
    if turn_input.get("type") == "interrupt":
        interrupt_active_turn()
        return
    if turn_input.get("type") != "user":
        return

    message = turn_input.get("message")
    if not isinstance(message, dict):
        return
    blocks = message.get("content")
    if not isinstance(blocks, list) or not blocks:
        return

    TURN_DONE.clear()
    send_to_claude(_user_envelope(_rewrite_goal(blocks)))
    wait_for_turn_done()


def _resume_session_id() -> str:
    return (
        os.environ.get("CLAUDE_CONTINUE_SESSION_ID")
        or os.environ.get("AMP_CONTINUE_THREAD_ID")
        or ""
    ).strip()


def _build_claude_cmd() -> list[str]:
    cmd: list[str] = [
        "claude",
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--dangerously-skip-permissions",
        "--permission-mode",
        "bypassPermissions",
    ]
    if os.path.isfile("AGENTS.md"):
        cmd.extend(["--append-system-prompt-file", "AGENTS.md"])
    model = (os.environ.get("CLAUDE_MODEL") or DEFAULT_CLAUDE_MODEL).strip()
    model = model or DEFAULT_CLAUDE_MODEL
    cmd.extend(["--model", model])
    resume = _resume_session_id()
    if resume:
        cmd.extend(["--resume", resume])
    return cmd


def exit_wrapper(*_args: object) -> None:
    global SHUTTING_DOWN
    SHUTTING_DOWN = True
    if APP and APP.poll() is None:
        try:
            os.killpg(os.getpgid(APP.pid), signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            pass


def main() -> None:
    global APP
    signal.signal(signal.SIGTERM, exit_wrapper)
    signal.signal(signal.SIGINT, exit_wrapper)
    signal.signal(signal.SIGUSR1, interrupt_active_turn)

    APP = subprocess.Popen(
        _build_claude_cmd(),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=sys.stderr,
        text=True,
        bufsize=1,
        cwd=os.getcwd(),
        start_new_session=True,
    )
    threading.Thread(target=claude_stdout_reader, daemon=True).start()
    threading.Thread(target=api_stdin_reader, daemon=True).start()

    emit({"type": "system", "subtype": "wrapper_heartbeat", "phase": "startup"})

    while not SHUTTING_DOWN:
        item = next_input()
        if item is None:
            break
        try:
            handle_input(item)
        except Exception as exc:
            emit({"type": "error", "message": str(exc)})
            emit(
                {
                    "type": "result",
                    "subtype": "error",
                    "result": f"wrapper error: {exc}",
                    "is_error": True,
                }
            )

    exit_wrapper()
    if APP:
        try:
            APP.wait(timeout=10)
        except subprocess.TimeoutExpired:
            APP.kill()


if __name__ == "__main__":
    main()
