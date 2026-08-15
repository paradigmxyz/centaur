#!/usr/bin/env python3
"""Python workflow host for api-rs Absurd workflows.

The host speaks newline-delimited JSON over stdin/stdout. It imports existing
Centaur workflow modules, runs ``handler(input, ctx)``, and delegates durable
context operations back to api-rs.
"""

from __future__ import annotations

import asyncio
import ast
import dataclasses
import importlib.util
import inspect
import json
import os
import sys
import traceback
import typing
from pathlib import Path
from typing import Any

from api import metrics
from api.workflow_engine import WorkflowContext

DATABASE_CONNECT_ATTEMPTS = 5
DATABASE_CONNECT_BACKOFF_SECONDS = 0.25
DATABASE_CONNECT_BACKOFF_MAX_SECONDS = 2.0


class ProtocolError(RuntimeError):
    pass


class RpcClient:
    def __init__(self) -> None:
        self._next_request_id = 1
        self._pending: dict[str, asyncio.Future[Any]] = {}
        self._notifications: set[asyncio.Task[None]] = set()
        self._write_lock = asyncio.Lock()

    async def write(self, payload: dict[str, Any]) -> None:
        async with self._write_lock:
            sys.stdout.write(json.dumps(payload, separators=(",", ":"), default=str) + "\n")
            sys.stdout.flush()

    def notify(self, payload: dict[str, Any]) -> None:
        task = asyncio.create_task(self.write(payload))
        self._notifications.add(task)
        task.add_done_callback(self._notifications.discard)

    async def drain_notifications(self) -> None:
        if self._notifications:
            await asyncio.gather(*list(self._notifications), return_exceptions=True)

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


@dataclasses.dataclass
class RegisteredWorkflow:
    workflow_name: str
    source_path: str
    handler: Any
    input_cls: type | None
    webhooks: Any
    schedule: Any
    principal: Any = None
    agent_defaults: dict[str, Any] | None = None


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


def workflow_allowed_names() -> set[str]:
    raw = os.getenv("WORKFLOW_ALLOWED_NAMES", "")
    return {
        name
        for name in (part.strip() for part in raw.replace(",", " ").split())
        if name
    }


def workflow_enabled(workflow_name: str) -> bool:
    mode = os.getenv("WORKFLOW_ENABLE_MODE", "").strip().lower()
    if not mode or mode == "all":
        return True
    if mode == "allowlist":
        return workflow_name.strip() in workflow_allowed_names()
    raise RuntimeError(f'WORKFLOW_ENABLE_MODE must be "all" or "allowlist", got {mode!r}')


def configure_workflow_import_paths(dirs: list[Path]) -> None:
    for directory in dirs:
        candidate_paths = [directory.parent, directory]
        if directory.name == "workflows":
            candidate_paths.append(directory.parent / "services" / "api")
        for path in candidate_paths:
            if path.is_dir() and str(path) not in sys.path:
                sys.path.insert(0, str(path))


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
    agent_defaults = getattr(module, "AGENT_DEFAULTS", None)
    if not isinstance(agent_defaults, dict):
        agent_defaults = None
    return RegisteredWorkflow(
        workflow_name=workflow_name,
        source_path=str(path),
        handler=handler,
        input_cls=getattr(module, "Input", None),
        webhooks=getattr(module, "WEBHOOKS", None),
        schedule=getattr(module, "SCHEDULE", None),
        principal=getattr(module, "WORKFLOW_PRINCIPAL", None),
        agent_defaults=agent_defaults,
    )


def _constant_string(node: ast.AST | None) -> str | None:
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    return None


def workflow_name_from_source(path: Path) -> str | None:
    """Return ``WORKFLOW_NAME`` when it is a top-level string constant.

    Discovery uses this so ``WORKFLOW_ENABLE_MODE=allowlist`` can reject modules
    before ``exec_module`` runs their top-level code.
    """
    try:
        tree = ast.parse(path.read_text())
    except Exception:
        return None
    name: str | None = None
    saw_assignment = False
    for node in tree.body:
        value: ast.AST | None = None
        if isinstance(node, ast.Assign):
            if any(isinstance(target, ast.Name) and target.id == "WORKFLOW_NAME" for target in node.targets):
                value = node.value
        elif isinstance(node, ast.AnnAssign):
            target = node.target
            if isinstance(target, ast.Name) and target.id == "WORKFLOW_NAME":
                value = node.value
        if value is None:
            continue
        saw_assignment = True
        # Last assignment wins, matching Python module semantics.
        name = _constant_string(value)
    if not saw_assignment:
        return None
    return name


