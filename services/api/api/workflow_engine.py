"""Durable workflow engine using the checkpoint/replay model.

Inspired by Cloudflare Workflows and earendil-works/absurd.  The workflow
handler function IS the workflow.  Steps are runtime-discovered via
``ctx.step(name, fn)`` calls.  The engine checkpoints each step result to
Postgres.  On resume after crash or suspension, the handler re-executes
top-to-bottom but skips steps that already have checkpoints (returning
the cached result instantly).  Dynamic branching, loops, and conditional
logic work naturally because it is just Python.

Handler discovery follows the same pattern as tools: Python files in the
``api/workflows/`` package directory, each exporting ``WORKFLOW_NAME``
and an async ``handler(params, ctx)`` function.
"""

from __future__ import annotations

import asyncio
import contextlib
import datetime as dt
import hashlib
import importlib
import importlib.util
import os
import sys
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Awaitable, Callable, TypeVar
from zoneinfo import ZoneInfo

import structlog

from api.agent import _insert_system_message
from api import slackbot_client
from api.harness_config import default_harness
from api.runtime_control import (
    ControlPlaneError,
    append_message,
    cancel_execution,
    canonical_json,
    decode_jsonb,
    enqueue_execution,
    get_active_assignment,
    get_execution,
    request_hash,
    spawn_assignment,
)
from api.vm_metrics import (
    record_workflow_event_sent,
    record_workflow_run_claimed,
    record_workflow_run_enqueued,
    record_workflow_run_terminal,
)
from api.webhooks import clear_webhook_specs, register_workflow_webhooks

log = structlog.get_logger()

T = TypeVar("T")


# ── Typed helpers ─────────────────────────────────────────────────────────


@dataclass
class Delivery:
    """Where to deliver execution results.  Construct via class methods."""

    platform: str = "dev"
    channel: str | None = None
    thread_ts: str | None = None
    recipient_user_id: str | None = None
    recipient_team_id: str | None = None
    channel_id: str | None = None
    team_id: str | None = None

    @classmethod
    def slack(
        cls,
        channel: str,
        thread_ts: str,
        *,
        user_id: str | None = None,
        team_id: str | None = None,
    ) -> Delivery:
        return cls(
            platform="slack",
            channel=channel,
            thread_ts=thread_ts,
            recipient_user_id=user_id,
            recipient_team_id=team_id,
        )

    @classmethod
    def dev(cls) -> Delivery:
        return cls(platform="dev")

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {"platform": self.platform}
        channel = self.channel or self.channel_id
        recipient_team_id = self.recipient_team_id or self.team_id
        if channel:
            d["channel"] = channel
        if self.thread_ts:
            d["thread_ts"] = self.thread_ts
        if self.recipient_user_id:
            d["recipient_user_id"] = self.recipient_user_id
        if recipient_team_id:
            d["recipient_team_id"] = recipient_team_id
        return d

    @classmethod
    def from_dict(cls, d: dict[str, Any] | None) -> Delivery:
        if not d or not isinstance(d, dict):
            return cls()
        return cls(
            platform=str(d.get("platform") or "dev"),
            channel=d.get("channel") or d.get("channel_id"),
            thread_ts=d.get("thread_ts"),
            recipient_user_id=d.get("recipient_user_id") or d.get("user_id"),
            recipient_team_id=d.get("recipient_team_id") or d.get("team_id"),
        )


def text_part(text: str) -> dict[str, str]:
    """Convenience: build a text content block."""
    return {"type": "text", "text": text}


@dataclass
class RetryPolicy:
    """Per-step retry configuration, inspired by Cloudflare Workflows."""

    limit: int = 0
    delay: dt.timedelta = field(default_factory=dt.timedelta)
    backoff: str = "fixed"  # "fixed" | "exponential"
    max_delay: dt.timedelta | None = None

    def delay_for_attempt(self, attempt: int) -> float:
        """Return delay in seconds for a given attempt (0-indexed)."""
        base = self.delay.total_seconds()
        if self.backoff == "exponential":
            delay = base * (2**attempt)
        else:
            delay = base
        if self.max_delay is not None:
            delay = min(delay, self.max_delay.total_seconds())
        return max(delay, 0.0)


# ── Configuration ─────────────────────────────────────────────────────────

WORKFLOW_RECONCILE_INTERVAL_S = max(
    float(os.getenv("WORKFLOW_RECONCILE_INTERVAL_S", "0.5")), 0.25
)
WORKFLOW_WORKER_CONCURRENCY = max(
    int(os.getenv("WORKFLOW_WORKER_CONCURRENCY", "2")), 1
)
WORKFLOW_WORKER_LEASE_S = max(
    float(os.getenv("WORKFLOW_WORKER_LEASE_S", "30.0")), 1.0
)
WORKFLOW_SCHEDULE_TICK_INTERVAL_S = max(
    float(os.getenv("WORKFLOW_SCHEDULE_TICK_INTERVAL_S", "5.0")), 0.5
)
WORKFLOW_SCHEDULE_CATCHUP_LIMIT = max(
    int(os.getenv("WORKFLOW_SCHEDULE_CATCHUP_LIMIT", "5")), 1
)
WORKFLOW_SCHEDULE_MISFIRE_GRACE_S = max(
    float(os.getenv("WORKFLOW_SCHEDULE_MISFIRE_GRACE_S", "90.0")), 0.0
)
WORKFLOW_INSTANCE_ID = (
    f"{os.getenv('HOSTNAME') or 'api'}:wf:{uuid.uuid4().hex[:8]}"
)
# Minimum delay before a waiting/sleeping workflow run can be re-claimed.
# Prevents hot-loop starvation when available_at is in the past (e.g. elapsed
# deadline on a still-running child workflow).
WORKFLOW_RESUSPEND_BACKOFF_S = max(
    float(os.getenv("WORKFLOW_RESUSPEND_BACKOFF_S", "5.0")), 1.0
)

_workflow_tasks: list[asyncio.Task] = []
_workflow_wake = asyncio.Event()
_tick_lock = asyncio.Lock()
_last_tick_at = 0.0
_EXECUTION_TERMINAL_STATUSES = {"completed", "failed_permanent", "cancelled"}
_WORKFLOW_TERMINAL_STATUSES = {"completed", "failed", "cancelled"}


# ── Exceptions ────────────────────────────────────────────────────────────


class SuspendWorkflow(Exception):
    """Raised by ctx.sleep / handler logic to suspend the run."""

    def __init__(
        self,
        *,
        available_at: dt.datetime | None = None,
        status: str = "sleeping",
    ):
        super().__init__("workflow suspended")
        self.available_at = available_at or dt.datetime.max.replace(
            tzinfo=dt.timezone.utc,
        )
        self.status = status


class CancelledWorkflow(Exception):
    """Raised at checkpoint write when the run has been cancelled."""


class NonRetryableError(Exception):
    """Raised inside a step to prevent retries and fail immediately."""


# ── WorkflowContext ───────────────────────────────────────────────────────


