from __future__ import annotations

import dataclasses
import datetime as dt
import inspect
from typing import Any

from api.app import WorkflowToolManager, WorkflowTools, bind_context_rpc, reset_context_rpc


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
    ) -> None:
        self._rpc = rpc
        self.run_id = run_id
        self.task_id = task_id
        self.workflow_name = workflow_name
        self._pool = pool
        self.tools = WorkflowTools(WorkflowToolManager(self._rpc))

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
        args = dict(kwargs)
        if text is not None:
            args.setdefault("text", text)
        return await self._rpc.request({"type": "ctx.agent_turn", "args": args})

    async def run_agent(self, *args: Any, text: str | None = None, **kwargs: Any) -> Any:
        if args:
            kwargs.setdefault("name", args[0])
            if len(args) > 1:
                raise TypeError("run_agent accepts at most one positional name argument")
        return await self.agent_turn(text, **kwargs)

    async def start_agent(self, *args: Any, text: str | None = None, **kwargs: Any) -> Any:
        return await self.run_agent(*args, text=text, **kwargs)

    async def call_tool(self, tool: str, method: str, args: dict[str, Any] | None = None) -> Any:
        return await WorkflowToolManager(self._rpc).call_tool_raw(tool, method, args or {})

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
