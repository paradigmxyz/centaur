#!/usr/bin/env python3
"""codex-app-wrapper — Centaur NDJSON bridge for `codex app-server`.

The API speaks a small Anthropic-shaped stdin protocol. This adapter keeps a
single Codex app-server process alive, translates each user turn into JSON-RPC
`turn/start` (or `turn/steer` while a turn is active), opts into experimental
APIs for thread goals, and emits Codex-shaped NDJSON events for Centaur.
"""

from __future__ import annotations

import json
import os
import queue
import signal
import subprocess
import sys
import threading
import time
import uuid
from typing import Any

APP: subprocess.Popen[str] | None = None
WRITE_LOCK = threading.Lock()
NEXT_ID = 1
RESPONSES: dict[int, queue.Queue[dict[str, Any]]] = {}
EVENTS: queue.Queue[dict[str, Any] | None] = queue.Queue()
INPUTS: queue.Queue[dict[str, Any] | None] = queue.Queue()
THREAD_ID: str | None = None
ACTIVE_TURN_ID: str | None = None
SHUTTING_DOWN = False
CONFIGURED_OTEL_TRACE_ID: str | None = None
APP_INITIALIZED = False
CURRENT_TRACEPARENT: str | None = None


def emit(payload: dict[str, Any]) -> None:
    sys.stdout.write(
        json.dumps(payload, separators=(",", ":"), ensure_ascii=False) + "\n"
    )
    sys.stdout.flush()


def _next_id() -> int:
    global NEXT_ID
    with WRITE_LOCK:
        value = NEXT_ID
        NEXT_ID += 1
    return value


def send_raw(payload: dict[str, Any]) -> None:
    assert APP is not None and APP.stdin is not None
    with WRITE_LOCK:
        APP.stdin.write(
            json.dumps(payload, separators=(",", ":"), ensure_ascii=False) + "\n"
        )
        APP.stdin.flush()


def request(
    method: str, params: dict[str, Any] | None = None, timeout: float = 30.0
) -> dict[str, Any]:
    msg_id = _next_id()
    q: queue.Queue[dict[str, Any]] = queue.Queue(maxsize=1)
    RESPONSES[msg_id] = q
    payload: dict[str, Any] = {"id": msg_id, "method": method, "params": params or {}}
    if CURRENT_TRACEPARENT:
        payload["trace"] = {"traceparent": CURRENT_TRACEPARENT}
    send_raw(payload)
    try:
        response = q.get(timeout=timeout)
    finally:
        RESPONSES.pop(msg_id, None)
    if "error" in response:
        raise RuntimeError(response["error"].get("message") or str(response["error"]))
    return response.get("result") or {}


def notify(method: str, params: dict[str, Any] | None = None) -> None:
    send_raw({"method": method, "params": params or {}})