class WorkflowContext:
    """Checkpoint-aware context passed to every workflow handler.

    Provides three primitives:
    - ``step(name, fn)`` — execute *fn* once, cache the return value
    - ``sleep(name, duration)`` — suspend the run for *duration*
    - ``wait_for_event(name, event_type, …)`` — suspend until an
      external event arrives
    """

    def __init__(
        self,
        *,
        pool,
        run_id: str,
        checkpoints: dict[str, Any],
        lease_s: float,
        worker_id: str,
        run_input: dict[str, Any] | None = None,
    ):
        self._pool = pool
        self.run_id = run_id
        self._checkpoints = checkpoints
        self._step_counter: dict[str, int] = {}
        self._in_replay = bool(checkpoints)
        self._lease_s = lease_s
        self._worker_id = worker_id
        self.run_input: dict[str, Any] = run_input or {}

    # ── Internal ──────────────────────────────────────────────────────

    @staticmethod
    def _format_step_name(name: str, count: int) -> str:
        return name if count == 1 else f"{name}#{count}"

    def _peek_resolved_name(self, name: str) -> str:
        """Return the checkpoint name the next ``step(name, ...)`` will use."""
        return self._format_step_name(name, self._step_counter.get(name, 0) + 1)

    def _resolve_name(self, name: str) -> str:
        """Auto-deduplicate step names for loops (name, name#2, …)."""
        count = self._step_counter.get(name, 0) + 1
        self._step_counter[name] = count
        return self._format_step_name(name, count)

    async def _persist_checkpoint(
        self,
        checkpoint_name: str,
        value: Any,
        *,
        step_kind: str | None = None,
        execution_id: str | None = None,
        child_run_id: str | None = None,
    ) -> None:
        """Write checkpoint + extend lease atomically, with fencing."""
        async with self._pool.acquire() as conn:
            async with conn.transaction():
                # Fence: verify we still own the lease
                row = await conn.fetchrow(
                    "SELECT status, worker_id "
                    "FROM workflow_runs "
                    "WHERE run_id = $1 FOR UPDATE",
                    self.run_id,
                )
                if not row:
                    raise CancelledWorkflow()
                if row["status"] == "cancelled":
                    raise CancelledWorkflow()
                if row["worker_id"] != self._worker_id:
                    raise CancelledWorkflow()

                await conn.execute(
                    "INSERT INTO workflow_checkpoints "
                    "(run_id, checkpoint_name, step_kind, state, "
                    "execution_id, child_run_id) "
                    "VALUES ($1, $2, $3, $4::jsonb, $5, $6) "
                    "ON CONFLICT (run_id, checkpoint_name) DO UPDATE "
                    "SET state = EXCLUDED.state, "
                    "    execution_id = EXCLUDED.execution_id, "
                    "    child_run_id = EXCLUDED.child_run_id",
                    self.run_id,
                    checkpoint_name,
                    step_kind,
                    canonical_json(value),
                    execution_id,
                    child_run_id,
                )
                await conn.execute(
                    "UPDATE workflow_runs "
                    "SET worker_lease_expires_at = NOW() "
                    "    + make_interval("
                    "        secs => $2::double precision"
                    "    ), "
                    "    updated_at = NOW() "
                    "WHERE run_id = $1 AND worker_id = $3",
                    self.run_id,
                    self._lease_s,
                    self._worker_id,
                )
        self._checkpoints[checkpoint_name] = value

    @staticmethod
    async def _call_step_fn(fn: Callable[[], Awaitable[T] | T]) -> T:
        """Call the step function, awaiting if it returns a coroutine."""
        result = fn()
        if asyncio.iscoroutine(result) or asyncio.isfuture(result):
            result = await result
        return result  # type: ignore[return-value]

    # ── Public API ────────────────────────────────────────────────────

    async def step(
        self,
        name: str,
        fn: Callable[[], Awaitable[T] | T],
        *,
        step_kind: str | None = None,
        execution_id: str | None = None,
        child_run_id: str | None = None,
        retry: RetryPolicy | None = None,
        timeout: dt.timedelta | None = None,
    ) -> T:
        """Execute *fn* exactly once; return cached result on replay.

        *retry* — optional per-step retry policy.  On failure the step is
        re-tried up to ``retry.limit`` times with the configured delay and
        backoff.  ``NonRetryableError`` skips retries and propagates
        immediately.

        *timeout* — optional per-step timeout.  If *fn* does not return
        within *timeout*, ``TimeoutError`` is raised (retryable by default).
        """
        checkpoint_name = self._resolve_name(name)
        if checkpoint_name in self._checkpoints:
            cached = self._checkpoints[checkpoint_name]
            return cached  # type: ignore[return-value]

        self._in_replay = False

        max_attempts = 1 + (retry.limit if retry else 0)
        last_err: Exception | None = None

        for attempt in range(max_attempts):
            try:
                if timeout is not None:
                    result = await asyncio.wait_for(
                        self._call_step_fn(fn), timeout.total_seconds(),
                    )
                else:
                    result = await self._call_step_fn(fn)

                eid = execution_id
                cid = child_run_id
                if eid is None and step_kind == "agent_turn" and isinstance(result, dict):
                    eid = result.get("execution_id")
                if cid is None and step_kind == "child_workflow_start" and isinstance(result, dict):
                    cid = result.get("run_id")

                await self._persist_checkpoint(
                    checkpoint_name, result, step_kind=step_kind,
                    execution_id=eid,
                    child_run_id=cid,
                )
                return result  # type: ignore[return-value]

            except NonRetryableError:
                raise
            except CancelledWorkflow:
                raise
            except SuspendWorkflow:
                raise
            except Exception as err:
                last_err = err
                if attempt + 1 >= max_attempts:
                    break
                delay = retry.delay_for_attempt(attempt) if retry else 0.0
                if delay > 0:
                    await asyncio.sleep(delay)

        raise last_err  # type: ignore[misc]

    async def sleep(self, name: str, duration: dt.timedelta) -> None:
        """Suspend the run for *duration*; no-op on replay if past."""
        checkpoint_name = self._resolve_name(name)
        if checkpoint_name in self._checkpoints:
            cached = self._checkpoints[checkpoint_name]
            wake_at_str = cached if isinstance(cached, str) else None
            if wake_at_str:
                wake_at = dt.datetime.fromisoformat(wake_at_str)
                if dt.datetime.now(dt.timezone.utc) < wake_at:
                    raise SuspendWorkflow(available_at=wake_at)
            return  # wake time already passed — fall through

        wake_at = dt.datetime.now(dt.timezone.utc) + duration
        await self._persist_checkpoint(
            checkpoint_name, wake_at.isoformat(), step_kind="sleep",
        )
        raise SuspendWorkflow(available_at=wake_at)

    async def sleep_until(self, name: str, when: dt.datetime) -> None:
        """Suspend the run until *when*; no-op on replay if past."""
        checkpoint_name = self._resolve_name(name)
        if checkpoint_name in self._checkpoints:
            cached = self._checkpoints[checkpoint_name]
            wake_at_str = cached if isinstance(cached, str) else None
            if wake_at_str:
                wake_at = dt.datetime.fromisoformat(wake_at_str)
                if dt.datetime.now(dt.timezone.utc) < wake_at:
                    raise SuspendWorkflow(available_at=wake_at)
            return

        wake_at = when if when.tzinfo else when.replace(tzinfo=dt.timezone.utc)
        await self._persist_checkpoint(
            checkpoint_name, wake_at.isoformat(), step_kind="sleep",
        )
        if dt.datetime.now(dt.timezone.utc) < wake_at:
            raise SuspendWorkflow(available_at=wake_at)

    def log(self, msg: str, **kwargs: Any) -> None:
        """Emit a structured log line, suppressed during replay.

        Use this instead of the module-level ``log`` for messages
        inside workflow handlers — it auto-includes run context and
        avoids duplicate log lines when steps are replayed from cache.
        """
        if self._in_replay:
            return
        log.info(
            msg,
            workflow_run_id=self.run_id,
            **kwargs,
        )

    async def wait_for_event(
        self,
        name: str,
        *,
        event_type: str,
        correlation_id: str,
        timeout: dt.timedelta | None = None,
    ) -> dict[str, Any]:
        """Suspend until an external event arrives, or time out.

        The event is matched by *event_type* + *correlation_id*.  If the
        event was already emitted before this call, it returns immediately
        with the cached checkpoint.  Otherwise the run suspends (status =
        ``waiting``) and is woken when ``send_workflow_event()`` delivers
        the matching event.

        *timeout* — optional.  If set and the event has not arrived when
        the timeout elapses, ``TimeoutError`` is raised.
        """
        checkpoint_name = self._resolve_name(name)
        if checkpoint_name in self._checkpoints:
            cached = self._checkpoints[checkpoint_name]
            if isinstance(cached, dict) and cached.get("_waiting"):
                # Wait marker — event wasn't available yet on last run.
                # Check if event arrived since we last ran
                existing = await self._pool.fetchrow(
                    "SELECT payload FROM workflow_events "
                    "WHERE event_type = $1 AND correlation_id = $2",
                    event_type,
                    correlation_id,
                )
                if existing:
                    payload = decode_jsonb(existing["payload"], {})
                    await self._persist_checkpoint(
                        checkpoint_name, payload, step_kind="event",
                    )
                    return payload
                deadline_str = cached.get("deadline")
                if deadline_str:
                    deadline = dt.datetime.fromisoformat(deadline_str)
                    if dt.datetime.now(dt.timezone.utc) >= deadline:
                        raise TimeoutError(
                            f"wait_for_event timed out: {event_type}:{correlation_id}"
                        )
                # Still not available — re-suspend
                raise SuspendWorkflow(
                    status="waiting",
                    available_at=(
                        dt.datetime.fromisoformat(deadline_str)
                        if deadline_str
                        else dt.datetime.max.replace(tzinfo=dt.timezone.utc)
                    ),
                )
            return cached  # type: ignore[return-value]

        # Check if event already exists in DB
        existing = await self._pool.fetchrow(
            "SELECT payload FROM workflow_events "
            "WHERE event_type = $1 AND correlation_id = $2",
            event_type,
            correlation_id,
        )
        if existing:
            payload = decode_jsonb(existing["payload"], {})
            await self._persist_checkpoint(
                checkpoint_name, payload, step_kind="event",
            )
            return payload

        # Event not yet available — persist a wait-marker checkpoint and suspend
        wait_marker: dict[str, Any] = {
            "_waiting": True,
            "event_type": event_type,
            "correlation_id": correlation_id,
        }
        if timeout is not None:
            deadline = dt.datetime.now(dt.timezone.utc) + timeout
            wait_marker["deadline"] = deadline.isoformat()
        else:
            deadline = None

        await self._persist_checkpoint(
            checkpoint_name, wait_marker, step_kind="event_wait",
        )
        raise SuspendWorkflow(
            status="waiting",
            available_at=(
                deadline
                if deadline
                else dt.datetime.max.replace(tzinfo=dt.timezone.utc)
            ),
        )

    async def start_workflow(
        self,
        name: str,
        *,
        workflow_name: str,
        run_input: dict[str, Any],
        trigger_key: str | None = None,
        eager_start: bool = False,
    ) -> dict[str, Any]:
        """Create a child workflow run and checkpoint its run metadata."""

        async def _start() -> dict[str, Any]:
            response = await create_workflow_run(
                self._pool,
                workflow_name=workflow_name,
                run_input=run_input,
                trigger_key=trigger_key,
                eager_start=eager_start,
                parent_run_id=self.run_id,
            )
            # Strip execution_id so ctx.step does not auto-link the parent's
            # child_workflow_start checkpoint to the child's execution. The
            # child owns that execution_id in its own checkpoint namespace;
            # duplicating it here violates the global unique index on
            # workflow_checkpoints.execution_id.
            response.pop("execution_id", None)
            return response

        return await self.step(
            name,
            _start,
            step_kind="child_workflow_start",
        )

    async def wait_for_workflow(
        self,
        name: str,
        *,
        run_id: str,
        timeout: dt.timedelta | None = None,
    ) -> dict[str, Any]:
        """Suspend until a child workflow reaches a terminal state."""
        checkpoint_name = self._resolve_name(name)
        if checkpoint_name in self._checkpoints:
            cached = self._checkpoints[checkpoint_name]
            if isinstance(cached, dict) and cached.get("_waiting"):
                child_run_id = str(cached.get("child_run_id") or run_id)
                child = await get_workflow_run(self._pool, child_run_id)
                if child is None:
                    raise ControlPlaneError(
                        "CHILD_WORKFLOW_MISSING",
                        f"child workflow run not found: {child_run_id}",
                        409,
                    )
                if child.get("status") in _WORKFLOW_TERMINAL_STATUSES:
                    await self._persist_checkpoint(
                        checkpoint_name,
                        child,
                        step_kind="child_workflow_result",
                        child_run_id=child_run_id,
                    )
                    return child
                deadline_str = cached.get("deadline")
                if deadline_str:
                    deadline = dt.datetime.fromisoformat(deadline_str)
                    if dt.datetime.now(dt.timezone.utc) >= deadline:
                        raise TimeoutError(
                            f"wait_for_workflow timed out: {child_run_id}"
                        )
                raise SuspendWorkflow(
                    status="waiting",
                    available_at=(
                        dt.datetime.fromisoformat(deadline_str)
                        if deadline_str
                        else dt.datetime.max.replace(tzinfo=dt.timezone.utc)
                    ),
                )
            return cached  # type: ignore[return-value]

        child = await get_workflow_run(self._pool, run_id)
        if child is None:
            raise ControlPlaneError(
                "CHILD_WORKFLOW_MISSING",
                f"child workflow run not found: {run_id}",
                409,
            )
        if child.get("status") in _WORKFLOW_TERMINAL_STATUSES:
            await self._persist_checkpoint(
                checkpoint_name,
                child,
                step_kind="child_workflow_result",
                child_run_id=run_id,
            )
            return child

        wait_marker: dict[str, Any] = {
            "_waiting": True,
            "child_run_id": run_id,
            "workflow_name": child.get("workflow_name"),
        }
        if timeout is not None:
            deadline = dt.datetime.now(dt.timezone.utc) + timeout
            wait_marker["deadline"] = deadline.isoformat()
        else:
            deadline = None

        await self._persist_checkpoint(
            checkpoint_name,
            wait_marker,
            step_kind="child_workflow_wait",
            child_run_id=run_id,
        )
        raise SuspendWorkflow(
            status="waiting",
            available_at=(
                deadline
                if deadline
                else dt.datetime.max.replace(tzinfo=dt.timezone.utc)
            ),
        )

    async def run_workflow(
        self,
        name: str,
        *,
        workflow_name: str,
        run_input: dict[str, Any],
        trigger_key: str | None = None,
        timeout: dt.timedelta | None = None,
        eager_start: bool = False,
    ) -> dict[str, Any]:
        """Create a child workflow and wait for its terminal result."""
        child = await self.start_workflow(
            f"{name}.start",
            workflow_name=workflow_name,
            run_input=run_input,
            trigger_key=trigger_key,
            eager_start=eager_start,
        )
        child_run_id = child.get("run_id") if isinstance(child, dict) else None
        if not child_run_id:
            raise ControlPlaneError(
                "CHILD_WORKFLOW_MISSING",
                "child workflow run did not return run_id",
                500,
            )
        return await self.wait_for_workflow(
            f"{name}.wait",
            run_id=str(child_run_id),
            timeout=timeout,
        )

    async def start_agent(
        self,
        name: str,
        *,
        parts: list[dict[str, Any]] | None = None,
        text: str | None = None,
        thread_key: str | None = None,
        message_id: str | None = None,
        user_id: str | None = None,
        metadata: dict[str, Any] | None = None,
        delivery: Delivery | dict[str, Any] | None = None,
        harness: str | None = None,
        persona: str | None = None,
        agents_md_override: str | None = None,
        trigger_key: str | None = None,
        eager_start: bool = False,
    ) -> dict[str, Any]:
        run_input: dict[str, Any] = {
            "parts": parts or [],
            "text": text,
            "message_id": message_id,
            "user_id": user_id,
            "metadata": metadata or {},
            "harness": harness,
            "persona": persona,
            "agents_md_override": agents_md_override,
        }
        if thread_key:
            run_input["thread_key"] = thread_key
        if isinstance(delivery, Delivery):
            run_input["delivery"] = delivery.to_dict()
        elif isinstance(delivery, dict):
            run_input["delivery"] = delivery
        return await self.start_workflow(
            name,
            workflow_name="agent_turn",
            run_input=run_input,
            trigger_key=trigger_key,
            eager_start=eager_start,
        )

    async def run_agent(
        self,
        name: str,
        *,
        parts: list[dict[str, Any]] | None = None,
        text: str | None = None,
        thread_key: str | None = None,
        message_id: str | None = None,
        user_id: str | None = None,
        metadata: dict[str, Any] | None = None,
        delivery: Delivery | dict[str, Any] | None = None,
        harness: str | None = None,
        persona: str | None = None,
        agents_md_override: str | None = None,
        trigger_key: str | None = None,
        timeout: dt.timedelta | None = None,
        eager_start: bool = False,
    ) -> dict[str, Any]:
        run_input: dict[str, Any] = {
            "parts": parts or [],
            "text": text,
            "message_id": message_id,
            "user_id": user_id,
            "metadata": metadata or {},
            "harness": harness,
            "persona": persona,
            "agents_md_override": agents_md_override,
        }
        if thread_key:
            run_input["thread_key"] = thread_key
        if isinstance(delivery, Delivery):
            run_input["delivery"] = delivery.to_dict()
        elif isinstance(delivery, dict):
            run_input["delivery"] = delivery
        return await self.run_workflow(
            name,
            workflow_name="agent_turn",
            run_input=run_input,
            trigger_key=trigger_key,
            timeout=timeout,
            eager_start=eager_start,
        )

    async def post_to_slack(
        self,
        channel: str,
        text: str,
        *,
        thread_ts: str | None = None,
    ) -> dict[str, Any]:
        """Post a message to a Slack channel via the slack tool.

        Accepts channel name (e.g. ``"team-updates"``) or ID.
        Uses a checkpointed step so the message is sent exactly once,
        even if the workflow replays.
        """
        from api.app import get_tool_manager

        async def _post() -> dict[str, Any]:
            tm = get_tool_manager()
            args: dict[str, Any] = {
                "channel": channel,
                "text": text,
                "no_attribution": True,
            }
            if thread_ts:
                args["thread_ts"] = thread_ts
            raw = await tm.call_tool("slack", "send_message", args)
            import json as _json
            try:
                result = _json.loads(raw) if isinstance(raw, str) else raw
            except (ValueError, TypeError):
                result = {"raw": raw}
            if isinstance(result, dict) and result.get("error"):
                raise RuntimeError(str(result["error"]))
            return result

        step_name = f"post_slack_{channel}"
        return await self.step(step_name, _post, step_kind="slack_post")

    @property
    def tools(self) -> _ToolProxy:
        """Dynamic tool proxy for ergonomic checkpointed tool calls.

        Usage::

            result = await ctx.tools.websearch.search(query="ETH price")
        """
        return _ToolProxy(self)

    async def call_tool(
        self,
        tool: str,
        method: str,
        args: dict[str, Any] | None = None,
    ) -> Any:
        """Call an API tool with checkpointed exactly-once semantics.

        Example::

            data = await ctx.call_tool("websearch", "search", {"query": "ETH price"})
        """
        from api.app import get_tool_manager

        async def _call() -> Any:
            import json as _json
            tm = get_tool_manager()
            raw = await tm.call_tool(tool, method, args or {})
            try:
                return _json.loads(raw) if isinstance(raw, str) else raw
            except (_json.JSONDecodeError, TypeError):
                return {"raw": raw}

        return await self.step(
            f"tool_{tool}_{method}", _call, step_kind="tool_call",
        )

    async def agent_turn(
        self,
        prompt: str,
        **kwargs: Any,
    ) -> dict[str, Any]:
        """Run an agent turn and return the result.

        Simplest usage::

            result = await ctx.agent_turn("Generate a daily digest.")
            text = result["result_text"]

        The agent's ``thread_key`` and ``delivery`` are resolved from the
        workflow run's input (set automatically by the schedule engine).
        Pass explicit ``thread_key=``, ``delivery=``, etc. as kwargs to
        override.
        """
        return await do_agent_turn(self, prompt=prompt, **kwargs)


