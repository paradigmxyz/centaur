#!/usr/bin/env python3
"""Python workflow host for api-rs Absurd workflows.

The host speaks newline-delimited JSON over stdin/stdout. It imports existing
Centaur workflow modules, runs ``handler(input, ctx)``, and delegates durable
context operations back to api-rs.
"""

from __future__ import annotations

import asyncio
import dataclasses
import importlib.util
import inspect
import json
import os
import shutil
import subprocess
import sys
import traceback
import types
import typing
from pathlib import Path
from typing import Any


class ProtocolError(RuntimeError):
    pass


class WorkflowContext:
    def __init__(
        self,
        rpc: "RpcClient",
        *,
        run_id: str,
        task_id: str,
        workflow_name: str,
        pool: Any = None,
    ) -> None:
        self._rpc = rpc
        self.run_id = run_id
        self.task_id = task_id
        self.workflow_name = workflow_name
        self._pool = pool

    def log(self, event: str, **fields: Any) -> None:
        self._rpc.notify(
            {
                "type": "ctx.log",
                "message": event,
                "fields": fields,
            }
        )

    async def step(self, name: str, fn: Any, *, retry: Any = None, timeout: Any = None) -> Any:
        del retry, timeout
        started = await self._rpc.request({"type": "ctx.step.get", "step": name})
        if started.get("done"):
            return started.get("value")

        value = fn()
        if inspect.isawaitable(value):
            value = await value
        await self._rpc.request(
            {
                "type": "ctx.step.put",
                "checkpoint_name": started["checkpoint_name"],
                "value": value,
            }
        )
        return value

    async def agent_turn(self, text: str | None = None, **kwargs: Any) -> Any:
        args = dict(kwargs)
        if text is not None:
            args.setdefault("text", text)
        return await self._rpc.request({"type": "ctx.agent_turn", "args": args})

    async def run_agent(self, text: str | None = None, **kwargs: Any) -> Any:
        return await self.agent_turn(text, **kwargs)

    async def call_tool(self, tool: str, method: str, args: dict[str, Any] | None = None) -> Any:
        tool_shim = resolve_tool_shim()
        if tool_shim is not None:
            return await call_tool_shim(tool_shim, tool, method, args or {})
        return await self._rpc.request(
            {
                "type": "ctx.call_tool",
                "tool": tool,
                "method": method,
                "args": args or {},
            }
        )

    async def post_to_slack(self, channel: str, text: str, **kwargs: Any) -> Any:
        return await self._rpc.request(
            {
                "type": "ctx.post_to_slack",
                "channel": channel,
                "text": text,
                "args": kwargs,
            }
        )


class RpcClient:
    def __init__(self) -> None:
        self._next_request_id = 1
        self._pending: dict[str, asyncio.Future[Any]] = {}
        self._write_lock = asyncio.Lock()

    async def write(self, payload: dict[str, Any]) -> None:
        async with self._write_lock:
            sys.stdout.write(json.dumps(payload, separators=(",", ":"), default=str) + "\n")
            sys.stdout.flush()

    def notify(self, payload: dict[str, Any]) -> None:
        asyncio.create_task(self.write(payload))

    async def request(self, payload: dict[str, Any]) -> Any:
        request_id = str(self._next_request_id)
        self._next_request_id += 1
        payload = dict(payload)
        payload["request_id"] = request_id
        loop = asyncio.get_running_loop()
        fut: asyncio.Future[Any] = loop.create_future()
        self._pending[request_id] = fut
        await self.write(payload)
        return await fut

    def resolve(self, response: dict[str, Any]) -> None:
        request_id = str(response.get("request_id") or "")
        fut = self._pending.pop(request_id, None)
        if fut is None:
            raise ProtocolError(f"response for unknown request_id {request_id!r}")
        if response.get("ok"):
            fut.set_result(response.get("value"))
        else:
            fut.set_exception(RuntimeError(str(response.get("error") or "context RPC failed")))


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
    tool_shim: str, tool: str, method: str, args: dict[str, Any]
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


@dataclasses.dataclass
class RegisteredWorkflow:
    workflow_name: str
    source_path: str
    handler: Any
    input_cls: type | None
    webhooks: Any
    schedule: Any


