"""Sandbox backend interface — pluggable agent execution environments."""

from __future__ import annotations

import abc
import asyncio
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from typing import Any


@dataclass
class RuntimeState:
    """Process-local ephemeral state for a running sandbox.

    Keyed by sandbox_id in agent._runtime. Contains backend stream handles
    and turn bookkeeping. Fully async -- no threads or queues.
    """

    turn_counter: int = 0
    stdout_stream: Any = None  # backend stream for reading stdout
    stdin_stream: Any = None  # backend stream for writing stdin
    attach_context: Any = None  # backend-specific context manager for attach sessions
    prefetched_stdout: list[str] | None = (
        None  # buffered lines loaded before live attach
    )
    stdout_read_lock: asyncio.Lock = field(default_factory=asyncio.Lock)
    last_result: str | None = None


@dataclass
class SandboxSession:
    """Represents a running sandbox."""

    sandbox_id: str  # backend-specific ID, such as a Kubernetes Pod name
    thread_key: str
    harness: str
    engine: str
    started_at: float = 0.0
    backend_name: str = ""  # "kubernetes", etc.
    db_state: str = ""
    agent_thread_id: str = ""
    last_delivered_id: str = ""
    inflight_turn_id: str = ""
    inflight_turn_input: dict | None = None
    inflight_attempts: int = 0
    last_result: str = ""
    trace_id: str = ""


class SandboxBackend(abc.ABC):
    """ABC for agent sandbox orchestration backends."""

    @property
    @abc.abstractmethod
    def name(self) -> str:
        """Short identifier for this backend, for example 'kubernetes'."""

    @property
    def supports_warm_pool(self) -> bool:
        """Whether this backend supports pre-warming sandboxes."""
        return False

    @abc.abstractmethod
    async def create(
        self,
        thread_key: str,
        harness: str,
        engine: str,
        *,
        persona: str | None = None,
        repo: str | None = None,
        warm: bool = False,
        model: str | None = None,
        resume_thread_id: str | None = None,
        trace_id: str | None = None,
    ) -> SandboxSession:
        """Create and start a new sandbox. Block until ready."""

    @abc.abstractmethod
    async def attach(self, session: SandboxSession, *, logs: bool = False) -> None:
        """Attach stdin/stdout streams to the sandbox.

        If logs=True, include buffered output from before the attach point.
        """

    @abc.abstractmethod
    async def write_stdin(self, session: SandboxSession, obj: dict) -> None:
        """Write an NDJSON line to the sandbox's stdin."""

    @abc.abstractmethod
    async def stream_stdout(self, session: SandboxSession) -> AsyncIterator[str]:
        """Yield stdout lines from the sandbox asynchronously."""
        yield  # pragma: no cover — abstract async generator

    @abc.abstractmethod
    async def stop(self, session: SandboxSession) -> None:
        """Stop and clean up the sandbox."""

    @abc.abstractmethod
    async def status(self, session: SandboxSession) -> str:
        """Return sandbox status: 'running', 'stopped', 'gone', etc."""

    async def close_streams(self, session: SandboxSession) -> None:
        """Close any open streams. Default: no-op."""

    async def close_stdin(self, session: SandboxSession) -> None:
        """Close only the stdin stream. Default: fall back to closing all streams."""
        await self.close_streams(session)

    async def reattach_stdin(self, session: SandboxSession) -> None:
        """Re-open stdin after a broken pipe. Default: re-attach the full session."""
        await self.close_streams(session)
        await self.attach(session)

    async def exec_run(
        self,
        sandbox_id: str,
        cmd: list[str],
        *,
        environment: dict | None = None,
        user: str = "",
    ) -> tuple[int, bytes]:
        """Run a command inside a sandbox and return (exit_code, output)."""
        raise NotImplementedError

    async def status_by_id(self, sandbox_id: str) -> str:
        """Check sandbox status by ID (no session needed)."""
        raise NotImplementedError

    async def stop_by_id(self, sandbox_id: str) -> None:
        """Stop and remove a sandbox by ID (no session needed)."""
        raise NotImplementedError

    async def pause_by_id(self, sandbox_id: str) -> None:
        """Pause a sandbox while retaining resumable state."""
        await self.stop_by_id(sandbox_id)

    async def resume_by_id(self, sandbox_id: str) -> None:
        """Resume a paused sandbox if the backend supports it."""
        return None

    async def interrupt_by_id(self, sandbox_id: str) -> None:
        """Interrupt the active turn for a sandbox without destroying it."""
        raise NotImplementedError

    async def rename_by_id(self, sandbox_id: str, new_name: str) -> None:
        """Rename a sandbox by ID."""
        raise NotImplementedError

    async def refresh_token_by_id(self, sandbox_id: str, new_token: str) -> None:
        """Write a fresh API token into a running sandbox."""
        raise NotImplementedError

    async def recover_warm(self, pool_harness: str) -> list[SandboxSession]:
        """Discover warm (pre-created, unclaimed) sandboxes. Default: empty."""
        return []