# ── Tool proxy (ergonomic checkpointed tool calls) ───────────────────


class _ToolMethodProxy:
    """Proxy for a single tool — attribute access returns an awaitable method call."""

    __slots__ = ("_ctx", "_tool")

    def __init__(self, ctx: WorkflowContext, tool: str):
        self._ctx = ctx
        self._tool = tool

    def __getattr__(self, method: str) -> Callable[..., Awaitable[Any]]:
        async def _call(**kwargs: Any) -> Any:
            return await self._ctx.call_tool(self._tool, method, kwargs or None)
        return _call


class _ToolProxy:
    """Proxy returned by ``ctx.tools`` — attribute access returns a tool proxy."""

    __slots__ = ("_ctx",)

    def __init__(self, ctx: WorkflowContext):
        self._ctx = ctx

    def __getattr__(self, tool: str) -> _ToolMethodProxy:
        return _ToolMethodProxy(self._ctx, tool)


# ── Agent-turn helper (domain logic, NOT on the context) ──────────────


_EXECUTION_HARNESSES = frozenset({"amp", "claude-code", "codex", "pi-mono"})


async def _compute_agent_session_title(
    pool,
    thread_key: str,
    selector: dict[str, str | None],
) -> str:
    """Build the streamed timeline header from the resolved persona and harness.

    Renders as ``Centaur · {persona} · {engine}`` so users always see which
    persona is active and which underlying runtime is executing. Persona
    assignments use ``harness`` as the requested selector/profile and ``engine``
    as the actual runtime, so prefer ``engine`` for display whenever present.
    """
    persona = selector.get("persona_id")
    harness = selector.get("harness")
    if not persona or not harness:
        active = await get_active_assignment(pool, thread_key)
        if isinstance(active, dict):
            persona = persona or _nonempty(active.get("persona_id"))
            harness = harness or _assignment_display_engine(active)
    if persona and (not harness or harness == persona):
        harness = _persona_default_engine(persona) or (None if harness == persona else harness)
    if not persona and not harness:
        harness = default_harness()
    parts = ["Centaur"]
    if persona:
        parts.append(str(persona))
    if harness:
        parts.append(str(harness))
    return " · ".join(parts)


async def _compute_agent_session_header(
    pool,
    thread_key: str,
    selector: dict[str, str | None],
) -> str:
    """Build the per-message italic header (``"persona · engine-model"``).

    Mirrors ``_compute_agent_session_title`` for the resolution path but emits
    the persona/engine pair the slackbot renders italic at the top of every
    assistant message. Persona defaults to the literal ``"base"`` when no
    persona is active. The engine segment is upgraded to a concrete model
    identifier (e.g. ``claude-opus-4-7``, ``codex-gpt-5``) when known.
    """
    from api.runtime_control import _agent_session_header  # local to avoid cycle

    persona = selector.get("persona_id")
    harness = selector.get("harness")
    engine: str | None = None
    if not persona or not harness:
        active = await get_active_assignment(pool, thread_key)
        if isinstance(active, dict):
            persona = persona or _nonempty(active.get("persona_id"))
            harness = harness or _nonempty(active.get("harness"))
            engine = _nonempty(active.get("engine"))
    if persona and not engine:
        engine = _persona_default_engine(persona)
    return _agent_session_header(
        persona_id=persona,
        engine=engine,
        harness=harness,
    )


def _history_has_assistant_message(history_messages: Any) -> bool:
    if not isinstance(history_messages, list):
        return False
    return any(
        isinstance(item, dict)
        and str(item.get("role") or "").strip().lower() == "assistant"
        for item in history_messages
    )


async def _thread_has_prior_slack_agent_reply(pool, thread_key: str) -> bool:
    if await pool.fetchval(
        "SELECT EXISTS (SELECT 1 FROM agent_execution_requests "
        "WHERE thread_key = $1 AND COALESCE(delivery->>'platform', '') = 'slack' "
        "AND COALESCE(metadata->>'slackbot_agent_session_id', '') <> '')",
        thread_key,
    ):
        return True
    return bool(
        await pool.fetchval(
            "SELECT EXISTS (SELECT 1 FROM agent_message_requests "
            "WHERE thread_key = $1 AND event_json->>'type' = 'assistant')",
            thread_key,
        )
    )


async def _should_show_agent_session_header(
    pool,
    *,
    thread_key: str,
    history_messages: Any,
) -> bool:
    if _history_has_assistant_message(history_messages):
        return False
    return not await _thread_has_prior_slack_agent_reply(pool, thread_key)


def _assignment_display_engine(active: dict[str, Any]) -> str | None:
    engine = _nonempty(active.get("engine"))
    if engine:
        return engine
    return _nonempty(active.get("harness"))


def _persona_default_engine(persona_id: str) -> str | None:
    try:
        from api.app import get_tool_manager

        persona = get_tool_manager().get_persona(persona_id)
    except Exception:
        return None
    return _nonempty(getattr(persona, "engine", None)) if persona else None


def _nonempty(value: Any) -> str | None:
    text = str(value or "").strip()
    return text or None


def _new_worker_id() -> str:
    return f"{WORKFLOW_INSTANCE_ID}:{uuid.uuid4().hex[:8]}"


def _command_updated(command_tag: str) -> bool:
    return command_tag.rsplit(" ", 1)[-1] != "0"


async def _refresh_worker_lease(
    pool,
    *,
    run_id: str,
    worker_id: str,
    lease_s: float,
) -> bool:
    result = await pool.execute(
        "UPDATE workflow_runs "
        "SET worker_lease_expires_at = NOW() "
        "    + make_interval(secs => $3::double precision), "
        "    updated_at = NOW() "
        "WHERE run_id = $1 AND worker_id = $2 AND status = 'running'",
        run_id,
        worker_id,
        lease_s,
    )
    return _command_updated(result)


async def _lease_heartbeat(
    pool,
    *,
    run_id: str,
    worker_id: str,
    lease_s: float,
    stop_event: asyncio.Event,
) -> None:
    interval_s = max(min(lease_s / 3.0, 5.0), 0.5)
    while True:
        try:
            await asyncio.wait_for(stop_event.wait(), timeout=interval_s)
            return
        except TimeoutError:
            pass
        if not await _refresh_worker_lease(
            pool,
            run_id=run_id,
            worker_id=worker_id,
            lease_s=lease_s,
        ):
            log.info(
                "workflow_run_lease_lost",
                run_id=run_id,
                worker_id=worker_id,
            )
            return


async def _linked_execution_is_terminal(pool, run_id: str) -> bool:
    row = await pool.fetchrow(
        "SELECT e.status "
        "FROM workflow_checkpoints c "
        "JOIN agent_execution_requests e ON e.execution_id = c.execution_id "
        "WHERE c.run_id = $1 AND c.execution_id IS NOT NULL "
        "ORDER BY c.created_at DESC LIMIT 1",
        run_id,
    )
    if not row:
        return False
    return str(row["status"]) in _EXECUTION_TERMINAL_STATUSES


async def _requeue_expired_running_runs(conn) -> int:
    result = await conn.execute(
        "UPDATE workflow_runs "
        "SET status = 'queued', "
        "    available_at = NOW(), "
        "    worker_id = NULL, "
        "    worker_lease_expires_at = NULL, "
        "    updated_at = NOW() "
        "WHERE status = 'running' "
        "  AND worker_lease_expires_at IS NOT NULL "
        "  AND worker_lease_expires_at <= NOW()",
    )
    if _command_updated(result):
        log.warning("workflow_runs_requeued_after_lease_expiry", updated=result)
    return int(result.rsplit(" ", 1)[-1])