def start_app_server() -> None:
    global APP, APP_INITIALIZED
    if APP is not None and APP.poll() is None and APP_INITIALIZED:
        return
    if APP is not None and APP.poll() is not None:
        APP = None
        APP_INITIALIZED = False

    APP = subprocess.Popen(
        [
            "codex",
            "app-server",
            "--listen",
            "stdio://",
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=sys.stderr,
        text=True,
        bufsize=1,
        cwd=os.getcwd(),
    )
    threading.Thread(target=app_stdout_reader, daemon=True).start()
    request(
        "initialize",
        {
            "clientInfo": {"name": "centaur", "title": "Centaur", "version": "0.1.0"},
            "capabilities": {"experimentalApi": True},
        },
        timeout=30,
    )
    notify("initialized")
    APP_INITIALIZED = True
    emit({"type": "system", "subtype": "wrapper_heartbeat", "phase": "app_server_started"})


def app_stdout_reader() -> None:
    assert APP is not None and APP.stdout is not None
    for raw in APP.stdout:
        line = raw.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if "id" in msg:
            q = RESPONSES.get(msg["id"])
            if q:
                q.put(msg)
        elif "method" in msg:
            EVENTS.put(msg)
    EVENTS.put(None)


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


def text_from_blocks(blocks: list[dict[str, Any]]) -> str:
    parts: list[str] = []
    for block in blocks:
        btype = block.get("type")
        if btype == "text":
            parts.append(str(block.get("text") or ""))
        elif btype == "image":
            parts.append(
                "[User sent an image attachment; if needed, ask them to upload it as a file reference.]"
            )
        else:
            parts.append(json.dumps(block, ensure_ascii=False))
    return "\n".join(p for p in parts if p).strip()


def input_items(turn_input: dict[str, Any]) -> list[dict[str, Any]]:
    blocks = turn_input.get("message", {}).get("content") or []
    if not isinstance(blocks, list):
        blocks = []
    text = text_from_blocks(blocks)
    return [{"type": "text", "text": text or "continue"}]


def split_goal(items: list[dict[str, Any]]) -> tuple[str | None, list[dict[str, Any]]]:
    if len(items) != 1 or items[0].get("type") != "text":
        return None, items
    text = str(items[0].get("text") or "").strip()
    if not text.startswith("/goal"):
        return None, items
    goal = text[len("/goal") :].strip()
    return goal or None, []


def _trace_id_to_w3c_hex(trace_id: str | None) -> str | None:
    raw = (trace_id or "").strip()
    if not raw:
        return None
    try:
        return uuid.UUID(raw).hex
    except ValueError:
        compact = raw.replace("-", "").lower()
        if len(compact) == 32 and all(char in "0123456789abcdef" for char in compact):
            return compact
    return None


def _new_parent_span_id() -> str:
    span_id = uuid.uuid4().hex[:16]
    return span_id if span_id != "0" * 16 else "1".rjust(16, "0")


def _traceparent_from_trace_id(trace_id: str | None) -> str | None:
    trace_hex = _trace_id_to_w3c_hex(trace_id)
    if not trace_hex or trace_hex == "0" * 32:
        return None
    return f"00-{trace_hex}-{_new_parent_span_id()}-01"


def configure_trace_context(trace_id: str | None) -> None:
    global CURRENT_TRACEPARENT
    traceparent = _traceparent_from_trace_id(trace_id)
    if not traceparent:
        return
    CURRENT_TRACEPARENT = traceparent
    os.environ["TRACEPARENT"] = traceparent


def configure_trace_context_for_startup(trace_id: str | None) -> None:
    global CONFIGURED_OTEL_TRACE_ID
    trace_id = (trace_id or os.environ.get("CENTAUR_TRACE_ID") or "").strip()
    configure_trace_context(trace_id)
    if not trace_id or CONFIGURED_OTEL_TRACE_ID == trace_id:
        return
    CONFIGURED_OTEL_TRACE_ID = trace_id


def start_or_resume_thread() -> str:
    global THREAD_ID
    if THREAD_ID:
        return THREAD_ID
    resume = (
        os.environ.get("CODEX_CONTINUE_THREAD_ID")
        or os.environ.get("AMP_CONTINUE_THREAD_ID")
        or ""
    ).strip()
    if resume:
        result = request(
            "thread/resume", {"threadId": resume, "cwd": os.getcwd()}, timeout=60
        )
    else:
        result = request("thread/start", {"cwd": os.getcwd()}, timeout=60)
    thread = result.get("thread") or {}
    THREAD_ID = str(thread.get("id") or resume or uuid.uuid4())
    emit({"type": "thread.started", "thread_id": THREAD_ID})
    return THREAD_ID


def emit_notification(msg: dict[str, Any]) -> bool:
    global THREAD_ID, ACTIVE_TURN_ID
    method = str(msg.get("method") or "")
    params = msg.get("params") if isinstance(msg.get("params"), dict) else {}

    if method == "thread/started":
        thread = params.get("thread") or {}
        tid = thread.get("id") or params.get("threadId")
        if tid:
            THREAD_ID = str(tid)
            emit({"type": "thread.started", "thread_id": THREAD_ID})
        return False

    if method == "turn/started":
        turn = params.get("turn") or {}
        ACTIVE_TURN_ID = (
            str(turn.get("id") or params.get("turnId") or "") or ACTIVE_TURN_ID
        )
        emit({"type": "turn.started", "turn_id": ACTIVE_TURN_ID or ""})
        return False

    if method in {"item/started", "item/updated", "item/completed"}:
        emit({"type": method.replace("/", "."), "item": params.get("item") or params})
        return False

    if method in {
        "item/commandExecution/outputDelta",
        "item/fileChange/outputDelta",
        "item/plan/delta",
        "item/reasoning/summaryTextDelta",
        "item/reasoning/summaryPartAdded",
        "item/reasoning/textDelta",
    }:
        emit({"type": method.replace("/", "."), **params})
        return False

    if method == "turn/plan/updated":
        emit({"type": method.replace("/", "."), **params})
        return False

    if method == "item/agentMessage/delta":
        payload = {"type": method.replace("/", "."), **params}
        if THREAD_ID and "session_id" not in payload and "thread_id" not in payload:
            payload["session_id"] = THREAD_ID
        emit(payload)
        return False

    if method == "turn/completed":
        turn = params.get("turn") or {}
        emit(
            {
                "type": "turn.completed",
                "turn": turn,
                "usage": params.get("usage") or turn.get("usage"),
            }
        )
        ACTIVE_TURN_ID = None
        return True

    if method in {"turn/failed", "error"}:
        emit({"type": "turn.failed", "error": params.get("error") or params})
        ACTIVE_TURN_ID = None
        return True

    if method in {"thread/goal/updated", "thread/goal/cleared"}:
        emit({"type": method.replace("/", "."), **params})
        return False

    return False


def drain_until_turn_done() -> None:
    while True:
        try:
            msg = EVENTS.get(timeout=0.1)
        except queue.Empty:
            try:
                incoming = INPUTS.get_nowait()
            except queue.Empty:
                continue
            if incoming is None:
                return
            handle_input(incoming)
            continue
        if msg is None:
            return
        if emit_notification(msg):
            return


def handle_input(turn_input: dict[str, Any]) -> None:
    global ACTIVE_TURN_ID
    if turn_input.get("type") == "interrupt":
        interrupt_active_turn()
        return
    if turn_input.get("type") != "user":
        return

    configure_trace_context_for_startup(turn_input.get("trace_id"))
    start_app_server()
    thread_id = start_or_resume_thread()
    items = input_items(turn_input)
    goal, items = split_goal(items)
    if goal is not None:
        request(
            "thread/goal/set", {"threadId": thread_id, "objective": goal}, timeout=30
        )
        emit(
            {
                "type": "assistant",
                "session_id": thread_id,
                "message": {"content": [{"type": "text", "text": "Goal set."}]},
            }
        )
        emit({"type": "turn.completed"})
        return

    params = {"threadId": thread_id, "input": items}
    if ACTIVE_TURN_ID or turn_input.get("steer"):
        try:
            steer_params = {**params, "expectedTurnId": ACTIVE_TURN_ID or ""}
            result = request("turn/steer", steer_params, timeout=10)
            ACTIVE_TURN_ID = (
                str(
                    result.get("turnId")
                    or result.get("turn_id")
                    or ACTIVE_TURN_ID
                    or ""
                )
                or None
            )
            return
        except Exception:
            interrupt_active_turn()
    result = request("turn/start", params, timeout=60)
    turn = result.get("turn") or {}
    ACTIVE_TURN_ID = str(turn.get("id") or result.get("turnId") or "") or None
    drain_until_turn_done()


def interrupt_active_turn(*_args: object) -> None:
    global ACTIVE_TURN_ID
    if THREAD_ID and ACTIVE_TURN_ID:
        try:
            request(
                "turn/interrupt",
                {"threadId": THREAD_ID, "turnId": ACTIVE_TURN_ID},
                timeout=5,
            )
        except Exception as exc:
            emit({"type": "error", "message": f"interrupt failed: {exc}"})
    ACTIVE_TURN_ID = None


def exit_wrapper(*_args: object) -> None:
    global SHUTTING_DOWN
    SHUTTING_DOWN = True
    if APP and APP.poll() is None:
        APP.terminate()


def main() -> None:
    signal.signal(signal.SIGTERM, exit_wrapper)
    signal.signal(signal.SIGINT, exit_wrapper)
    signal.signal(signal.SIGUSR1, interrupt_active_turn)

    threading.Thread(target=api_stdin_reader, daemon=True).start()
    emit({"type": "system", "subtype": "wrapper_heartbeat", "phase": "startup"})

    while not SHUTTING_DOWN:
        item = INPUTS.get()
        if item is None:
            break
        try:
            handle_input(item)
        except Exception as exc:
            emit({"type": "error", "message": str(exc)})
            emit({"type": "turn.failed", "error": {"message": str(exc)}})
        time.sleep(0.01)

    exit_wrapper()
    if APP:
        APP.wait(timeout=10)


if __name__ == "__main__":
    main()
