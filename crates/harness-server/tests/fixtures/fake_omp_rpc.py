#!/usr/bin/env python3
"""Provider-free OMP 18.3.1 peer and replay driver for captured RPC frames."""
import base64
import json
import os
from pathlib import Path
import sys
import threading
import time
import uuid

assert "--no-ui" in sys.argv, "OMP peer requires --no-ui"
assert sys.argv[1:] == ["--mode", "rpc", "--no-ui"], sys.argv
lock = threading.Lock()
abort = threading.Event()
model = {"provider": "anthropic", "id": "fake-model"}
session_dir = None
session_file = None
history = {}
active_prompt = None
prompt_mode = ""
message_number = 0
negotiated = False
event_filter = None
SESSION_EVENTS = {
    "agent_start", "agent_end", "turn_start", "turn_end", "message_start",
    "message_update", "message_end", "tool_execution_start", "tool_execution_update",
    "tool_execution_end", "notice",
}


def record(value):
    if path := os.environ.get("FAKE_OMP_LOG"):
        with lock, open(path, "a") as log:
            log.write(json.dumps({"pid": os.getpid(), **value}) + "\n")


def raw(data):
    with lock:
        sys.stdout.buffer.write(data + b"\n")
        sys.stdout.buffer.flush()


def emit(frame):
    if event_filter is not None and frame["type"] in SESSION_EVENTS and frame["type"] not in event_filter:
        return
    raw(json.dumps(frame, separators=(",", ":")).encode())


def response(command, success=True, data=None):
    frame = {"type": "response", "id": command["id"], "command": command["type"], "success": success}
    if data is not None:
        frame["data"] = data
    if not success:
        frame["error"] = "fake command failure"
    emit(frame)


def result(command, status="completed", settled=True, agent_invoked=True, error=None):
    frame = {"type": "prompt_result", "id": command["id"], "status": status,
             "agentInvoked": agent_invoked, "sessionSettled": settled}
    if error is not None:
        frame["error"] = {"message": error, "retryable": False}
    emit(frame)


def message_id():
    global message_number
    message_number += 1
    return f"msg-{message_number}"