async def do_agent_turn(
    ctx: WorkflowContext,
    *,
    prompt: str | None = None,
    thread_key: str | None = None,
    parts: list[dict[str, Any]] | None = None,
    history_messages: list[dict[str, Any]] | None = None,
    message_id: str | None = None,
    user_id: str | None = None,
    metadata: dict[str, Any] | None = None,
    delivery: Delivery | dict[str, Any] | None = None,
    harness: str | None = None,
    persona: str | None = None,
    agents_md_override: str | None = None,
) -> dict[str, Any]:
    """Orchestrate spawn → message → execute → wait-for-terminal.

    Simplest usage::

        await do_agent_turn(ctx, prompt="Generate the daily digest.")

    ``thread_key`` and ``delivery`` default to values from the workflow run's
    input. ``prompt`` is a shorthand for ``parts=[text_part(prompt)]``.
    ``harness`` and ``persona`` compose orthogonally:
    ``harness="claude-code"`` plus ``persona="invest"`` runs the invest persona
    on the Claude Code runtime, matching ``--invest --claude`` from Slack.
    """

    # Resolve defaults from run input
    run_in = ctx.run_input
    effective_thread_key = (
        thread_key
        or str(run_in.get("thread_key", "")).strip()
        or f"workflow:{ctx.run_id}"
    )
    effective_parts = parts or ([text_part(prompt)] if prompt else [])
    if not effective_parts:
        raise ControlPlaneError(
            "MISSING_PROMPT",
            "do_agent_turn requires prompt or parts",
            422,
        )
    checkpoint_name = ctx._peek_resolved_name("agent_turn")
    step_id = f"wf:{ctx.run_id}:{checkpoint_name}"

    async def _dispatch() -> dict[str, Any]:
        effective_metadata = dict(metadata or run_in.get("metadata") or {})
        effective_metadata["workflow_run_id"] = ctx.run_id
        eff_user_id = user_id or run_in.get("user_id")
        if eff_user_id and not effective_metadata.get("user_id"):
            effective_metadata["user_id"] = eff_user_id
        if isinstance(delivery, Delivery):
            effective_delivery = delivery.to_dict()
        elif delivery:
            effective_delivery = dict(delivery)
        else:
            effective_delivery = dict(run_in.get("delivery") or {})
        effective_history = history_messages or run_in.get("history_messages") or []
        selector = {"persona_id": persona, "harness": harness}
        slackbot_session_id: str | None = None

        try:
            spawn = await spawn_assignment(
                ctx._pool,
                thread_key=effective_thread_key,
                spawn_id=f"{step_id}:spawn",
                harness=harness,
                engine=None,
                persona_id=persona,
                agents_md_override=agents_md_override,
            )
        except Exception as exc:
            try:
                failure_session_id = await slackbot_client.open_agent_session(
                    delivery=effective_delivery,
                    metadata=effective_metadata,
                    thread_key=effective_thread_key,
                    title="Centaur",
                    header=None,
                )
                if failure_session_id:
                    await slackbot_client.session_text(
                        failure_session_id,
                        f"Failed to start the runtime: {exc}",
                    )
                    await slackbot_client.session_done(failure_session_id)
            except Exception:
                log.warning(
                    "workflow_spawn_failure_session_failed",
                    workflow_run_id=ctx.run_id,
                    thread_key=effective_thread_key,
                    exc_info=True,
                )
            raise
        ag = int(spawn["assignment_generation"])

        session_title = await _compute_agent_session_title(
            ctx._pool, effective_thread_key, selector,
        )
        session_header = (
            await _compute_agent_session_header(
                ctx._pool,
                effective_thread_key,
                selector,
            )
            if await _should_show_agent_session_header(
                ctx._pool,
                thread_key=effective_thread_key,
                history_messages=effective_history,
            )
            else None
        )
        slackbot_session_id = await slackbot_client.open_agent_session(
            delivery=effective_delivery,
            metadata=effective_metadata,
            thread_key=effective_thread_key,
            title=session_title,
            header=session_header,
        )
        if slackbot_session_id:
            effective_metadata["slackbot_agent_session_id"] = slackbot_session_id
            effective_metadata["slackbot_live_delivery"] = True

        effective_platform = str(effective_delivery.get("platform") or "").strip().lower()
        if effective_platform == "slack" or effective_thread_key.startswith("slack:"):
            requester_user_id = (
                str(
                    effective_delivery.get("recipient_user_id")
                    or effective_delivery.get("user_id")
                    or effective_metadata.get("user_id")
                    or "",
                ).strip()
                or None
            )
            await _insert_system_message(
                effective_thread_key,
                effective_platform or "slack",
                user_id=requester_user_id,
            )

        if isinstance(effective_history, list):
            backfilled = 0
            skipped = 0
            for item in effective_history:
                if not isinstance(item, dict):
                    skipped += 1
                    continue
                history_message_id = str(
                    item.get("message_id") or item.get("messageId") or "",
                ).strip()
                if not history_message_id or history_message_id == message_id:
                    skipped += 1
                    continue
                history_parts = item.get("parts")
                if not isinstance(history_parts, list):
                    skipped += 1
                    continue
                history_parts = [p for p in history_parts if isinstance(p, dict)]
                if not history_parts:
                    skipped += 1
                    continue
                if await _message_request_exists(
                    ctx._pool,
                    thread_key=effective_thread_key,
                    message_id=history_message_id,
                ):
                    skipped += 1
                    continue
                history_role = str(item.get("role") or "user").strip().lower()
                if history_role not in {"user", "assistant"}:
                    skipped += 1
                    continue
                raw_history_metadata = item.get("metadata")
                history_metadata = dict(raw_history_metadata) if isinstance(
                    raw_history_metadata, dict,
                ) else {}
                history_metadata.setdefault("history_backfill", True)
                history_metadata.setdefault("workflow_run_id", ctx.run_id)
                history_user_id = item.get("user_id") or item.get("userId")
                if history_user_id and not history_metadata.get("user_id"):
                    history_metadata["user_id"] = history_user_id
                history_event = {
                    "type": history_role,
                    "message": {"role": history_role, "content": history_parts},
                }
                try:
                    await append_message(
                        ctx._pool,
                        thread_key=effective_thread_key,
                        assignment_generation=ag,
                        message_id=history_message_id,
                        event=history_event,
                        metadata=history_metadata,
                    )
                    backfilled += 1
                except ControlPlaneError as exc:
                    skipped += 1
                    log.warn(
                        "workflow_history_backfill_skipped",
                        workflow_run_id=ctx.run_id,
                        thread_key=effective_thread_key,
                        message_id=history_message_id,
                        code=exc.code,
                        error=exc.message,
                    )
            if backfilled or skipped:
                log.info(
                    "workflow_history_backfilled",
                    workflow_run_id=ctx.run_id,
                    thread_key=effective_thread_key,
                    backfilled=backfilled,
                    skipped=skipped,
                )

        event = {
            "type": "user",
            "message": {"role": "user", "content": effective_parts},
        }
        await append_message(
            ctx._pool,
            thread_key=effective_thread_key,
            assignment_generation=ag,
            message_id=message_id or f"{step_id}:message",
            event=event,
            metadata=effective_metadata,
        )

        execution = await enqueue_execution(
            ctx._pool,
            thread_key=effective_thread_key,
            assignment_generation=ag,
            execute_id=f"{step_id}:execute",
            harness=harness,
            delivery=effective_delivery,
            metadata=effective_metadata,
        )
        return {
            "execution_id": execution["execution_id"],
            "status": "waiting",
        }

    # Step 1: dispatch (or retrieve cached dispatch result)
    dispatch_result = await ctx.step(
        "agent_turn", _dispatch, step_kind="agent_turn",
    )
    execution_id = dispatch_result.get("execution_id") if isinstance(
        dispatch_result, dict,
    ) else None
    if not execution_id:
        return dispatch_result

    # Step 2: check if execution reached terminal state
    execution = await get_execution(ctx._pool, execution_id)
    if execution and execution.get("status") in _EXECUTION_TERMINAL_STATUSES:
        result_text = str(execution.get("result_text") or "").strip()
        return {
            "execution_id": execution_id,
            "result_text": result_text,
            "execution": execution,
        }

    # Not terminal yet — suspend and wait for notify_execution_terminal
    raise SuspendWorkflow(status="waiting")


async def _message_request_exists(
    pool,
    *,
    thread_key: str,
    message_id: str,
) -> bool:
    row = await pool.fetchrow(
        "SELECT 1 FROM agent_message_requests "
        "WHERE thread_key = $1 AND message_id = $2",
        thread_key,
        message_id,
    )
    return row is not None


# ── Handler discovery ─────────────────────────────────────────────────

@dataclass
class _RegisteredHandler:
    handler: Callable[..., Awaitable[Any]]
    input_cls: type | None  # Optional typed Input dataclass
    source_path: str
    version: str
    schedule: dict[str, Any] | None = None  # Optional SCHEDULE export


# Maps workflow_name → registered handler + optional input class
_WORKFLOW_HANDLERS: dict[str, _RegisteredHandler] = {}

_BUILTIN_WORKFLOWS_PACKAGE = "api.workflows"
_EXTERNAL_WORKFLOWS_NAMESPACE = "centaur.workflows"


def get_workflow_dirs() -> list[Path]:
    """Return external workflow directories from WORKFLOW_DIRS env var."""
    raw = os.getenv("WORKFLOW_DIRS", "")
    dirs: list[Path] = []
    for entry in raw.split(":"):
        entry = entry.strip()
        if entry:
            p = Path(entry)
            if p.is_dir():
                dirs.append(p)
    return dirs


def _coerce_value(value: Any, target_type: type) -> Any:
    """Coerce a raw value to a target type if it's a dataclass."""
    import dataclasses as _dc
    import typing as _typing

    if not _dc.is_dataclass(target_type):
        return value
    if isinstance(value, target_type):
        return value
    if not isinstance(value, dict):
        return value
    field_names = {f.name for f in _dc.fields(target_type)}
    try:
        resolved_hints = _typing.get_type_hints(target_type)
    except Exception:
        resolved_hints = {}
    coerced: dict[str, Any] = {}
    for k, v in value.items():
        if k not in field_names:
            continue
        hint = resolved_hints.get(k)
        if isinstance(hint, type) and _dc.is_dataclass(hint) and isinstance(v, dict):
            coerced[k] = _coerce_value(v, hint)
        else:
            coerced[k] = v
    return target_type(**coerced)


def _coerce_input(
    raw: dict[str, Any], input_cls: type | None,
) -> Any:
    """Coerce a raw dict to a typed dataclass if one is registered."""
    if input_cls is None:
        return raw
    try:
        return _coerce_value(raw, input_cls)
    except Exception:
        log.warning(
            "workflow_input_coerce_failed",
            input_cls=input_cls.__name__,
            exc_info=True,
        )
    return raw