def install_api_compat_module() -> None:
    api_mod = sys.modules.setdefault("api", types.ModuleType("api"))
    workflow_engine = types.ModuleType("api.workflow_engine")
    workflow_engine.WorkflowContext = WorkflowContext
    sys.modules.setdefault("api.workflow_engine", workflow_engine)
    setattr(api_mod, "workflow_engine", workflow_engine)


def workflow_dirs() -> list[Path]:
    dirs = []
    raw = os.getenv("WORKFLOW_DIRS", "")
    for entry in raw.split(":"):
        entry = entry.strip()
        if entry:
            path = Path(entry).expanduser().resolve()
            if path.is_dir():
                dirs.append(path)
    return dirs


def module_name_for(path: Path) -> str:
    return f"_centaur_workflow_host.{path.stem}_{abs(hash(str(path)))}"


def load_workflow_file(path: Path) -> RegisteredWorkflow | None:
    spec = importlib.util.spec_from_file_location(module_name_for(path), path)
    if spec is None or spec.loader is None:
        return None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)

    workflow_name = getattr(module, "WORKFLOW_NAME", None)
    handler = getattr(module, "handler", None)
    if not isinstance(workflow_name, str) or not callable(handler):
        return None
    return RegisteredWorkflow(
        workflow_name=workflow_name,
        source_path=str(path),
        handler=handler,
        input_cls=getattr(module, "Input", None),
        webhooks=getattr(module, "WEBHOOKS", None),
        schedule=getattr(module, "SCHEDULE", None),
    )


def discover_workflows() -> dict[str, RegisteredWorkflow]:
    install_api_compat_module()
    discovered: dict[str, RegisteredWorkflow] = {}
    for directory in workflow_dirs():
        if str(directory) not in sys.path:
            sys.path.insert(0, str(directory.parent))
            sys.path.insert(0, str(directory))
        for path in sorted(directory.glob("*.py")):
            if path.name.startswith("_"):
                continue
            try:
                registered = load_workflow_file(path)
            except Exception as exc:
                print(f"workflow_load_error path={path} error={exc}", file=sys.stderr)
                continue
            if registered is None:
                continue
            if registered.workflow_name in discovered:
                raise RuntimeError(f"duplicate workflow name {registered.workflow_name!r}")
            discovered[registered.workflow_name] = registered
    return discovered


def coerce_value(value: Any, target_type: type) -> Any:
    if not dataclasses.is_dataclass(target_type):
        return value
    if isinstance(value, target_type):
        return value
    if not isinstance(value, dict):
        return value
    fields = {field.name for field in dataclasses.fields(target_type)}
    try:
        hints = typing.get_type_hints(target_type)
    except Exception:
        hints = {}
    coerced = {}
    for key, raw in value.items():
        if key not in fields:
            continue
        hint = hints.get(key)
        if isinstance(hint, type) and dataclasses.is_dataclass(hint) and isinstance(raw, dict):
            coerced[key] = coerce_value(raw, hint)
        else:
            coerced[key] = raw
    return target_type(**coerced)


def coerce_input(raw: Any, input_cls: type | None) -> Any:
    if input_cls is None:
        return raw
    try:
        return coerce_value(raw, input_cls)
    except Exception:
        return raw


async def create_pool() -> Any:
    database_url = os.getenv("DATABASE_URL", "").strip()
    if not database_url:
        return None
    try:
        import asyncpg  # type: ignore
    except ImportError:
        return None
    return await asyncpg.create_pool(database_url)


def jsonable(value: Any) -> Any:
    if dataclasses.is_dataclass(value):
        return dataclasses.asdict(value)
    json.dumps(value, default=str)
    return value


def normalize_webhook_auth(auth: Any) -> dict[str, Any]:
    if auth is None or auth == "none":
        return {"type": "none"}
    if isinstance(auth, str):
        return {"type": auth}
    if isinstance(auth, dict):
        if "type" not in auth:
            return {"type": "none", **auth}
        return auth
    return {"type": "none"}


def normalize_webhook_spec(workflow: RegisteredWorkflow, raw: Any) -> dict[str, Any] | None:
    if not isinstance(raw, dict):
        return None
    spec = dict(raw)
    slug = spec.get("slug")
    if not isinstance(slug, str) or not slug.strip():
        return None
    spec["slug"] = slug.strip()
    spec["auth"] = normalize_webhook_auth(spec.get("auth"))
    return {
        "workflow_name": workflow.workflow_name,
        "source_path": workflow.source_path,
        "spec": spec,
    }