def discover_workflows() -> dict[str, RegisteredWorkflow]:
    dirs = workflow_dirs()
    configure_workflow_import_paths(dirs)
    discovered: dict[str, RegisteredWorkflow] = {}
    for directory in dirs:
        for path in sorted(directory.rglob("*.py")):
            if path.name == "__init__.py" or path.name.startswith("_"):
                continue
            workflow_name = workflow_name_from_source(path)
            if workflow_name is None:
                continue
            if not workflow_enabled(workflow_name):
                continue
            try:
                registered = load_workflow_file(path)
            except Exception as exc:
                print(f"workflow_load_error path={path} error={exc}", file=sys.stderr)
                continue
            if registered is None:
                continue
            if registered.workflow_name != workflow_name:
                print(
                    f"workflow_name_mismatch path={path} "
                    f"declared={workflow_name!r} loaded={registered.workflow_name!r}",
                    file=sys.stderr,
                )
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

    last_error: Exception | None = None
    for attempt in range(1, DATABASE_CONNECT_ATTEMPTS + 1):
        try:
            return await asyncpg.create_pool(database_url)
        except Exception as exc:
            last_error = exc
            if attempt == DATABASE_CONNECT_ATTEMPTS:
                break
            delay = min(
                DATABASE_CONNECT_BACKOFF_MAX_SECONDS,
                DATABASE_CONNECT_BACKOFF_SECONDS * (2 ** (attempt - 1)),
            )
            print(
                "workflow_database_connect_retry "
                f"attempt={attempt} attempts={DATABASE_CONNECT_ATTEMPTS} "
                f"delay_seconds={delay} "
                f"error={type(exc).__name__}: {exc}",
                file=sys.stderr,
            )
            await asyncio.sleep(delay)
    assert last_error is not None
    raise last_error


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


def normalize_principal(workflow: RegisteredWorkflow) -> bool | None:
    raw = workflow.principal
    return raw if isinstance(raw, bool) and raw else None


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
        agent_defaults=registered.agent_defaults,
    )
    previous_metric_rpc = metrics.get_metric_rpc()
    metrics.set_metric_rpc(rpc)
    try:
        inp = coerce_input(message.get("input") or {}, registered.input_cls)
        result = registered.handler(inp, ctx)
        if inspect.isawaitable(result):
            result = await result
        return {
            "type": "workflow.result",
            "workflow_run_id": ctx.run_id,
            "run_id": ctx.run_id,
            "workflow_task_id": ctx.task_id,
            "task_id": ctx.task_id,
            "workflow_name": ctx.workflow_name,
            "result": jsonable(result),
        }
    finally:
        await rpc.drain_notifications()
        metrics.set_metric_rpc(previous_metric_rpc)
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
                "principal": normalize_principal(workflow),
            }
            for workflow in workflows.values()
        ],
    }


async def read_protocol_line(reader: asyncio.StreamReader, buffer: bytearray) -> bytes:
    search_from = 0
    while True:
        newline = buffer.find(b"\n", search_from)
        if newline >= 0:
            line = bytes(buffer[: newline + 1])
            del buffer[: newline + 1]
            return line

        search_from = len(buffer)
        chunk = await reader.read(64 * 1024)
        if chunk:
            buffer.extend(chunk)
            continue
        if buffer:
            line = bytes(buffer)
            buffer.clear()
            return line
        return b""


async def main() -> int:
    rpc = RpcClient()
    active_workflow: asyncio.Task[dict[str, Any]] | None = None

    async def workflow_response(task: asyncio.Task[dict[str, Any]]) -> dict[str, Any]:
        try:
            return await task
        except Exception as exc:
            return {
                "type": "workflow.error",
                "message": str(exc),
                "traceback": traceback.format_exc(),
            }

    loop = asyncio.get_running_loop()
    reader = asyncio.StreamReader()
    protocol = asyncio.StreamReaderProtocol(reader)
    try:
        transport, _ = await loop.connect_read_pipe(lambda: protocol, sys.stdin)
    except Exception as exc:
        await rpc.write(
            {
                "type": "host.error",
                "message": f"failed to read workflow host input: {exc}",
                "traceback": traceback.format_exc(),
            }
        )
        return 1

    stdin_read: asyncio.Task[bytes] | None = None
    stdin_buffer = bytearray()

    try:
        while True:
            if active_workflow is None:
                line = await read_protocol_line(reader, stdin_buffer)
            else:
                stdin_read = asyncio.create_task(
                    read_protocol_line(reader, stdin_buffer)
                )
                done, _ = await asyncio.wait(
                    {stdin_read, active_workflow},
                    return_when=asyncio.FIRST_COMPLETED,
                )
                if active_workflow in done:
                    await rpc.write(await workflow_response(active_workflow))
                    active_workflow = None
                    return 0
                line = stdin_read.result()
                stdin_read = None

            if line == b"":
                if active_workflow is not None:
                    await rpc.write(await workflow_response(active_workflow))
                    active_workflow = None
                return 0

            try:
                message = json.loads(line)
                if not isinstance(message, dict):
                    raise ProtocolError("workflow host input must be a JSON object")
            except Exception as exc:
                await rpc.write(
                    {
                        "type": "host.error",
                        "message": f"invalid workflow host input: {exc}",
                        "traceback": traceback.format_exc(),
                    }
                )
                return 1

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
    finally:
        transport.close()
        remaining = [task for task in (stdin_read, active_workflow) if task is not None]
        for task in remaining:
            if not task.done():
                task.cancel()
        await asyncio.gather(*remaining, return_exceptions=True)


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