def _load_workflow_file(
    py_file: Path, mod_name: str, discovered: dict[str, str],
) -> None:
    """Load a single workflow handler file into the registry."""
    try:
        if mod_name in sys.modules:
            del sys.modules[mod_name]
        spec = importlib.util.spec_from_file_location(mod_name, py_file)
        if not spec or not spec.loader:
            log.warning("workflow_handler_skip", file=str(py_file), reason="no loader")
            return
        mod = importlib.util.module_from_spec(spec)
        sys.modules[mod_name] = mod
        spec.loader.exec_module(mod)

        wf_name = getattr(mod, "WORKFLOW_NAME", None)
        if not isinstance(wf_name, str):
            log.warning("workflow_handler_skip", file=str(py_file), reason="missing WORKFLOW_NAME")
            return
        wf_handler = getattr(mod, "handler", None)

        # Auto-generate handler from PROMPT + SLACK_CHANNEL exports
        if not callable(wf_handler):
            prompt_val = getattr(mod, "PROMPT", None)
            if not isinstance(prompt_val, str) or not prompt_val.strip():
                log.warning(
                    "workflow_handler_skip",
                    file=str(py_file),
                    reason="missing handler and PROMPT",
                )
                return
            channel_val = getattr(mod, "SLACK_CHANNEL", None)

            async def _auto_handler(
                inp: Any, ctx: WorkflowContext,
                _prompt: str = prompt_val, _channel: str | None = channel_val,
            ) -> dict[str, Any]:
                result = await ctx.agent_turn(_prompt)
                text = result.get("result_text", "")
                channel = (inp.get("slack_channel") if isinstance(inp, dict) else None) or _channel
                if text and channel:
                    await ctx.post_to_slack(channel, text)
                return result

            wf_handler = _auto_handler
        input_cls = getattr(mod, "Input", None)
        schedule = getattr(mod, "SCHEDULE", None)
        if schedule is not None and not isinstance(schedule, dict):
            log.warning("workflow_schedule_skip", file=str(py_file), reason="SCHEDULE must be a dict")
            schedule = None
        # Shorthand: CRON = "...", INTERVAL = 300, SLACK_CHANNEL = "..."
        if schedule is None:
            cron_val = getattr(mod, "CRON", None)
            interval_val = getattr(mod, "INTERVAL", None)
            if isinstance(cron_val, str) and cron_val.strip():
                schedule = {"cron": cron_val.strip()}
            elif isinstance(interval_val, (int, float)) and interval_val > 0:
                schedule = {"interval_seconds": int(interval_val)}
        if schedule is not None:
            slack_ch = getattr(mod, "SLACK_CHANNEL", None)
            if isinstance(slack_ch, str) and slack_ch.strip():
                schedule.setdefault("slack_channel", slack_ch.strip())
        version = hashlib.sha256(py_file.read_bytes()).hexdigest()
        _WORKFLOW_HANDLERS[wf_name] = _RegisteredHandler(
            handler=wf_handler,
            input_cls=input_cls,
            source_path=str(py_file),
            version=version,
            schedule=schedule,
        )
        discovered[wf_name] = str(py_file)
        try:
            register_workflow_webhooks(
                wf_name,
                str(py_file),
                getattr(mod, "WEBHOOKS", None),
            )
        except Exception:
            log.warning(
                "workflow_webhook_registration_failed",
                workflow_name=wf_name,
                file=str(py_file),
                exc_info=True,
            )
    except Exception:
        log.warning("workflow_handler_load_failed", file=str(py_file), exc_info=True)


def discover_workflow_handlers() -> dict[str, str]:
    """Scan built-in and external workflow directories for handler modules.

    Each module must export:
    - ``WORKFLOW_NAME: str`` — the registered workflow name
    - ``handler(params, ctx) -> Any`` — the async handler function

    Optionally export:
    - ``Input`` — a ``@dataclass`` that raw ``input_json`` is
      auto-coerced into before calling the handler.

    Built-in workflows live in ``api/workflows/``.  External directories
    are specified via the ``WORKFLOW_DIRS`` env var (colon-separated paths)
    and are bind-mounted + hot-reloaded like tools.

    Returns a dict of {workflow_name: module_path} for logging.
    """
    global _WORKFLOW_HANDLERS
    _WORKFLOW_HANDLERS.clear()
    clear_webhook_specs()
    discovered: dict[str, str] = {}

    # 1. Built-in workflows (api.workflows package)
    pkg_path = Path(__file__).resolve().parent / "workflows"
    if pkg_path.is_dir():
        for py_file in sorted(pkg_path.glob("*.py")):
            if py_file.name.startswith("_"):
                continue
            mod_name = f"{_BUILTIN_WORKFLOWS_PACKAGE}.{py_file.stem}"
            _load_workflow_file(py_file, mod_name, discovered)

    # 2. External workflow directories (WORKFLOW_DIRS)
    for wf_dir in get_workflow_dirs():
        for py_file in sorted(wf_dir.glob("*.py")):
            if py_file.name.startswith("_"):
                continue
            mod_name = f"{_EXTERNAL_WORKFLOWS_NAMESPACE}.{py_file.stem}"
            _load_workflow_file(py_file, mod_name, discovered)

    log.info(
        "workflow_handlers_discovered",
        workflows=list(discovered.keys()),
        count=len(discovered),
    )
    return discovered


def get_workflow_handler(
    workflow_name: str,
) -> _RegisteredHandler | None:
    return _WORKFLOW_HANDLERS.get(workflow_name)


# ── Schedule specs ────────────────────────────────────────────────────


@dataclass(frozen=True)
class ScheduleSpec:
    schedule_id: str
    workflow_name: str
    schedule_kind: str
    schedule_expr: str | None = None
    timezone: str = "UTC"
    interval_seconds: int | None = None
    catchup_policy: str = "skip"
    input_json: dict[str, Any] = field(default_factory=dict)
    enabled: bool = True


def _split_thread_key(thread_key: str) -> tuple[str, str]:
    parts = thread_key.strip().split(":")
    if len(parts) == 2 and parts[0] and parts[1]:
        return parts[0], parts[1]
    if len(parts) == 3 and parts[1] and parts[2]:
        return parts[1], parts[2]
    raise ControlPlaneError(
        "INVALID_THREAD_KEY",
        f"invalid thread_key format: {thread_key}",
        422,
    )


def _registered_schedule_specs() -> list[ScheduleSpec]:
    """Collect schedule specs from all registered workflow handlers.

    Each handler may export a ``SCHEDULE`` dict.  Minimal example::

        SCHEDULE = {
            "cron": "45 7 * * *",
            "timezone": "America/Los_Angeles",
            "thread_key": os.getenv("MY_THREAD_KEY", ""),
        }

    Supported keys:

    - ``cron`` or ``interval_seconds`` — trigger (required, one of)
    - ``timezone`` — default ``"UTC"``
    - ``enabled`` — default ``True``
    - ``thread_key`` — Slack thread to post to; auto-derives delivery
    - ``slack_channel`` — channel name (e.g. ``"team-updates"``);
      used as delivery channel when no thread_key
    - ``input`` — extra fields merged into the handler Input
    - ``catchup_policy`` — ``"skip"`` (default) or ``"catch_up"``
    - ``no_delivery`` — skip the destination requirement (for workflows
      that write to DB instead of posting to Slack)
    """
    specs: list[ScheduleSpec] = []
    for wf_name, reg in _WORKFLOW_HANDLERS.items():
        sched = reg.schedule
        if not sched:
            continue
        cron_expr = sched.get("cron")
        interval_s = sched.get("interval_seconds")
        if not cron_expr and not interval_s:
            log.warning(
                "workflow_schedule_skip",
                workflow_name=wf_name,
                reason="SCHEDULE must have 'cron' or 'interval_seconds'",
            )
            continue

        enabled_val = sched.get("enabled", True)
        if isinstance(enabled_val, str):
            enabled_val = enabled_val.strip().lower() not in {"0", "false", "no"}

        input_json = dict(sched.get("input") or {})
        input_json.setdefault("metadata", {})
        input_json["metadata"]["source"] = "workflow_schedule"
        input_json["metadata"]["workflow_name"] = wf_name

        # thread_key: explicit > input > convention env var ({NAME}_THREAD_KEY)
        thread_key = (
            sched.get("thread_key")
            or input_json.get("thread_key")
            or os.getenv(f"{wf_name.upper()}_THREAD_KEY", "")
        ).strip()
        slack_channel = (
            str(sched.get("slack_channel", ""))
            or os.getenv(f"{wf_name.upper()}_SLACK_CHANNEL", "")
        ).strip().lstrip("#")

        # If both thread_key and slack_channel are empty, skip —
        # unless the workflow explicitly opts out of delivery.
        if not thread_key and not slack_channel and not sched.get("no_delivery"):
            log.info(
                "workflow_schedule_skip_no_destination",
                workflow_name=wf_name,
            )
            continue

        if thread_key:
            input_json["thread_key"] = thread_key
            # Auto-derive Slack delivery from thread_key
            if "delivery" not in input_json:
                try:
                    channel, thread_ts = _split_thread_key(thread_key)
                    input_json["delivery"] = {
                        "channel": channel,
                        "thread_ts": thread_ts,
                        "platform": "slack",
                    }
                except ControlPlaneError:
                    log.warning(
                        "workflow_schedule_bad_thread_key",
                        workflow_name=wf_name,
                        thread_key=thread_key,
                    )

        # slack_channel: use channel name for delivery (no thread_ts)
        if slack_channel and "delivery" not in input_json:
            input_json["delivery"] = {
                "channel": slack_channel,
                "platform": "slack",
            }

        specs.append(ScheduleSpec(
            schedule_id=sched.get("schedule_id", wf_name),
            workflow_name=wf_name,
            schedule_kind="cron" if cron_expr else "interval",
            schedule_expr=cron_expr,
            timezone=sched.get("timezone", "America/Los_Angeles"),
            interval_seconds=interval_s,
            catchup_policy=sched.get("catchup_policy", "skip"),
            input_json=input_json,
            enabled=bool(enabled_val),
        ))
    return specs


# ── Cron helpers ──────────────────────────────────────────────────────


def _next_cron_time(
    expr: str, timezone: str, *, after: dt.datetime,
) -> dt.datetime:
    """Compute the next cron match after *after* using croniter."""
    from croniter import croniter

    tz = ZoneInfo(timezone)
    base = after.astimezone(tz)
    try:
        cron = croniter(expr, base)
        next_dt = cron.get_next(dt.datetime)
    except (ValueError, KeyError) as exc:
        raise ControlPlaneError(
            "INVALID_SCHEDULE",
            f"invalid cron expression: {expr} ({exc})",
            422,
        ) from exc
    return next_dt.astimezone(dt.timezone.utc)


def _next_schedule_time(
    schedule: dict[str, Any], *, after: dt.datetime,
) -> dt.datetime:
    kind = str(schedule.get("schedule_kind") or "")
    if kind == "interval":
        interval_seconds = int(schedule.get("interval_seconds") or 0)
        if interval_seconds <= 0:
            raise ControlPlaneError(
                "INVALID_SCHEDULE", "interval_seconds must be > 0", 422,
            )
        return after + dt.timedelta(seconds=interval_seconds)
    return _next_cron_time(
        str(schedule.get("schedule_expr") or ""),
        str(schedule.get("timezone") or "UTC"),
        after=after,
    )


def _schedule_due_occurrences(
    schedule: dict[str, Any], *, now: dt.datetime,
) -> tuple[list[dt.datetime], dt.datetime]:
    next_run_at = schedule.get("next_run_at")
    if not isinstance(next_run_at, dt.datetime):
        raise ControlPlaneError(
            "INVALID_SCHEDULE", "schedule missing next_run_at", 422,
        )
    catchup_policy = str(schedule.get("catchup_policy") or "skip")
    occurrences: list[dt.datetime] = []
    cursor = next_run_at
    if catchup_policy == "all":
        while (
            cursor <= now
            and len(occurrences) < WORKFLOW_SCHEDULE_CATCHUP_LIMIT
        ):
            occurrences.append(cursor)
            cursor = _next_schedule_time(schedule, after=cursor)
        return occurrences, cursor

    stale = (now - cursor).total_seconds() > WORKFLOW_SCHEDULE_MISFIRE_GRACE_S
    if cursor <= now and not stale:
        occurrences.append(cursor)
        return occurrences, _next_schedule_time(schedule, after=cursor)

    while cursor <= now:
        cursor = _next_schedule_time(schedule, after=cursor)
    return [], cursor


# ── Run CRUD ──────────────────────────────────────────────────────────


def _workflow_request_hash(
    workflow_name: str, run_input: dict[str, Any],
) -> str:
    hash_input = dict(run_input)
    if workflow_name == "slack_thread_turn":
        hash_input.pop("history_messages", None)
    return request_hash(
        {"workflow_name": workflow_name, "input": hash_input},
    )


