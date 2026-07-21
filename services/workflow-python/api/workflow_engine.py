from __future__ import annotations

import dataclasses
import datetime as dt
import hashlib
import inspect
import json
from typing import Any

from api.app import (
    WorkflowToolManager,
    WorkflowTools,
    bind_context_rpc,
    reset_context_rpc,
)


@dataclasses.dataclass
class Delivery:
    channel: str = ""
    thread_ts: str = ""
    mode: str = ""
    metadata: dict[str, Any] = dataclasses.field(default_factory=dict)


class WorkflowContext:
    def __init__(
        self,
        rpc: Any,
        *,
        run_id: str,
        task_id: str,
        workflow_name: str,
        pool: Any = None,
        agent_defaults: dict[str, Any] | None = None,
    ) -> None:
        self._rpc = rpc
        self.run_id = run_id
        self.task_id = task_id
        self.workflow_name = workflow_name
        self._pool = pool
        # Module-level `AGENT_DEFAULTS` (e.g. {"model": ..., "reasoning": ...})
        # applied to every ctx.agent_turn as a per-workflow default; explicit
        # per-call kwargs always win. See agent_turn().
        self._agent_defaults = dict(agent_defaults or {})
        self.tools = WorkflowTools(
            WorkflowToolManager(self._rpc, durable_call=self.call_tool)
        )

    def log(self, event: str, **fields: Any) -> None:
        self._rpc.notify(
            {
                "type": "ctx.log",
                "message": event,
                "fields": fields,
            }
        )

    async def step(
        self,
        name: str,
        fn: Any,
        *,
        retry: Any = None,
        timeout: Any = None,
        step_kind: str | None = None,
    ) -> Any:
        del retry, timeout
        request: dict[str, Any] = {"type": "ctx.step.get", "step": name}
        if step_kind:
            request["step_kind"] = step_kind
        started = await self._rpc.request(request)
        if started.get("done"):
            return started.get("value")

        token = bind_context_rpc(self._rpc)
        try:
            value = fn()
            if inspect.isawaitable(value):
                value = await value
        finally:
            reset_context_rpc(token)
        await self._rpc.request(
            {
                "type": "ctx.step.put",
                "checkpoint_name": started["checkpoint_name"],
                "value": value,
                **({"step_kind": step_kind} if step_kind else {}),
            }
        )
        return value

    async def sleep(self, name: str, duration: dt.timedelta | int | float) -> None:
        await self._rpc.request(
            {
                "type": "ctx.sleep",
                "step": name,
                "duration_seconds": duration_seconds(duration),
            }
        )

    async def sleep_until(self, name: str, when: dt.datetime) -> None:
        if when.tzinfo is None:
            when = when.replace(tzinfo=dt.timezone.utc)
        await self._rpc.request(
            {
                "type": "ctx.sleep_until",
                "step": name,
                "wake_at": when.astimezone(dt.timezone.utc).isoformat(),
            }
        )

    async def agent_turn(self, text: str | None = None, **kwargs: Any) -> Any:
        # Per-workflow AGENT_DEFAULTS (model / provider / reasoning / harness,
        # ...) form the base; explicit per-call kwargs override them key by key.
        args = {**self._agent_defaults, **kwargs}
        if text is not None:
            args.setdefault("text", text)
        return await self._rpc.request({"type": "ctx.agent_turn", "args": args})

    async def run_agent(
        self, *args: Any, text: str | None = None, **kwargs: Any
    ) -> Any:
        if args:
            kwargs.setdefault("name", args[0])
            if len(args) > 1:
                raise TypeError(
                    "run_agent accepts at most one positional name argument"
                )
        return await self.agent_turn(text, **kwargs)

    async def start_agent(
        self, *args: Any, text: str | None = None, **kwargs: Any
    ) -> Any:
        return await self.run_agent(*args, text=text, **kwargs)

    async def start_workflow(
        self,
        workflow_name: str,
        input: dict[str, Any] | None = None,
        *,
        idempotency_key: str | None = None,
    ) -> dict[str, Any]:
        """Queue another workflow and return its durable task identifiers."""
        request: dict[str, Any] = {
            "type": "ctx.workflow.start",
            "workflow_name": workflow_name,
            "input": input or {},
        }
        if idempotency_key:
            request["idempotency_key"] = idempotency_key
        return await self._rpc.request(request)

    async def call_tool(
        self, tool: str, method: str, args: dict[str, Any] | None = None
    ) -> Any:
        payload = args or {}
        step_name = durable_tool_step_name(tool, method, payload)

        async def execute() -> Any:
            return await WorkflowToolManager(self._rpc).call_tool_raw(
                tool, method, payload
            )

        return await self.step(step_name, execute, step_kind="tool_call")

    async def post_to_slack(self, channel: str, text: str, **kwargs: Any) -> Any:
        return await self._rpc.request(
            {
                "type": "ctx.post_to_slack",
                "channel": channel,
                "text": text,
                "args": kwargs,
            }
        )


def duration_seconds(value: dt.timedelta | int | float) -> float:
    if isinstance(value, dt.timedelta):
        return max(value.total_seconds(), 0.0)
    return max(float(value), 0.0)


def durable_tool_step_name(tool: str, method: str, args: dict[str, Any]) -> str:
    normalized_tool = tool.strip()
    normalized_method = method.strip()
    if not normalized_tool or not normalized_method:
        raise ValueError("tool and method must be non-empty")
    canonical = json.dumps(
        {"args": args, "method": normalized_method, "tool": normalized_tool},
        sort_keys=True,
        separators=(",", ":"),
        default=str,
    ).encode()
    digest = hashlib.sha256(canonical).hexdigest()[:24]
    return f"$ctx.call_tool:{_checkpoint_label(normalized_tool)}.{_checkpoint_label(normalized_method)}:{digest}"


def _checkpoint_label(value: str) -> str:
    label = "".join(
        character
        if character.isascii() and (character.isalnum() or character in "._-")
        else "_"
        for character in value
    )
    return label[:64] or "tool"
