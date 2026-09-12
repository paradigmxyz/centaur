#!/usr/bin/env python3
from __future__ import annotations

import base64
import fcntl
import json
import os
import stat
import subprocess
import sys
import traceback
from pathlib import Path
from typing import Any


DOWNLOADS_ROOT = Path("/tmp/downloads")
MAX_DOWNLOAD_BYTES = 10 * 1024 * 1024
MACOS_F_GETPATH = 50


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


def _open_file_path(file_fd: int) -> Path:
    """Return the kernel-resolved path for an already-open file descriptor."""
    if sys.platform.startswith("linux"):
        return Path(os.readlink(f"/proc/self/fd/{file_fd}"))
    if sys.platform == "darwin":
        # F_GETPATH returns the path associated with the open descriptor on macOS.
        path_buffer = fcntl.fcntl(file_fd, MACOS_F_GETPATH, bytes(1024))
        path = path_buffer.split(b"\0", 1)[0]
        return Path(os.fsdecode(path))
    raise OSError(f"open file path lookup is unsupported on {sys.platform}")


def _open_download_file(artifact_path: Any) -> tuple[int, os.stat_result]:
    """Open one regular file whose resolved location is below the download root."""
    if not isinstance(artifact_path, str) or not artifact_path:
        raise ValueError("artifact path must be a non-empty string")
    relative_path = Path(artifact_path)
    if (
        relative_path.is_absolute()
        or relative_path == Path(".")
        or ".." in relative_path.parts
        or "\0" in artifact_path
    ):
        raise ValueError("artifact path must be relative to /tmp/downloads")

    root_flags = os.O_RDONLY
    if hasattr(os, "O_DIRECTORY"):
        root_flags |= os.O_DIRECTORY
    if hasattr(os, "O_NOFOLLOW"):
        root_flags |= os.O_NOFOLLOW
    root_fd = os.open(DOWNLOADS_ROOT, root_flags)
    file_fd: int | None = None
    try:
        root_path = _open_file_path(root_fd)
        file_flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0)
        file_fd = os.open(artifact_path, file_flags, dir_fd=root_fd)
        file_stat = os.fstat(file_fd)
        if not stat.S_ISREG(file_stat.st_mode):
            raise ValueError("download target must be a regular file")
        if file_stat.st_size > MAX_DOWNLOAD_BYTES:
            raise ValueError(
                f"download file exceeds the {MAX_DOWNLOAD_BYTES}-byte size limit"
            )
        file_path = _open_file_path(file_fd)
        try:
            file_path.relative_to(root_path)
        except ValueError as error:
            raise ValueError(
                "artifact path resolves outside /tmp/downloads"
            ) from error
        return file_fd, file_stat
    except Exception:
        if file_fd is not None:
            os.close(file_fd)
        raise
    finally:
        os.close(root_fd)


def _read_download_file(artifact_path: Any) -> bytes:
    file_fd, _ = _open_download_file(artifact_path)
    with os.fdopen(file_fd, "rb") as file:
        contents = file.read(MAX_DOWNLOAD_BYTES + 1)
    if len(contents) > MAX_DOWNLOAD_BYTES:
        raise ValueError(
            f"download file exceeds the {MAX_DOWNLOAD_BYTES}-byte size limit"
        )
    return contents


def _download_file(request: dict[str, Any]) -> dict[str, Any]:
    artifact_path = request.get("path")
    contents = _read_download_file(artifact_path)
    return {
        "id": request.get("id"),
        "status": 0,
        "path": artifact_path,
        "size_bytes": len(contents),
        "data_base64": base64.b64encode(contents).decode("ascii"),
        "stderr": "",
        "timed_out": False,
    }


def _run_tool(request: dict[str, Any]) -> dict[str, Any]:
    request_id = request.get("id")
    if request.get("mode") == "download_file":
        return _download_file(request)
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


def _emit_result(response: dict[str, Any], *, transient: bool = False) -> None:
    print(
        json.dumps(
            {
                "type": "centaur.transient_result" if transient else "result",
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
        transient = False
        try:
            request = json.loads(raw_line)
            request_id = request.get("id")
            transient = request.get("mode") == "download_file"
            response = _run_tool(request)
        except Exception:
            response = {
                "id": request_id,
                "status": 1,
                "stdout": "",
                "stderr": traceback.format_exc(),
                "timed_out": False,
            }
        _emit_result(response, transient=transient)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
