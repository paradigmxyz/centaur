from __future__ import annotations

import asyncio
import json
import shutil
import subprocess
import sys
from contextvars import ContextVar, Token
from pathlib import Path
from typing import Any


_ACTIVE_RPC: ContextVar[Any | None] = ContextVar("centaur_workflow_active_rpc", default=None)


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
    stdout, stderr = await proc.communicate()
    text = stdout.decode(errors="replace").strip()
    err = stderr.decode(errors="replace").strip()
    if proc.returncode != 0:
        detail = err or text or f"exit code {proc.returncode}"
        raise RuntimeError(f"centaur-tools call {tool}.{method} failed: {detail}")
    if not text:
        return None
    return json.loads(text)


class WorkflowToolManager:
    def __init__(self, rpc: Any | None = None) -> None:
        self._rpc = rpc

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
            raise TypeError("tool method calls accept either one dict positional arg or keywords")
        if not args:
            payload = kwargs
        elif len(args) == 1 and isinstance(args[0], dict):
            payload = args[0]
        else:
            raise TypeError("tool method calls accept at most one positional dict arg")
        return await self._manager.call_tool_raw(self._tool, self._method, payload)


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
