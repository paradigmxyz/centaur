from __future__ import annotations

import asyncio
import json
import shutil
import subprocess
import sys
from contextvars import ContextVar, Token
from pathlib import Path
from typing import Any, Awaitable, Callable


_ACTIVE_RPC: ContextVar[Any | None] = ContextVar(
    "centaur_workflow_active_rpc", default=None
)
_TOOL_CALL_TIMEOUT_SECONDS = 120.0
_TOOL_CALL_MAX_OUTPUT_BYTES = 1024 * 1024
_TOOL_CALL_READ_CHUNK_BYTES = 64 * 1024


def bind_context_rpc(rpc: Any) -> Token[Any | None]:
    return _ACTIVE_RPC.set(rpc)


def reset_context_rpc(token: Token[Any | None]) -> None:
    _ACTIVE_RPC.reset(token)


def resolve_tool_shim() -> str | None:
    if tool_shim := shutil.which("centaur-tools"):
        return tool_shim
    fallback = Path("/home/agent/.local/bin/centaur-tools")
    if fallback.exists():
        return str(fallback)
    installer = Path("/usr/local/bin/install-tool-shims")
    if installer.exists():
        subprocess.run(
            [str(installer)],
            check=False,
            stdout=sys.stderr,
            stderr=sys.stderr,
        )
        if tool_shim := shutil.which("centaur-tools"):
            return tool_shim
        if fallback.exists():
            return str(fallback)
    return None


async def _read_bounded_tool_output(
    stream: asyncio.StreamReader,
    *,
    stream_name: str,
) -> bytes:
    output = bytearray()
    while chunk := await stream.read(_TOOL_CALL_READ_CHUNK_BYTES):
        if len(output) + len(chunk) > _TOOL_CALL_MAX_OUTPUT_BYTES:
            raise RuntimeError(
                f"centaur-tools {stream_name} exceeded "
                f"{_TOOL_CALL_MAX_OUTPUT_BYTES} bytes"
            )
        output.extend(chunk)
    return bytes(output)


async def _terminate_and_reap_tool_process(
    proc: asyncio.subprocess.Process,
) -> None:
    if proc.returncode is None:
        try:
            proc.kill()
        except ProcessLookupError:
            pass
    await proc.wait()


async def _collect_tool_process_output(
    proc: asyncio.subprocess.Process,
) -> tuple[bytes, bytes]:
    assert proc.stdout is not None
    assert proc.stderr is not None
    tasks = [
        asyncio.create_task(
            _read_bounded_tool_output(proc.stdout, stream_name="stdout")
        ),
        asyncio.create_task(
            _read_bounded_tool_output(proc.stderr, stream_name="stderr")
        ),
        asyncio.create_task(proc.wait()),
    ]
    try:
        stdout, stderr, _ = await asyncio.gather(*tasks)
        return stdout, stderr
    finally:
        for task in tasks:
            if not task.done():
                task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)


async def call_tool_shim(
    tool_shim: str,
    tool: str,
    method: str,
    args: dict[str, Any],
) -> Any:
    proc = await asyncio.create_subprocess_exec(
        tool_shim,
        "call",
        tool,
        method,
        json.dumps(args, separators=(",", ":"), default=str),
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    try:
        stdout, _stderr = await asyncio.wait_for(
            _collect_tool_process_output(proc),
            timeout=_TOOL_CALL_TIMEOUT_SECONDS,
        )
    except TimeoutError as exc:
        await _terminate_and_reap_tool_process(proc)
        raise RuntimeError(
            f"centaur-tools call {tool}.{method} exceeded "
            f"{_TOOL_CALL_TIMEOUT_SECONDS:g} seconds"
        ) from exc
    except BaseException:
        await _terminate_and_reap_tool_process(proc)
        raise
    text = stdout.decode(errors="replace").strip()
    if proc.returncode != 0:
        raise RuntimeError(
            f"centaur-tools call {tool}.{method} failed with exit code "
            f"{proc.returncode}"
        )
    if not text:
        return None
    return json.loads(text)


class WorkflowToolManager:
    def __init__(
        self,
        rpc: Any | None = None,
        *,
        durable_call: Callable[[str, str, dict[str, Any]], Awaitable[Any]]
        | None = None,
    ) -> None:
        self._rpc = rpc
        self._durable_call = durable_call

    async def call_tool_raw(
        self,
        tool: str,
        method: str,
        args: dict[str, Any] | None = None,
    ) -> Any:
        tool_shim = resolve_tool_shim()
        if tool_shim is not None:
            return await call_tool_shim(tool_shim, tool, method, args or {})
        if self._rpc is not None:
            return await self._rpc.request(
                {
                    "type": "ctx.call_tool",
                    "tool": tool,
                    "method": method,
                    "args": args or {},
                }
            )
        raise RuntimeError(
            "centaur-tools is not installed and no active workflow context RPC is available"
        )

    async def call_tool(
        self,
        tool: str,
        method: str,
        args: dict[str, Any] | None = None,
    ) -> Any:
        if self._durable_call is not None:
            return await self._durable_call(tool, method, args or {})
        return await self.call_tool_raw(tool, method, args)


def get_tool_manager() -> WorkflowToolManager:
    return WorkflowToolManager(_ACTIVE_RPC.get())


class WorkflowToolMethod:
    def __init__(self, manager: WorkflowToolManager, tool: str, method: str) -> None:
        self._manager = manager
        self._tool = tool
        self._method = method

    async def __call__(self, *args: Any, **kwargs: Any) -> Any:
        if args and kwargs:
            raise TypeError(
                "tool method calls accept either one dict positional arg or keywords"
            )
        if not args:
            payload = kwargs
        elif len(args) == 1 and isinstance(args[0], dict):
            payload = args[0]
        else:
            raise TypeError("tool method calls accept at most one positional dict arg")
        return await self._manager.call_tool(self._tool, self._method, payload)


class WorkflowToolProxy:
    def __init__(self, manager: WorkflowToolManager, tool: str) -> None:
        self._manager = manager
        self._tool = tool

    def __getattr__(self, method: str) -> WorkflowToolMethod:
        if method.startswith("_"):
            raise AttributeError(method)
        return WorkflowToolMethod(self._manager, self._tool, method)


class WorkflowTools:
    def __init__(self, manager: WorkflowToolManager) -> None:
        self._manager = manager

    def __getattr__(self, tool: str) -> WorkflowToolProxy:
        if tool.startswith("_"):
            raise AttributeError(tool)
        return WorkflowToolProxy(self._manager, tool)