async def _insert_workflow_run(
    conn,
    *,
    workflow_name: str,
    run_input: dict[str, Any],
    trigger_key: str | None,
    workflow_version: str,
    workflow_source_path: str | None,
    parent_run_id: str | None,
    root_run_id: str | None,
) -> tuple[str, bool]:
    """Insert a new workflow run (idempotent on trigger_key)."""
    if get_workflow_handler(workflow_name) is None:
        raise ControlPlaneError(
            "UNKNOWN_WORKFLOW",
            f"unknown workflow_name: {workflow_name}",
            422,
        )

    req_hash = _workflow_request_hash(workflow_name, run_input)

    if trigger_key:
        await conn.execute(
            "SELECT pg_advisory_xact_lock(hashtext($1), hashtext($2))",
            workflow_name,
            trigger_key,
        )
        existing = await conn.fetchrow(
            "SELECT run_id, request_hash "
            "FROM workflow_runs "
            "WHERE workflow_name = $1 AND trigger_key = $2",
            workflow_name,
            trigger_key,
        )
        if existing:
            if existing["request_hash"] != req_hash:
                keys = ",".join(sorted(str(key) for key in run_input.keys()))
                log.warning(
                    "workflow_idempotency_payload_mismatch",
                    workflow_name=workflow_name,
                    trigger_key=trigger_key,
                    thread_key=run_input.get("thread_key"),
                    input_keys=keys,
                    existing_request_hash_prefix=str(existing["request_hash"])[:12],
                    request_hash_prefix=req_hash[:12],
                    run_id=str(existing["run_id"]),
                )
                raise ControlPlaneError(
                    "IDEMPOTENCY_PAYLOAD_MISMATCH",
                    "trigger_key was already used with a different payload",
                    409,
                )
            return str(existing["run_id"]), False

    # Extract thread_key from input if present
    thread_key = None
    candidate = run_input.get("thread_key")
    if isinstance(candidate, str) and candidate.strip():
        thread_key = candidate.strip()

    run_id = f"wfr_{uuid.uuid4().hex[:16]}"
    effective_root_run_id = root_run_id or run_id
    await conn.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, workflow_source_path, "
        "request_hash, trigger_key, parent_run_id, root_run_id, "
        "thread_key, status, input_json"
        ") VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'queued', $10::jsonb)",
        run_id,
        workflow_name,
        workflow_version,
        workflow_source_path,
        req_hash,
        trigger_key,
        parent_run_id,
        effective_root_run_id,
        thread_key,
        canonical_json(run_input),
    )
    return run_id, True


def _workflow_waiting_on(
    *,
    step_kind: str | None,
    state: Any,
    execution_id: str | None,
    child_run_id: str | None,
) -> dict[str, Any] | None:
    if step_kind == "sleep" and isinstance(state, str):
        return {"type": "sleep", "until": state}
    if step_kind == "event_wait" and isinstance(state, dict) and state.get("_waiting"):
        return {
            "type": "event",
            "event_type": state.get("event_type"),
            "correlation_id": state.get("correlation_id"),
            "deadline": state.get("deadline"),
        }
    if step_kind == "child_workflow_wait" and isinstance(state, dict) and state.get("_waiting"):
        return {
            "type": "workflow",
            "run_id": state.get("child_run_id") or child_run_id,
            "workflow_name": state.get("workflow_name"),
            "deadline": state.get("deadline"),
        }
    if execution_id:
        return {"type": "execution", "execution_id": execution_id}
    if child_run_id:
        return {"type": "workflow", "run_id": child_run_id}
    return None


async def _latest_checkpoint_summary(
    conn, run_id: str,
) -> dict[str, Any]:
    row = await conn.fetchrow(
        "SELECT checkpoint_name, step_kind, state, execution_id, child_run_id "
        "FROM workflow_checkpoints WHERE run_id = $1 "
        "ORDER BY created_at DESC LIMIT 1",
        run_id,
    )
    if not row:
        return {
            "latest_checkpoint_name": None,
            "latest_step_kind": None,
            "waiting_on": None,
        }
    state = decode_jsonb(row["state"], None)
    return {
        "latest_checkpoint_name": str(row["checkpoint_name"]),
        "latest_step_kind": row["step_kind"],
        "waiting_on": _workflow_waiting_on(
            step_kind=row["step_kind"],
            state=state,
            execution_id=row["execution_id"],
            child_run_id=row["child_run_id"],
        ),
    }


async def _fetch_run_response(
    conn, run_id: str,
) -> dict[str, Any] | None:
    row = await conn.fetchrow(
        "SELECT r.run_id, r.workflow_name, r.workflow_version, "
        "r.workflow_source_path, r.parent_run_id, r.root_run_id, "
        "r.status, r.thread_key, r.output_json, r.error_text, r.created_at, r.started_at, "
        "r.completed_at, "
        "(SELECT execution_id FROM workflow_checkpoints c "
        " WHERE c.run_id = r.run_id AND c.execution_id IS NOT NULL "
        " ORDER BY c.created_at DESC LIMIT 1) AS execution_id "
        "FROM workflow_runs r WHERE r.run_id = $1",
        run_id,
    )
    if not row:
        return None
    latest = await _latest_checkpoint_summary(conn, run_id)
    child_runs_count = int(await conn.fetchval(
        "SELECT COUNT(*)::int FROM workflow_runs WHERE parent_run_id = $1",
        run_id,
    ) or 0)
    return {
        "ok": True,
        "run_id": str(row["run_id"]),
        "workflow_name": str(row["workflow_name"]),
        "workflow_version": str(row["workflow_version"]),
        "workflow_source_path": row["workflow_source_path"],
        "parent_run_id": row["parent_run_id"],
        "root_run_id": row["root_run_id"],
        "status": str(row["status"]),
        "thread_key": row["thread_key"],
        "execution_id": row["execution_id"],
        "output_json": decode_jsonb(row["output_json"], None),
        "error_text": row["error_text"],
        "latest_checkpoint_name": latest["latest_checkpoint_name"],
        "latest_step_kind": latest["latest_step_kind"],
        "waiting_on": (
            latest["waiting_on"]
            if str(row["status"]) in {"waiting", "sleeping"}
            else None
        ),
        "child_runs_count": child_runs_count,
        "created_at": (
            row["created_at"].isoformat() if row["created_at"] else None
        ),
        "started_at": (
            row["started_at"].isoformat() if row["started_at"] else None
        ),
        "completed_at": (
            row["completed_at"].isoformat()
            if row["completed_at"]
            else None
        ),
    }


async def get_workflow_run(
    pool, run_id: str,
) -> dict[str, Any] | None:
    async with pool.acquire() as conn:
        return await _fetch_run_response(conn, run_id)


async def get_workflow_checkpoints(
    pool, run_id: str,
) -> dict[str, Any] | None:
    exists = await pool.fetchval(
        "SELECT EXISTS(SELECT 1 FROM workflow_runs WHERE run_id = $1)",
        run_id,
    )
    if not exists:
        return None
    rows = await pool.fetch(
        "SELECT checkpoint_name, step_kind, execution_id, child_run_id, state, "
        "created_at "
        "FROM workflow_checkpoints WHERE run_id = $1 "
        "ORDER BY created_at ASC",
        run_id,
    )
    items = []
    for row in rows:
        items.append({
            "checkpoint_name": str(row["checkpoint_name"]),
            "step_kind": row["step_kind"],
            "execution_id": row["execution_id"],
            "child_run_id": row["child_run_id"],
            "state": decode_jsonb(row["state"], None),
            "created_at": (
                row["created_at"].isoformat()
                if row["created_at"]
                else None
            ),
        })
    return {"ok": True, "run_id": run_id, "checkpoints": items}


async def list_workflow_runs(
    pool,
    *,
    workflow_name: str | None = None,
    thread_key: str | None = None,
    status: str | None = None,
    parent_run_id: str | None = None,
    limit: int = 50,
) -> dict[str, Any]:
    rows = await pool.fetch(
        "SELECT r.run_id, r.workflow_name, r.workflow_version, "
        "r.workflow_source_path, r.parent_run_id, r.root_run_id, "
        "r.status, r.thread_key, r.error_text, r.created_at, r.started_at, r.completed_at, "
        "(SELECT execution_id FROM workflow_checkpoints c "
        " WHERE c.run_id = r.run_id AND c.execution_id IS NOT NULL "
        " ORDER BY c.created_at DESC LIMIT 1) AS execution_id, "
        "(SELECT checkpoint_name FROM workflow_checkpoints c "
        " WHERE c.run_id = r.run_id ORDER BY c.created_at DESC LIMIT 1) AS latest_checkpoint_name, "
        "(SELECT step_kind FROM workflow_checkpoints c "
        " WHERE c.run_id = r.run_id ORDER BY c.created_at DESC LIMIT 1) AS latest_step_kind, "
        "(SELECT COUNT(*)::int FROM workflow_runs child WHERE child.parent_run_id = r.run_id) AS child_runs_count "
        "FROM workflow_runs r "
        "WHERE ($1::text IS NULL OR r.workflow_name = $1) "
        "  AND ($2::text IS NULL OR r.thread_key = $2) "
        "  AND ($3::text IS NULL OR r.status = $3) "
        "  AND ($4::text IS NULL OR r.parent_run_id = $4) "
        "ORDER BY r.created_at DESC "
        "LIMIT $5",
        workflow_name,
        thread_key,
        status,
        parent_run_id,
        max(1, min(limit, 200)),
    )
    items = []
    for row in rows:
        items.append({
            "run_id": str(row["run_id"]),
            "workflow_name": str(row["workflow_name"]),
            "workflow_version": str(row["workflow_version"]),
            "workflow_source_path": row["workflow_source_path"],
            "parent_run_id": row["parent_run_id"],
            "root_run_id": row["root_run_id"],
            "status": str(row["status"]),
            "thread_key": row["thread_key"],
            "execution_id": row["execution_id"],
            "error_text": row["error_text"],
            "latest_checkpoint_name": row["latest_checkpoint_name"],
            "latest_step_kind": row["latest_step_kind"],
            "child_runs_count": int(row["child_runs_count"] or 0),
            "created_at": (
                row["created_at"].isoformat()
                if row["created_at"]
                else None
            ),
            "started_at": (
                row["started_at"].isoformat()
                if row["started_at"]
                else None
            ),
            "completed_at": (
                row["completed_at"].isoformat()
                if row["completed_at"]
                else None
            ),
        })
    return {"ok": True, "items": items}


async def create_workflow_run(
    pool,
    *,
    workflow_name: str,
    run_input: dict[str, Any],
    trigger_key: str | None,
    eager_start: bool,
    parent_run_id: str | None = None,
) -> dict[str, Any]:
    registered = get_workflow_handler(workflow_name)
    if registered is None:
        raise ControlPlaneError(
            "UNKNOWN_WORKFLOW",
            f"unknown workflow_name: {workflow_name}",
            422,
        )
    async with pool.acquire() as conn:
        async with conn.transaction():
            root_run_id: str | None = None
            if parent_run_id:
                parent_row = await conn.fetchrow(
                    "SELECT run_id, root_run_id FROM workflow_runs WHERE run_id = $1",
                    parent_run_id,
                )
                if not parent_row:
                    raise ControlPlaneError(
                        "PARENT_WORKFLOW_RUN_NOT_FOUND",
                        f"parent workflow run not found: {parent_run_id}",
                        409,
                    )
                root_run_id = str(
                    parent_row["root_run_id"] or parent_row["run_id"],
                )
            run_id, inserted = await _insert_workflow_run(
                conn,
                workflow_name=workflow_name,
                run_input=run_input,
                trigger_key=trigger_key,
                workflow_version=registered.version,
                workflow_source_path=registered.source_path,
                parent_run_id=parent_run_id,
                root_run_id=root_run_id,
            )

    if inserted:
        record_workflow_run_enqueued(workflow_name)
        log.info(
            "workflow_run_enqueued",
            run_id=run_id,
            workflow_name=workflow_name,
            trigger_key=trigger_key,
            eager_start=eager_start,
        )

    _workflow_wake.set()

    response = await get_workflow_run(pool, run_id)
    if response is None:
        raise ControlPlaneError(
            "WORKFLOW_RUN_MISSING",
            "workflow run was not found after creation",
            500,
        )
    response["idempotent"] = not inserted
    return response


