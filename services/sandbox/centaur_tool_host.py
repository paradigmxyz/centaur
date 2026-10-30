#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import subprocess
import sys
import traceback
from typing import Any


def _text(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode(errors="replace")
    return str(value)


def _v1_call_command(request: dict[str, Any]) -> list[str]:
    """V1 invokes a Python client method with JSON keyword arguments."""
    return [
        "centaur-tools",
        "call",
        str(request["tool"]),
        str(request["method"]),
        json.dumps(request.get("arguments", {}), separators=(",", ":")),
    ]


def _v2_run_command(request: dict[str, Any]) -> list[str]:
    """V2 invokes the tool CLI with literal argv tokens."""
    argv = request["argv"]
    if not isinstance(argv, list):
        raise TypeError("tool argv must be an array of strings")
    for arg in argv:
        if not isinstance(arg, str):
            raise TypeError("each tool argv token must be a string")
        if "\0" in arg:
            raise ValueError("tool argv tokens must not contain NUL characters")
    return ["centaur-tools", "run", str(request["tool"]), *argv]


def _command_for_request(request: dict[str, Any]) -> list[str]:
    # V1 requests predate the mode field and invoke Python client methods.
    mode = request.get("mode", "v1")
    match mode:
        case "v1":
            return _v1_call_command(request)
        case "v2":
            return _v2_run_command(request)
        case _:
            raise ValueError(f"unsupported tool host mode: {mode}")


def _run_tool(request: dict[str, Any]) -> dict[str, Any]:
    request_id = request.get("id")
    command = _command_for_request(request)
    timeout_seconds = max(1, int(request.get("timeout_seconds") or 120))

    env = os.environ.copy()
    principal_id = request.get("principal_id")
    token_id = request.get("token_id")
    if principal_id:
        env["CENTAUR_MCP_PRINCIPAL_ID"] = str(principal_id)
    if token_id:
        env["CENTAUR_MCP_TOKEN_ID"] = str(token_id)

    try:
        completed = subprocess.run(
            command,
            check=False,
            text=True,
            capture_output=True,
            timeout=timeout_seconds,
            env=env,
        )
        return {
            "id": request_id,
            "status": completed.returncode,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
            "timed_out": False,
        }
    except subprocess.TimeoutExpired as exc:
        return {
            "id": request_id,
            "status": None,
            "stdout": _text(exc.stdout),
            "stderr": _text(exc.stderr)
            + f"\ncentaur tool call timed out after {timeout_seconds}s",
            "timed_out": True,
        }


def _emit_result(response: dict[str, Any]) -> None:
    print(
        json.dumps(
            {
                "type": "result",
                "turn_id": response.get("id"),
                "result": json.dumps(response, separators=(",", ":")),
            },
            separators=(",", ":"),
        ),
        flush=True,
    )


def main() -> int:
    print("__CENTAUR_TOOL_HOST_READY", flush=True)
    for raw_line in sys.stdin:
        raw_line = raw_line.strip()
        if not raw_line:
            continue
        request_id = None
        try:
            request = json.loads(raw_line)
            request_id = request.get("id")
            response = _run_tool(request)
        except Exception:
            response = {
                "id": request_id,
                "status": 1,
                "stdout": "",
                "stderr": traceback.format_exc(),
                "timed_out": False,
            }
        _emit_result(response)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
