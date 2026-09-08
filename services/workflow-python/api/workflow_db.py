"""Workflow database access and explicitly invoked SQLx migrations."""

from __future__ import annotations

import asyncio
import contextlib
import json
import math
import os
from pathlib import Path
from typing import Any


class MigrationError(RuntimeError):
    """The SQLx workflow migration runner failed."""


class WorkflowDatabase:
    """A thin handle over the host's asyncpg pool; queries are not checkpointed."""

    def __init__(self, pool: Any, *, source_path: str | Path | None = None) -> None:
        self._pool = pool
        self._source_path = (
            Path(source_path).resolve() if source_path is not None else None
        )

    def _require_pool(self) -> Any:
        if self._pool is None:
            raise RuntimeError(
                "Workflow database is unavailable; configure DATABASE_URL"
            )
        return self._pool

    def acquire(self, *, timeout: float | None = None) -> Any:
        """Acquire an asyncpg connection, including for multi-query transactions."""
        return self._require_pool().acquire(timeout=timeout)

    async def execute(self, query: str, *args: Any, **kwargs: Any) -> Any:
        return await self._require_pool().execute(query, *args, **kwargs)

    async def fetch(self, query: str, *args: Any, **kwargs: Any) -> Any:
        return await self._require_pool().fetch(query, *args, **kwargs)

    async def fetchrow(self, query: str, *args: Any, **kwargs: Any) -> Any:
        return await self._require_pool().fetchrow(query, *args, **kwargs)

    async def fetchval(self, query: str, *args: Any, **kwargs: Any) -> Any:
        return await self._require_pool().fetchval(query, *args, **kwargs)

    async def migrate(
        self,
        directory: str | Path,
        *,
        schema: str,
        lock_timeout: float = 60,
    ) -> list[int]:
        """Run SQLx migrations and return the versions applied by this call.

        Relative paths resolve from the workflow module. Call outside ctx.step
        on every invocation; SQLx owns history, checksums, and transactions.
        """
        self._require_pool()
        if not math.isfinite(lock_timeout) or lock_timeout <= 0:
            raise ValueError("lock_timeout must be positive and finite")
        path = Path(directory)
        if not path.is_absolute():
            if self._source_path is None:
                raise ValueError(
                    "Relative migrations require the workflow's source_path"
                )
            path = self._source_path.parent / path
        runner = os.environ.get("WORKFLOW_MIGRATOR_PATH", "centaur-workflow-migrate")
        try:
            process = await asyncio.create_subprocess_exec(
                runner,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
        except FileNotFoundError as exc:
            raise MigrationError(
                "SQLx migration runner is unavailable; install centaur-workflow-migrate "
                "or set WORKFLOW_MIGRATOR_PATH"
            ) from exc
        try:
            assert process.stdin is not None
            assert process.stdout is not None
            assert process.stderr is not None
            process.stdin.write(
                json.dumps(
                    {
                        "directory": str(path),
                        "schema": schema,
                        "lock_timeout": lock_timeout,
                    }
                ).encode()
                + b"\n"
            )
            await process.stdin.drain()
            # Keep stdin open: its EOF tells the runner that the host has died.
            stdout, stderr = await asyncio.gather(
                process.stdout.read(), process.stderr.read()
            )
            await process.wait()
            if process.returncode:
                raise MigrationError(
                    stderr.decode(errors="replace").strip()
                    or "SQLx migration runner failed"
                )
            return json.loads(stdout)["applied"]
        finally:
            if process.returncode is None:
                with contextlib.suppress(ProcessLookupError):
                    process.kill()
                await asyncio.shield(process.wait())
            if process.stdin is not None:
                process.stdin.close()