def normalize_webhooks(workflow: RegisteredWorkflow) -> list[dict[str, Any]]:
    raw = workflow.webhooks
    if raw is None:
        return []
    if isinstance(raw, dict):
        raw = [raw]
    if not isinstance(raw, list):
        return []
    return [
        spec
        for spec in (normalize_webhook_spec(workflow, item) for item in raw)
        if spec is not None
    ]


def normalize_schedule(workflow: RegisteredWorkflow) -> dict[str, Any] | None:
    raw = workflow.schedule
    if not isinstance(raw, dict):
        return None
    schedule = dict(raw)
    schedule.setdefault("schedule_id", workflow.workflow_name)
    schedule.setdefault("workflow_name", workflow.workflow_name)
    schedule.setdefault("source_path", workflow.source_path)
    return schedule


async def run_workflow(message: dict[str, Any], rpc: RpcClient) -> dict[str, Any]:
    workflows = discover_workflows()
    workflow_name = str(message.get("workflow_name") or "")
    registered = workflows.get(workflow_name)
    if registered is None:
        raise RuntimeError(f"unknown workflow_name {workflow_name!r}")

    pool = await create_pool()
    ctx = WorkflowContext(
        rpc,
        run_id=str(message.get("run_id") or ""),
        task_id=str(message.get("task_id") or ""),
        workflow_name=workflow_name,
        pool=pool,
    )
    try:
        inp = coerce_input(message.get("input") or {}, registered.input_cls)
        result = registered.handler(inp, ctx)
        if inspect.isawaitable(result):
            result = await result
        return {"type": "workflow.result", "result": jsonable(result)}
    finally:
        if pool is not None:
            await pool.close()


def discovery_payload() -> dict[str, Any]:
    workflows = discover_workflows()
    return {
        "type": "workflow.discovery",
        "workflows": [
            {
                "workflow_name": workflow.workflow_name,
                "source_path": workflow.source_path,
                "webhooks": normalize_webhooks(workflow),
                "schedule": normalize_schedule(workflow),
            }
            for workflow in workflows.values()
        ],
    }


async def main() -> int:
    rpc = RpcClient()
    stdin_queue: asyncio.Queue[dict[str, Any] | None] = asyncio.Queue()
    completion_queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue()
    active_workflow: asyncio.Task[dict[str, Any]] | None = None

    async def read_stdin() -> None:
        while True:
            line = await asyncio.to_thread(sys.stdin.readline)
            if line == "":
                await stdin_queue.put(None)
                return
            await stdin_queue.put(json.loads(line))

    asyncio.create_task(read_stdin())

    def watch_workflow(task: asyncio.Task[dict[str, Any]]) -> None:
        try:
            completion_queue.put_nowait(task.result())
        except Exception as exc:
            completion_queue.put_nowait(
                {
                    "type": "workflow.error",
                    "message": str(exc),
                    "traceback": traceback.format_exc(),
                }
            )

    while True:
        stdin_get = asyncio.create_task(stdin_queue.get())
        completion_get = asyncio.create_task(completion_queue.get())
        done, pending = await asyncio.wait(
            {stdin_get, completion_get},
            return_when=asyncio.FIRST_COMPLETED,
        )
        for task in pending:
            task.cancel()

        if completion_get in done:
            active_workflow = None
            await rpc.write(completion_get.result())
            return 0

        message = stdin_get.result()
        if message is None:
            if active_workflow is not None:
                continue
            await asyncio.sleep(0.1)
            asyncio.create_task(read_stdin())
            continue
        message_type = message.get("type")
        try:
            if message_type == "ctx.response":
                rpc.resolve(message)
                continue
            if message_type == "workflow.discover":
                await rpc.write(discovery_payload())
                return 0
            if message_type == "workflow.start":
                if active_workflow is not None:
                    await rpc.write(
                        {
                            "type": "workflow.error",
                            "message": "workflow host already has an active workflow",
                        }
                    )
                    continue
                active_workflow = asyncio.create_task(run_workflow(message, rpc))
                active_workflow.add_done_callback(watch_workflow)
                continue
            raise ProtocolError(f"unknown message type {message_type!r}")
        except Exception as exc:
            await rpc.write(
                {
                    "type": "host.error",
                    "message": str(exc),
                    "traceback": traceback.format_exc(),
                }
            )


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
