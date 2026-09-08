"""Workflow database access through the host's asyncpg pool."""

from __future__ import annotations

from typing import Any


class WorkflowDatabase:
    """A thin handle over the host's asyncpg pool; queries are not checkpointed."""

    def __init__(self, pool: Any) -> None:
        self._pool = pool

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
