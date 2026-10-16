#!/usr/bin/env python3
"""App-server fixture with durable history and controllable transport failures."""
import json
import os
from pathlib import Path
import sys
import time

root = Path(os.environ["RECOVERY_FIXTURE_DIR"])
history_path = root / "history.json"
history = []


def log(value):
    with (root / "requests.jsonl").open("a") as stream:
        stream.write(json.dumps(value) + "\n")


def emit(value):
    print(json.dumps(value), flush=True)


log({"pid": os.getpid()})
for line in sys.stdin:
    request = json.loads(line)
    log(request)
    method = request["method"]
    params = request.get("params", {})
    if method == "initialize":
        emit({"id": request["id"], "result": {"userAgent": "recovery-fixture"}})
    elif method in ("thread/start", "thread/resume"):
        if method == "thread/resume":
            assert params["threadId"] == "saved-thread"
            history = json.loads(history_path.read_text())
            reject = root / "reject-resume"
            if reject.exists():
                reject.unlink()
                emit({"id": request["id"], "error": {"code": -32000, "message": "resume unavailable"}})
                continue
        else:
            assert not history_path.exists(), "must resume the saved conversation"
        emit({"id": request["id"], "result": {"thread": {"id": "saved-thread"}, "model": "first-model"}})
    elif method == "turn/start":
        assert params["threadId"] == "saved-thread"
        prompt = params["input"][0]["text"]
        turn = {"id": "turn-1", "items": [], "itemsView": "full", "status": "inProgress", "error": None}
        emit({"id": request["id"], "result": {"turn": turn}})
        emit({"method": "turn/started", "params": {"threadId": "saved-thread", "turn": turn}})
        if prompt in ("die-during-turn", "close-stdout"):
            emit({"method": "item/agentMessage/delta", "params": {"threadId": "saved-thread", "turnId": "turn-1", "itemId": "answer", "delta": "already executed"}})
            if prompt == "close-stdout":
                os.close(1)
                while True:
                    time.sleep(60)
            sys.exit(7)
        history.append(prompt)
        history_path.write_text(json.dumps(history))
        if prompt == "close-stdin":
            os.close(0)
        emit({"method": "item/agentMessage/delta", "params": {"threadId": "saved-thread", "turnId": "turn-1", "itemId": "answer", "delta": "|".join(history)}})
        turn["status"] = "completed"
        emit({"method": "turn/completed", "params": {"threadId": "saved-thread", "turn": turn}})
        if prompt == "close-stdin":
            while True:
                time.sleep(60)
    else:
        raise AssertionError(method)
