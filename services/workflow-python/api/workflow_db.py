"""Workflow-owned PostgreSQL tables and explicitly invoked SQL migrations."""

from __future__ import annotations

import hashlib
import logging
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from psycopg.sql import Identifier, Literal

logger = logging.getLogger(__name__)
_MIGRATION_NAME = re.compile(r"([0-9]+)_[a-zA-Z0-9_][a-zA-Z0-9_-]*\.sql\Z")


class MigrationError(RuntimeError):
    """A workflow's migration files or database history cannot be applied."""


@dataclass(frozen=True)
class Migration:
    version: int
    filename: str
    sql: str


def _read_migrations(directory: Path) -> list[Migration]:
    migrations: dict[int, Migration] = {}
    for path in sorted(directory.iterdir()):
        if path.suffix != ".sql":
            continue
        match = _MIGRATION_NAME.fullmatch(path.name)
        if match is None:
            raise MigrationError(
                f"Invalid migration filename {path.name!r}; use 001_description.sql"
            )
        version = int(match[1])
        if not 0 < version <= 2**63 - 1:
            raise MigrationError(
                f"Migration version must be a positive bigint: {path.name}"
            )
        if version in migrations:
            raise MigrationError(f"Duplicate migration version {version}: {path.name}")
        sql = path.read_text(encoding="utf-8")
        if not sql.strip():
            raise MigrationError(f"Empty migration: {path.name}")
        migrations[version] = Migration(version, path.name, sql)
    if not migrations:
        raise MigrationError(f"No SQL migrations in {directory}")
    return sorted(migrations.values(), key=lambda migration: migration.version)


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
    ) -> list[str]:
        """Apply pending SQL files and return their filenames in application order.

        Relative directories resolve from the workflow module. Call on each
        invocation, outside ctx.step: the schema's ledger makes retries safe.
        Each file is transactional, and previously applied versions are skipped.
        """
        pool = self._require_pool()
        # PostgreSQL silently truncates longer names, which would split the lock
        # identity from the actual schema. Reject those names before connecting.
        if not schema or "\x00" in schema or len(schema.encode("utf-8")) > 63:
            raise ValueError("Schema must contain 1 to 63 UTF-8 bytes and no NUL")
        quoted_schema = Identifier(schema).as_string()
        if lock_timeout <= 0:
            raise ValueError("lock_timeout must be positive")
        path = Path(directory)
        if not path.is_absolute():
            if self._source_path is None:
                raise ValueError(
                    "Relative migrations require the workflow's source_path"
                )
            path = self._source_path.parent / path
        migrations = _read_migrations(path)
        ledger = f"{quoted_schema}._workflow_migrations"
        lock_key = int.from_bytes(
            hashlib.sha256(f"centaur:workflow-migrations:{schema}".encode()).digest()[
                :8
            ],
            "big",
            signed=True,
        )

        # Keep one connection for the session lock across per-file commits.
        # asyncpg's pool release resets the connection (including advisory
        # locks), even if acquisition is cancelled or the connection is lost.
        async with pool.acquire() as connection:
            try:
                await connection.execute(
                    "SELECT pg_catalog.pg_advisory_lock($1)",
                    lock_key,
                    timeout=lock_timeout,
                )
            except TimeoutError as exc:
                raise MigrationError(
                    f"Timed out waiting to migrate schema {schema!r}"
                ) from exc
            async with connection.transaction():
                exists = await connection.fetchval(
                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = $1)",
                    schema,
                )
                # Even IF NOT EXISTS requires database CREATE privileges. An
                # existing schema should work with only schema-local grants.
                if not exists:
                    await connection.execute(
                        f"CREATE SCHEMA IF NOT EXISTS {quoted_schema}"
                    )
                await connection.execute(
                    f"CREATE TABLE IF NOT EXISTS {ledger} ("
                    "version bigint PRIMARY KEY, filename text NOT NULL, "
                    "applied_at timestamptz NOT NULL DEFAULT pg_catalog.clock_timestamp())"
                )
            rows = await connection.fetch(
                f"SELECT version, filename FROM {ledger} ORDER BY version"
            )
            applied = {row["version"]: row for row in rows}
            local = {migration.version: migration for migration in migrations}
            for version, row in applied.items():
                migration = local.get(version)
                if migration is None:
                    # Older bundles may run against newer database versions.
                    if version <= migrations[-1].version:
                        raise MigrationError(
                            f"Missing applied migration: {row['filename']}"
                        )
                elif migration.filename != row["filename"]:
                    raise MigrationError(
                        f"Applied migration renamed: {migration.filename}"
                    )
            latest = max(applied, default=0)
            pending = [
                migration
                for migration in migrations
                if migration.version not in applied
            ]
            for migration in pending:
                if migration.version <= latest:
                    raise MigrationError(
                        f"Out-of-order migration: {migration.filename}"
                    )

            completed = []
            for migration in pending:
                try:
                    async with connection.transaction():
                        # EXECUTE inside a DO block lets PostgreSQL parse the
                        # whole file, including dollar-quoted function bodies,
                        # while rejecting COMMIT/ROLLBACK that could separate
                        # the DDL from its ledger entry. No SQL parser needed.
                        body = (
                            f"BEGIN EXECUTE {Literal(migration.sql).as_string()}; END"
                        )
                        await connection.execute(f"DO {Literal(body).as_string()}")
                        await connection.execute(
                            f"INSERT INTO {ledger} (version, filename) VALUES ($1, $2)",
                            migration.version,
                            migration.filename,
                        )
                except Exception as exc:
                    raise MigrationError(
                        f"Migration {migration.filename!r} failed in schema {schema!r}"
                    ) from exc
                completed.append(migration.filename)
                logger.info(
                    "Applied workflow migration %s in schema %s",
                    migration.filename,
                    schema,
                )
            return completed