async def cancel_workflow_run(
    pool,
    run_id: str,
) -> dict[str, Any] | None:
    """Cancel a workflow run. Idempotent for terminal runs."""
    execution_ids: list[str] = []
    async with pool.acquire() as conn:
        async with conn.transaction():
            row = await conn.fetchrow(
                "SELECT run_id, status, worker_id "
                "FROM workflow_runs WHERE run_id = $1 FOR UPDATE",
                run_id,
            )
            if not row:
                return None
            status = str(row["status"])
            if status in ("completed", "failed", "cancelled"):
                return await _fetch_run_response(conn, run_id)
            execution_ids = [
                str(r["execution_id"])
                for r in await conn.fetch(
                    "SELECT DISTINCT execution_id FROM workflow_checkpoints "
                    "WHERE run_id = $1 AND execution_id IS NOT NULL",
                    run_id,
                )
            ]
            await conn.execute(
                "UPDATE workflow_runs "
                "SET status = 'cancelled', "
                "    worker_id = NULL, "
                "    worker_lease_expires_at = NULL, "
                "    completed_at = NOW(), "
                "    updated_at = NOW() "
                "WHERE run_id = $1",
                run_id,
            )
            result = await _fetch_run_response(conn, run_id)
    for execution_id in execution_ids:
        try:
            await cancel_execution(pool, execution_id)
        except Exception:
            log.warning(
                "workflow_cancel_linked_execution_failed",
                run_id=run_id,
                execution_id=execution_id,
                exc_info=True,
            )
    await notify_workflow_run_terminal(pool, run_id)
    return result


# ── Worker: claim / execute / complete ────────────────────────────────


async def _claim_run(pool) -> dict[str, Any] | None:
    """Claim the next available workflow run."""
    worker_id = _new_worker_id()
    async with pool.acquire() as conn:
        async with conn.transaction():
            await _requeue_expired_running_runs(conn)
            row = await conn.fetchrow(
                "WITH candidate AS ("
                "  SELECT run_id FROM workflow_runs "
                "  WHERE status IN ('queued', 'sleeping', 'waiting') "
                "    AND available_at <= NOW() "
                "    AND (worker_lease_expires_at IS NULL "
                "         OR worker_lease_expires_at <= NOW()) "
                "  ORDER BY "
                "    CASE status "
                "      WHEN 'queued' THEN 0 "
                "      WHEN 'waiting' THEN 1 "
                "      ELSE 2 "
                "    END, "
                "    created_at ASC "
                "  LIMIT 1 "
                "  FOR UPDATE SKIP LOCKED"
                ") "
                "UPDATE workflow_runs r "
                "SET worker_id = $1, "
                "    status = 'running', "
                "    worker_lease_expires_at = NOW() "
                "        + make_interval("
                "            secs => $2::double precision"
                "        ), "
                "    started_at = COALESCE(r.started_at, NOW()), "
                "    updated_at = NOW() "
                "FROM candidate c "
                "WHERE r.run_id = c.run_id "
                "RETURNING r.run_id, r.workflow_name, r.input_json, "
                "          r.status, r.created_at, r.worker_id",
                worker_id,
                float(WORKFLOW_WORKER_LEASE_S),
            )
            return dict(row) if row else None


async def _load_checkpoints(
    pool, run_id: str,
) -> dict[str, Any]:
    """Bulk-load all checkpoints for a run into a dict."""
    rows = await pool.fetch(
        "SELECT checkpoint_name, state "
        "FROM workflow_checkpoints WHERE run_id = $1",
        run_id,
    )
    return {
        str(row["checkpoint_name"]): decode_jsonb(row["state"], None)
        for row in rows
    }


async def _execute_run(pool, run_id: str) -> None:
    """Claim a specific run by ID and execute it."""
    worker_id = _new_worker_id()
    async with pool.acquire() as conn:
        async with conn.transaction():
            row = await conn.fetchrow(
                "UPDATE workflow_runs "
                "SET worker_id = $2, "
                "    status = 'running', "
                "    worker_lease_expires_at = NOW() "
                "        + make_interval("
                "            secs => $3::double precision"
                "        ), "
                "    started_at = COALESCE(started_at, NOW()), "
                "    updated_at = NOW() "
                "WHERE run_id = $1 "
                "  AND status = 'queued' "
                "  AND available_at <= NOW() "
                "RETURNING run_id, workflow_name, input_json, status, worker_id",
                run_id,
                worker_id,
                float(WORKFLOW_WORKER_LEASE_S),
            )
    if row:
        await _run_handler(pool, dict(row))


async def _run_handler(pool, run_row: dict[str, Any]) -> None:
    """Execute the handler for a claimed run."""
    _start = time.monotonic()
    run_id = str(run_row["run_id"])
    workflow_name = str(run_row["workflow_name"])
    worker_id = str(run_row.get("worker_id") or "")
    run_input = decode_jsonb(run_row.get("input_json"), {})
    if not isinstance(run_input, dict):
        run_input = {}
    created_at = run_row.get("created_at")
    if created_at and isinstance(created_at, dt.datetime):
        aware = (
            created_at.replace(tzinfo=dt.timezone.utc)
            if created_at.tzinfo is None
            else created_at
        )
        queue_delay = (dt.datetime.now(dt.timezone.utc) - aware).total_seconds()
    else:
        queue_delay = 0.0
    record_workflow_run_claimed(workflow_name, max(queue_delay, 0.0))
    log.info(
        "workflow_run_claimed",
        run_id=run_id,
        workflow_name=workflow_name,
        queue_delay_s=round(queue_delay, 3),
    )

    registered = get_workflow_handler(workflow_name)
    if registered is None:
        updated = await pool.execute(
            "UPDATE workflow_runs "
            "SET status = 'failed', "
            "    error_text = $2, "
            "    worker_id = NULL, "
            "    worker_lease_expires_at = NULL, "
            "    completed_at = NOW(), "
            "    updated_at = NOW() "
            "WHERE run_id = $1",
            run_id,
            f"unknown workflow: {workflow_name}",
        )
        if _command_updated(updated):
            await notify_workflow_run_terminal(pool, run_id)
        return

    checkpoints = await _load_checkpoints(pool, run_id)
    ctx = WorkflowContext(
        pool=pool,
        run_id=run_id,
        checkpoints=checkpoints,
        lease_s=WORKFLOW_WORKER_LEASE_S,
        worker_id=worker_id,
        run_input=run_input if isinstance(run_input, dict) else {},
    )
    params = _coerce_input(run_input, registered.input_cls)
    heartbeat_stop = asyncio.Event()
    heartbeat_task = asyncio.create_task(
        _lease_heartbeat(
            pool,
            run_id=run_id,
            worker_id=worker_id,
            lease_s=WORKFLOW_WORKER_LEASE_S,
            stop_event=heartbeat_stop,
        ),
        name=f"workflow-lease-{run_id}",
    )

    try:
        result = await registered.handler(params, ctx)
        # Handler completed normally → mark run as completed
        updated = await pool.execute(
            "UPDATE workflow_runs "
            "SET status = 'completed', "
            "    output_json = $2::jsonb, "
            "    worker_id = NULL, "
            "    worker_lease_expires_at = NULL, "
            "    completed_at = NOW(), "
            "    updated_at = NOW() "
            "WHERE run_id = $1 AND worker_id = $3 AND status = 'running'",
            run_id,
            canonical_json(result),
            worker_id,
        )
        if not _command_updated(updated):
            log.info(
                "workflow_run_terminal_write_skipped",
                run_id=run_id,
                workflow_name=workflow_name,
                state="completed",
            )
            return
        _duration = time.monotonic() - _start
        record_workflow_run_terminal(workflow_name, "completed", _duration)
        await notify_workflow_run_terminal(pool, run_id)
        log.info(
            "workflow_run_completed",
            run_id=run_id,
            workflow_name=workflow_name,
        )

    except SuspendWorkflow as exc:
        # Handler suspended — set status and available_at for re-wake.
        now = dt.datetime.now(dt.timezone.utc)
        available_at = exc.available_at
        linked_terminal = exc.status == "waiting" and await _linked_execution_is_terminal(
            pool, run_id,
        )
        if linked_terminal:
            # Linked execution finished — wake immediately so the handler
            # can pick up the result.
            available_at = now
        else:
            # Enforce a minimum backoff so we never hot-loop on runs whose
            # available_at is already in the past (e.g. elapsed deadline on
            # a child that is still running).
            min_available = now + dt.timedelta(seconds=WORKFLOW_RESUSPEND_BACKOFF_S)
            if available_at < min_available:
                available_at = min_available
        updated = await pool.execute(
            "UPDATE workflow_runs "
            "SET status = $2, "
            "    available_at = $3, "
            "    worker_id = NULL, "
            "    worker_lease_expires_at = NULL, "
            "    updated_at = NOW() "
            "WHERE run_id = $1 AND worker_id = $4 AND status = 'running'",
            run_id,
            exc.status,
            available_at,
            worker_id,
        )
        if not _command_updated(updated):
            log.info(
                "workflow_run_terminal_write_skipped",
                run_id=run_id,
                workflow_name=workflow_name,
                state=exc.status,
            )
            return
        log.info(
            "workflow_run_suspended",
            run_id=run_id,
            workflow_name=workflow_name,
            status=exc.status,
            available_at=available_at.isoformat(),
        )

    except CancelledWorkflow:
        updated = await pool.execute(
            "UPDATE workflow_runs "
            "SET status = 'cancelled', "
            "    worker_id = NULL, "
            "    worker_lease_expires_at = NULL, "
            "    completed_at = NOW(), "
            "    updated_at = NOW() "
            "WHERE run_id = $1 AND worker_id = $2 AND status = 'running'",
            run_id,
            worker_id,
        )
        if not _command_updated(updated):
            log.info(
                "workflow_run_terminal_write_skipped",
                run_id=run_id,
                workflow_name=workflow_name,
                state="cancelled",
            )
            return
        _duration = time.monotonic() - _start
        record_workflow_run_terminal(workflow_name, "cancelled", _duration)
        await notify_workflow_run_terminal(pool, run_id)
        log.info(
            "workflow_run_cancelled",
            run_id=run_id,
            workflow_name=workflow_name,
        )

    except ControlPlaneError as exc:
        updated = await pool.execute(
            "UPDATE workflow_runs "
            "SET status = 'failed', "
            "    output_json = $2::jsonb, "
            "    error_text = $3, "
            "    worker_id = NULL, "
            "    worker_lease_expires_at = NULL, "
            "    completed_at = NOW(), "
            "    updated_at = NOW() "
            "WHERE run_id = $1 AND worker_id = $4 AND status = 'running'",
            run_id,
            canonical_json({"code": exc.code}),
            exc.message,
            worker_id,
        )
        if _command_updated(updated):
            _duration = time.monotonic() - _start
            record_workflow_run_terminal(workflow_name, "failed", _duration)
            await notify_workflow_run_terminal(pool, run_id)
            log.warning(
                "workflow_run_failed",
                run_id=run_id,
                workflow_name=workflow_name,
                code=exc.code,
                error=exc.message,
            )
        else:
            log.info(
                "workflow_run_terminal_write_skipped",
                run_id=run_id,
                workflow_name=workflow_name,
                state="failed",
            )

    except Exception as exc:
        log.warning(
            "workflow_run_failed",
            run_id=run_id,
            workflow_name=workflow_name,
            exc_info=True,
        )
        updated = await pool.execute(
            "UPDATE workflow_runs "
            "SET status = 'failed', "
            "    error_text = $2, "
            "    worker_id = NULL, "
            "    worker_lease_expires_at = NULL, "
            "    completed_at = NOW(), "
            "    updated_at = NOW() "
            "WHERE run_id = $1 AND worker_id = $3 AND status = 'running'",
            run_id,
            str(exc)[:2000],
            worker_id,
        )
        if _command_updated(updated):
            _duration = time.monotonic() - _start
            record_workflow_run_terminal(workflow_name, "failed", _duration)
            await notify_workflow_run_terminal(pool, run_id)
        else:
            log.info(
                "workflow_run_terminal_write_skipped",
                run_id=run_id,
                workflow_name=workflow_name,
                state="failed",
            )
    finally:
        heartbeat_stop.set()
        heartbeat_task.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await heartbeat_task


