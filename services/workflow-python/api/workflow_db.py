"""Workflow database access and explicitly invoked SQLx migrations."""

from __future__ import annotations

import asyncio
import contextlib
import math
import os
import re
from pathlib import Path
from typing import Any
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit


class MigrationError(RuntimeError):
    """The SQLx CLI failed to run workflow migrations."""


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
    ) -> None:
        """Run SQLx migrations, raising MigrationError if the CLI fails.

        Relative paths resolve from the workflow module. Call outside ctx.step
        on every invocation; SQLx owns history, checksums, and transactions.
        """
        pool = self._require_pool()
        # Discovery and workflows without database operations need no DB driver.
        import asyncpg

        if not schema or len(schema.encode()) > 63 or "\0" in schema:
            raise ValueError("schema must contain 1 to 63 UTF-8 bytes and no NUL")
        if not math.isfinite(lock_timeout) or lock_timeout <= 0:
            raise ValueError("lock_timeout must be positive and finite")
        path = Path(directory)
        if not path.is_absolute():
            if self._source_path is None:
                raise ValueError(
                    "Relative migrations require the workflow's source_path"
                )
            path = self._source_path.parent / path
        if not path.is_dir():
            raise ValueError(f"Migration directory does not exist: {path}")
        database_url = os.environ.get("DATABASE_URL")
        if not database_url:
            raise RuntimeError("Workflow migrations require DATABASE_URL")

        quoted = await pool.fetchval("SELECT pg_catalog.quote_ident($1)", schema)
        exists_query = (
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = $1)"
        )
        if not await pool.fetchval(exists_query, schema):
            try:
                await pool.execute(f"CREATE SCHEMA {quoted}")
            except (asyncpg.DuplicateSchemaError, asyncpg.UniqueViolationError):
                # Another invocation may have created the schema concurrently.
                if not await pool.fetchval(exists_query, schema):
                    raise

        parts = urlsplit(database_url)
        query = parse_qsl(parts.query, keep_blank_values=True)
        # PostgreSQL splits startup options on whitespace, even inside SQL quotes.
        search_path = re.sub(r"([\\\s])", r"\\\1", quoted)
        query.append(
            (
                "options",
                (
                    f"-c search_path={search_path} "
                    f"-c lock_timeout={max(1, math.ceil(lock_timeout * 1000))} "
                    "-c client_connection_check_interval=1000"
                ),
            )
        )
        # Keep credentials in the environment, and exclude public from search_path
        # so SQLx cannot resolve the control plane's migration history table.
        env = {
            **os.environ,
            "DATABASE_URL": urlunsplit(parts._replace(query=urlencode(query))),
        }
        async with pool.acquire() as connection, connection.transaction():
            await connection.execute(
                "SELECT set_config('lock_timeout', $1, true)",
                f"{max(1, math.ceil(lock_timeout * 1000))}ms",
            )
            try:
                # The SQLx CLI does not lock migrations. Keep this transaction
                # open until it exits to serialize calls for the same schema.
                await connection.execute(
                    "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                    f"workflow-migrations:{schema}",
                )
            except asyncpg.LockNotAvailableError as exc:
                raise MigrationError(
                    "timed out waiting for the workflow migration lock"
                ) from exc
            try:
                process = await asyncio.create_subprocess_exec(
                    "sqlx",
                    "--no-dotenv",
                    "migrate",
                    "run",
                    "--source",
                    str(path),
                    "--ignore-missing",
                    stdin=asyncio.subprocess.DEVNULL,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                    env=env,
                )
            except FileNotFoundError as exc:
                raise MigrationError(
                    "SQLx CLI is unavailable; install sqlx-cli and put sqlx on PATH"
                ) from exc
            try:
                stdout, stderr = await process.communicate()
                if process.returncode:
                    raise MigrationError(
                        (stderr + stdout).decode(errors="replace").strip()
                        or "SQLx migrations failed"
                    )
            finally:
                if process.returncode is None:
                    with contextlib.suppress(ProcessLookupError):
                        process.kill()
                    await asyncio.shield(process.wait())