def answer(text, injected=False):
    emit({"type": "agent_start"})
    mid = message_id()
    message = {"role": "assistant"}
    emit({"type": "message_start", "messageId": mid, "message": message})
    for index, delta in enumerate((text[:len(text) // 2], text[len(text) // 2:])):
        emit({"type": "message_update", "messageId": mid, "message": message,
              "assistantMessageEvent": {"type": "text_delta", "delta": delta}})
        if injected and index == 0:
            card = {"role": "custom", "content": "injected advisory card"}
            cid = message_id()
            emit({"type": "message_start", "messageId": cid, "message": card})
            emit({"type": "message_end", "messageId": cid, "message": card})
            iid = message_id()
            emit({"type": "message_start", "messageId": iid, "message": message})
            emit({"type": "message_end", "messageId": iid, "message": {**message,
                  "content": [{"type": "text", "text": "injected assistant"}], "stopReason": "stop"}})
    emit({"type": "message_end", "messageId": mid, "message": {**message,
          "content": [{"type": "text", "text": text}], "stopReason": "stop", "usage": {"input": 10, "output": 4}}})
    emit({"type": "agent_end", "isTerminal": True, "yielded": True, "messages": []})


def complete(command, text):
    answer(text)
    result(command)
    emit({"type": "session_settled"})


def later(fn, *args):
    def run():
        time.sleep(0.02)
        fn(*args)
    threading.Thread(target=run, daemon=True).start()


def await_abort(command):
    assert abort.wait(30), "abort never arrived"
    result(command, status="aborted")
    emit({"type": "session_settled"})


def replay(command, name):
    for line in Path(__file__).with_name("omp-18.3.1").joinpath(name + ".jsonl").read_text().splitlines():
        frame = json.loads(line)
        if frame["type"] == "prompt_result":
            frame["id"] = command["id"]
            emit(frame)
        else:
            raw(line.encode())


# Over 1 MiB of UTF-8, so 256 KiB chunk cuts land inside the two-byte characters.
LARGE_ANSWER = "0123456789abcdé\n" * 90000


def chunked(payload, chunk_id, mode=""):
    size = 256 * 1024
    parts = [payload[offset:offset + size] for offset in range(0, len(payload), size)]
    for index, part in enumerate(parts):
        chunk = {"type": "rpc_chunk", "chunkId": chunk_id, "index": index,
                 "count": len(parts), "byteLength": len(payload), "data": base64.b64encode(part).decode()}
        if mode == "interleaved" and index == 1:
            chunk["chunkId"] = "rpc-2"
        elif mode == "interrupted" and index == 1:
            emit({"type": "session_settled"})
        elif mode == "gap" and index == 1:
            chunk["index"] = 2
        elif mode == "count" and index == 1:
            chunk["count"] += 1
        elif mode == "length":
            chunk["byteLength"] += 1
        elif mode == "over_cap":
            chunk["byteLength"] = 64 * 1024 * 1024 + 1
        elif mode == "base64":
            chunk["data"] = "!!!!"
        elif mode == "metadata":
            chunk["chunkId"] = ""
        emit(chunk)
        if mode == "eof":
            os._exit(0)


def chunks(command, mode):
    mid = message_id()
    message = {"role": "assistant"}
    emit({"type": "message_start", "messageId": mid, "message": message})
    frame = {"type": "message_update", "messageId": mid, "message": message,
             "assistantMessageEvent": {"type": "text_delta", "delta": LARGE_ANSWER}}
    payload = json.dumps(frame, ensure_ascii=False).encode()
    if mode == "utf8":
        payload = b'{"type":"notice","message":"' + b"x" * (1024 * 1024) + b'\xff"}'
    elif mode == "nonobject":
        payload = json.dumps("x" * (1024 * 1024)).encode()
    elif mode == "trailing":
        payload += b'{}'
    chunked(payload, "rpc-1", mode)
    end = {"type": "message_end", "messageId": mid, "message": {**message,
           "content": [{"type": "text", "text": LARGE_ANSWER}], "stopReason": "stop"}}
    chunked(json.dumps(end, ensure_ascii=False).encode(), "rpc-3")
    result(command)


def prompt(command):
    global active_prompt, prompt_mode
    assert session_file is not None, "prompt before open_session"
    text = command["message"]
    active_prompt = command
    prompt_mode = text
    abort.clear()
    if text in ("__local__", "__local_result__"):
        emit({"type": "command_output", "text": "local command completed"})
        if text == "__local__":
            response(command, data={"agentInvoked": False})
        else:
            response(command)
            result(command, agent_invoked=False)
        return
    response(command)
    if text.startswith("__replay:"):
        later(replay, command, text.split(":", 1)[1])
    elif text == "__background__":
        answer("foreground")
        result(command, settled=False)
        def follow_up():
            time.sleep(0.1)
            answer(" background follow-up")
            emit({"type": "session_settled"})
        later(follow_up)
    elif text == "__error__":
        result(command, status="error", error="provider line one\nprovider line two")
    elif text == "__long_error__":
        result(command, status="error", error="é" * 4096)
    elif text == "__aborted__":
        # Abort before dispatch: agentInvoked true, no session_settled follows.
        result(command, status="aborted")
    elif text == "__wrong_id__":
        result({"id": "not-the-prompt"})
    elif text.startswith("__host:"):
        emit({"type": text.split(":", 1)[1], "id": "unexpected", "method": "confirm"})
    elif text.startswith("__chunk:"):
        chunks(command, text.split(":", 1)[1])
    elif text == "__injected__":
        answer("reply across injection", injected=True)
        result(command)
    elif text == "__unknown_event__":
        emit({"type": "unrecognized_turn_event"})
    elif text == "__unknown_update__":
        emit({"type": "message_update", "messageId": message_id(), "message": {"role": "assistant"},
              "assistantMessageEvent": {"type": "unknown_delta"}})
    elif text == "__unknown_control__":
        response({"type": "steer", "id": "never-issued"})
    elif text == "__malformed__":
        raw(b"NOT JSON")
    elif text == "__oversize__":
        raw(b"x" * (2 * 1024 * 1024))
    elif text == "__command_then_abort__":
        emit({"type": "command_output", "text": "command output before abort"})
        emit({"type": "agent_start"})
        later(await_abort, command)
    elif text in ("__notice_error__", "__extension_error__", "__error_keeps_running__"):
        emit({"type": "extension_error" if text == "__extension_error__" else "notice",
              "level": "error", "message": "fake active turn failure"})
        later(await_abort, command)
    elif text == "__persistence_error__":
        emit({"type": "notice", "level": "error", "source": "session-persistence",
              "message": "private-store-path: write failed"})
    elif text in ("__ignore_abort__", "__late_abort__", "__steer__", "__late_steer__", "__silent_controls__"):
        emit({"type": "agent_start"})
    elif text == "__provider_env__":
        complete(command, "inherited" if os.environ.get("OPENAI_API_KEY") == "dummy" else "missing")
    elif text == "__model__":
        complete(command, model["provider"] + "/" + model["id"])
    elif text.startswith("__remember:"):
        history["codeword"] = text.split(":", 1)[1]
        session_file.write_text(json.dumps(history))
        complete(command, "remembered")
    elif text == "__recall__":
        complete(command, history.get("codeword", "forgotten"))
    elif text == "__image__":
        complete(command, "image count=" + str(len(command.get("images", []))))
    elif "Attached file saved to" in text:
        complete(command, "document path accepted")
    else:
        later(complete, command, f"fake response pid={os.getpid()} session={session_file.stem}")


def handle(command):
    global negotiated, event_filter, model, session_dir, session_file, history
    record(command)
    kind = command["type"]
    if command.get("slowNotifications"):
        for _ in range(20):
            time.sleep(0.04)
            emit({"type": "session_settled"})
    if kind == "negotiate_protocol":
        assert command["protocolVersion"] == 2
        negotiated = True
        response(command, data={"protocolVersion": 2})
    elif kind == "set_event_filter":
        assert negotiated, "filter before negotiation"
        event_filter = command["events"]
        assert event_filter == ["agent_start", "message_start", "message_update", "message_end",
                                "tool_execution_start", "tool_execution_end", "notice"]
        response(command, data={"events": event_filter})
    elif kind == "open_session":
        assert event_filter is not None, "open_session before filter"
        session_dir = Path(command["sessionDir"])
        session_dir.mkdir(parents=True, exist_ok=True)
        previous = sorted((p for p in session_dir.glob("*.jsonl") if p.stat().st_size),
                          key=lambda path: path.stat().st_mtime_ns)
        session_file = previous[-1] if previous else session_dir / f"fake-{uuid.uuid4().hex}.jsonl"
        history = json.loads(session_file.read_text()) if previous else {}
        model = history.get("model", {"provider": "anthropic", "id": "resumed-model" if previous else "fake-model"})
        session_file.write_text(json.dumps(history))
        response(command, data={"cancelled": False, "resumed": bool(previous),
                                "sessionId": session_file.stem, "sessionFile": str(session_file)})
    elif kind == "get_state":
        assert session_file is not None, "get_state before open_session"
        response(command, data={"model": model, "sessionId": session_file.stem, "sessionFile": str(session_file)})
    elif kind == "set_model":
        assert session_file is not None, "set_model before open_session"
        emit({"type": "session_settled"})
        model = {"provider": command["provider"], "id": command["modelId"]}
        history["model"] = model
        session_file.write_text(json.dumps(history))
        response(command, data=model)
    elif kind == "prompt":
        prompt(command)
    elif kind == "steer":
        if prompt_mode == "__silent_controls__":
            return
        if prompt_mode == "__late_steer__":
            complete(active_prompt, "steered before acknowledgement")
            response(command)
        else:
            response(command)
            complete(active_prompt, "steered reply")
    elif kind == "abort":
        if prompt_mode == "__ignore_abort__":
            return
        if prompt_mode == "__late_abort__":
            result(active_prompt, status="aborted")
        response(command)
        if prompt_mode == "__error_keeps_running__":
            session_dir.joinpath("error-turn-aborted").touch()
        abort.set()
    else:
        raise AssertionError(f"unexpected host command {kind}")


record({"type": "spawn", "argv": sys.argv[1:]})
versions = [1, 2]
if os.environ.get("FAKE_OMP_V1"):
    versions = [1]
if path := os.environ.get("FAKE_OMP_FAIL_ONCE"):
    marker = Path(path)
    if not marker.exists():
        marker.touch()
        versions = [1]
emit({"type": "ready", "protocolVersion": 1, "supportedProtocolVersions": versions,
      "maxFrameBytes": 1048576,
      "maxReassembledFrameBytes": int(os.environ.get("FAKE_OMP_REASSEMBLY_CAP", 67108864))})
emit({"type": "available_commands_update", "commands": []})
emit({"type": "advisor_cost_changed"})
for line in sys.stdin:
    if line.strip():
        handle(json.loads(line))