# ── Scheduler ─────────────────────────────────────────────────────────


async def _tick_workflow_schedules(
    pool, now: dt.datetime | None = None,
) -> int:
    current_now = now or dt.datetime.now(dt.timezone.utc)
    created = 0
    async with pool.acquire() as conn:
        async with conn.transaction():
            rows = await conn.fetch(
                "SELECT schedule_id, workflow_name, schedule_kind, "
                "schedule_expr, timezone, interval_seconds, "
                "catchup_policy, input_json, enabled, next_run_at "
                "FROM workflow_schedules "
                "WHERE enabled = TRUE AND next_run_at <= $1 "
                "ORDER BY next_run_at ASC "
                "FOR UPDATE SKIP LOCKED",
                current_now,
            )
            for row in rows:
                schedule = dict(row)
                schedule["input_json"] = decode_jsonb(
                    schedule.get("input_json"), {},
                )
                occurrences, next_run_at = _schedule_due_occurrences(
                    schedule, now=current_now,
                )
                last_run_at: dt.datetime | None = None
                last_trigger_key: str | None = None
                for scheduled_at in occurrences:
                    trigger_key = (
                        f"schedule:{schedule['schedule_id']}"
                        f":{int(scheduled_at.timestamp())}"
                    )
                    registered = get_workflow_handler(
                        str(schedule["workflow_name"]),
                    )
                    if registered is None:
                        raise ControlPlaneError(
                            "UNKNOWN_WORKFLOW",
                            f"unknown workflow_name: {schedule['workflow_name']}",
                            422,
                        )
                    _, inserted = await _insert_workflow_run(
                        conn,
                        workflow_name=str(schedule["workflow_name"]),
                        run_input=dict(schedule["input_json"]),
                        trigger_key=trigger_key,
                        workflow_version=registered.version,
                        workflow_source_path=registered.source_path,
                        parent_run_id=None,
                        root_run_id=None,
                    )
                    if inserted:
                        created += 1
                    last_run_at = scheduled_at
                    last_trigger_key = trigger_key

                await conn.execute(
                    "UPDATE workflow_schedules "
                    "SET next_run_at = $2, "
                    "    last_run_at = COALESCE($3, last_run_at), "
                    "    last_trigger_key = COALESCE($4, last_trigger_key),"
                    "    updated_at = NOW() "
                    "WHERE schedule_id = $1",
                    str(schedule["schedule_id"]),
                    next_run_at,
                    last_run_at,
                    last_trigger_key,
                )

    if created:
        _workflow_wake.set()
    return created


async def _tick_workflow_schedules_if_due(pool) -> None:
    global _last_tick_at
    now = asyncio.get_running_loop().time()
    if now - _last_tick_at < WORKFLOW_SCHEDULE_TICK_INTERVAL_S:
        return
    async with _tick_lock:
        now = asyncio.get_running_loop().time()
        if now - _last_tick_at < WORKFLOW_SCHEDULE_TICK_INTERVAL_S:
            return
        try:
            await _tick_workflow_schedules(pool)
        finally:
            _last_tick_at = now


# ── Notification from execution worker ────────────────────────────────


async def notify_execution_terminal(
    pool, execution_id: str,
) -> bool:
    """Wake a workflow run when its linked execution reaches terminal.

    Called from ``_mark_execution_terminal`` in runtime_control.py.
    Finds the checkpoint that references this execution_id and sets the
    owning run to re-claimable.
    """
    row = await pool.fetchrow(
        "SELECT c.run_id FROM workflow_checkpoints c "
        "JOIN workflow_runs r ON r.run_id = c.run_id "
        "WHERE c.execution_id = $1 "
        "  AND r.status IN ('waiting', 'sleeping')",
        execution_id,
    )
    if row:
        await pool.execute(
            "UPDATE workflow_runs "
            "SET available_at = NOW(), updated_at = NOW() "
            "WHERE run_id = $1 "
            "  AND status IN ('waiting', 'sleeping')",
            str(row["run_id"]),
        )
        wake_workflow_worker()
        return True
    return False


async def notify_workflow_run_terminal(
    pool, child_run_id: str,
) -> bool:
    rows = await pool.fetch(
        "SELECT DISTINCT r.run_id "
        "FROM workflow_checkpoints c "
        "JOIN workflow_runs r ON r.run_id = c.run_id "
        "WHERE c.child_run_id = $1 "
        "  AND r.status IN ('waiting', 'sleeping')",
        child_run_id,
    )
    woken = 0
    for row in rows:
        await pool.execute(
            "UPDATE workflow_runs "
            "SET available_at = NOW(), updated_at = NOW() "
            "WHERE run_id = $1 "
            "  AND status IN ('waiting', 'sleeping')",
            str(row["run_id"]),
        )
        woken += 1
    if woken:
        wake_workflow_worker()
        return True
    return False


async def send_workflow_event(
    pool,
    *,
    event_type: str,
    correlation_id: str,
    payload: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Deliver an external event to any waiting workflow runs.

    First-emit-wins: if the event already exists, the payload is NOT
    updated.  Returns the event row and whether any runs were woken.
    """
    effective_payload = payload or {}
    await pool.execute(
        "INSERT INTO workflow_events (event_type, correlation_id, payload) "
        "VALUES ($1, $2, $3::jsonb) "
        "ON CONFLICT (event_type, correlation_id) DO NOTHING",
        event_type,
        correlation_id,
        canonical_json(effective_payload),
    )
    record_workflow_event_sent(event_type)

    # Find waiting runs whose event_wait checkpoint matches this event
    rows = await pool.fetch(
        "SELECT DISTINCT r.run_id "
        "FROM workflow_checkpoints c "
        "JOIN workflow_runs r ON r.run_id = c.run_id "
        "WHERE c.step_kind = 'event_wait' "
        "  AND r.status IN ('waiting', 'sleeping') "
        "  AND c.state->>'_waiting' = 'true' "
        "  AND c.state->>'event_type' = $1 "
        "  AND c.state->>'correlation_id' = $2",
        event_type,
        correlation_id,
    )
    woken = 0
    for row in rows:
        await pool.execute(
            "UPDATE workflow_runs "
            "SET available_at = NOW(), updated_at = NOW() "
            "WHERE run_id = $1 "
            "  AND status IN ('waiting', 'sleeping')",
            row["run_id"],
        )
        woken += 1
    if woken:
        wake_workflow_worker()
    return {
        "ok": True,
        "event_type": event_type,
        "correlation_id": correlation_id,
        "runs_woken": woken,
    }


# ── Worker loop ───────────────────────────────────────────────────────


async def _workflow_worker_loop(pool) -> None:
    while True:
        try:
            await _tick_workflow_schedules_if_due(pool)
            run_row = await _claim_run(pool)
            if run_row is None:
                _workflow_wake.clear()
                try:
                    await asyncio.wait_for(
                        _workflow_wake.wait(),
                        timeout=WORKFLOW_RECONCILE_INTERVAL_S,
                    )
                except TimeoutError:
                    pass
                continue
            await _run_handler(pool, run_row)
            # Yield to the event loop after each handler run so execution
            # workers and other asyncio tasks are not starved.
            await asyncio.sleep(0)
        except asyncio.CancelledError:
            raise
        except Exception:
            log.warning("workflow_worker_tick_error", exc_info=True)
            await asyncio.sleep(0.5)


async def start_workflow_worker(pool) -> None:
    global _workflow_tasks, _last_tick_at
    if any(not task.done() for task in _workflow_tasks):
        return
    _last_tick_at = 0.0
    discover_workflow_handlers()
    _workflow_tasks = [
        asyncio.create_task(
            _workflow_worker_loop(pool),
            name=f"workflow-worker-{index + 1}",
        )
        for index in range(WORKFLOW_WORKER_CONCURRENCY)
    ]


async def stop_workflow_worker() -> None:
    global _workflow_tasks, _last_tick_at
    if not _workflow_tasks:
        return
    tasks = _workflow_tasks
    _workflow_tasks = []
    _last_tick_at = 0.0
    for task in tasks:
        task.cancel()
    for task in tasks:
        with contextlib.suppress(asyncio.CancelledError):
            await task


def wake_workflow_worker() -> None:
    _workflow_wake.set()


# ── Schedule sync ─────────────────────────────────────────────────────


async def sync_registered_workflow_schedules(pool) -> None:
    now = dt.datetime.now(dt.timezone.utc)
    specs = _registered_schedule_specs()
    active_schedule_ids = {spec.schedule_id for spec in specs}
    async with pool.acquire() as conn:
        async with conn.transaction():
            # Disable schedules that are no longer registered in code/config so
            # removed env-driven jobs do not keep firing indefinitely.
            if active_schedule_ids:
                await conn.execute(
                    "UPDATE workflow_schedules "
                    "SET enabled = FALSE, updated_at = NOW() "
                    "WHERE enabled = TRUE "
                    "  AND NOT (schedule_id = ANY($1::text[]))",
                    list(active_schedule_ids),
                )
            else:
                await conn.execute(
                    "UPDATE workflow_schedules "
                    "SET enabled = FALSE, updated_at = NOW() "
                    "WHERE enabled = TRUE",
                )

            for spec in specs:
                schedule_row = {
                    "schedule_kind": spec.schedule_kind,
                    "schedule_expr": spec.schedule_expr,
                    "timezone": spec.timezone,
                    "interval_seconds": spec.interval_seconds,
                }
                next_run_at = _next_schedule_time(
                    schedule_row, after=now,
                )
                existing = await conn.fetchrow(
                    "SELECT workflow_name, schedule_kind, schedule_expr, "
                    "timezone, interval_seconds, catchup_policy, "
                    "input_json, enabled "
                    "FROM workflow_schedules "
                    "WHERE schedule_id = $1",
                    spec.schedule_id,
                )
                if existing:
                    current_input = decode_jsonb(existing["input_json"], {})
                    spec_changed = any([
                        str(existing["workflow_name"]) != spec.workflow_name,
                        str(existing["schedule_kind"]) != spec.schedule_kind,
                        existing["schedule_expr"] != spec.schedule_expr,
                        str(existing["timezone"]) != spec.timezone,
                        existing["interval_seconds"] != spec.interval_seconds,
                        str(existing["catchup_policy"]) != spec.catchup_policy,
                        current_input != spec.input_json,
                        bool(existing["enabled"]) != spec.enabled,
                    ])
                    if spec_changed:
                        await conn.execute(
                            "UPDATE workflow_schedules "
                            "SET workflow_name = $2, "
                            "    schedule_kind = $3, "
                            "    schedule_expr = $4, "
                            "    timezone = $5, "
                            "    interval_seconds = $6, "
                            "    catchup_policy = $7, "
                            "    input_json = $8::jsonb, "
                            "    enabled = $9, "
                            "    next_run_at = $10, "
                            "    updated_at = NOW() "
                            "WHERE schedule_id = $1",
                            spec.schedule_id,
                            spec.workflow_name,
                            spec.schedule_kind,
                            spec.schedule_expr,
                            spec.timezone,
                            spec.interval_seconds,
                            spec.catchup_policy,
                            canonical_json(spec.input_json),
                            spec.enabled,
                            next_run_at,
                        )
                else:
                    await conn.execute(
                        "INSERT INTO workflow_schedules ("
                        "schedule_id, workflow_name, schedule_kind, "
                        "schedule_expr, timezone, interval_seconds, "
                        "catchup_policy, input_json, enabled, next_run_at"
                        ") VALUES ("
                        "$1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9, $10"
                        ")",
                        spec.schedule_id,
                        spec.workflow_name,
                        spec.schedule_kind,
                        spec.schedule_expr,
                        spec.timezone,
                        spec.interval_seconds,
                        spec.catchup_policy,
                        canonical_json(spec.input_json),
                        spec.enabled,
                        next_run_at,
                    )
