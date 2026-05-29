from __future__ import annotations

import os
import re
import subprocess
from dataclasses import dataclass
from pathlib import Path

import asyncpg
import structlog

log = structlog.get_logger()

BASE_MIGRATIONS_DIR = Path(__file__).resolve().parent.parent / "db" / "migrations"
BASE_MIGRATIONS_TABLE = "schema_migrations"
OVERLAY_MIGRATIONS_TABLE = "schema_migrations_overlay"
OVERLAY_MIGRATIONS_RELATIVE_DIR = Path("services") / "api" / "db" / "migrations"

REQUIRED_SANDBOX_SESSION_STATES = frozenset(
    {
        "creating",
        "running",
        "idle",
        "error",
        "stopped",
        "gone",
        "delivering",
        "suspended",
    }
)

REQUIRED_SANDBOX_SESSION_COLUMNS = frozenset(
    {
        "agent_thread_id",
        "inflight_turn_id",
        "inflight_turn_input",
        "inflight_started_at",
        "inflight_attempts",
        "last_result",
        "last_result_at",
        "trace_id",
    }
)

REQUIRED_MIGRATIONS = frozenset(
    {
        "005",
        "006",
        "007",
        "008",
        "009",
        "010",
        "011",
        "035",
    }
)


@dataclass(frozen=True, slots=True)
class MigrationSet:
    name: str
    migrations_dir: Path
    migrations_table: str


def get_migration_sets() -> tuple[MigrationSet, ...]:
    migration_sets = [
        MigrationSet(
            name="core",
            migrations_dir=BASE_MIGRATIONS_DIR,
            migrations_table=BASE_MIGRATIONS_TABLE,
        )
    ]

    overlay_root = (os.getenv("CENTAUR_OVERLAY_DIR") or "").strip()
    if overlay_root:
        overlay_migrations_dir = Path(overlay_root) / OVERLAY_MIGRATIONS_RELATIVE_DIR
        if overlay_migrations_dir.exists():
            migration_sets.append(
                MigrationSet(
                    name="overlay",
                    migrations_dir=overlay_migrations_dir,
                    migrations_table=OVERLAY_MIGRATIONS_TABLE,
                )
            )

    return tuple(migration_sets)


def _dbmate_url(database_url: str) -> str:
    # dbmate's Go pq driver requires explicit sslmode for non-SSL connections.
    if "sslmode=" in database_url:
        return database_url

    sep = "&" if "?" in database_url else "?"
    return f"{database_url}{sep}sslmode=disable"


async def create_pool(database_url: str, *, apply_migrations: bool = True) -> asyncpg.Pool:
    # The sandbox tool-server sidecar reaches the DB through the per-sandbox
    # iron-proxy and is not a schema owner, so it opens a pool with
    # apply_migrations=False. The API (and shared tool-server) own migrations.
    if apply_migrations:
        run_migrations(database_url)
    pool = await asyncpg.create_pool(
        database_url,
        min_size=2,
        max_size=10,
        command_timeout=60,
    )
    assert pool is not None
    return pool


async def close_pool(pool: asyncpg.Pool) -> None:
    await pool.close()


def run_migrations(database_url: str) -> None:
    """Run pending dbmate migrations. Idempotent — safe to call on every startup."""
    dbmate_url = _dbmate_url(database_url)
    migration_sets = get_migration_sets()

    try:
        applied_any = False
        for migration_set in migration_sets:
            if not migration_set.migrations_dir.exists():
                log.warning(
                    "migrations_dir_missing",
                    set_name=migration_set.name,
                    path=str(migration_set.migrations_dir),
                )
                continue

            applied_any = True
            result = subprocess.run(
                [
                    "dbmate",
                    "--url",
                    dbmate_url,
                    "--migrations-dir",
                    str(migration_set.migrations_dir),
                    "--migrations-table",
                    migration_set.migrations_table,
                    "--no-dump-schema",
                    "up",
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
            if result.returncode != 0:
                log.error(
                    "dbmate_failed",
                    set_name=migration_set.name,
                    migrations_table=migration_set.migrations_table,
                    stderr=result.stderr.strip(),
                    returncode=result.returncode,
                )
                raise RuntimeError(
                    "dbmate migration failed "
                    f"for {migration_set.name}: {result.stderr.strip()}"
                )
            if result.stderr.strip():
                for line in result.stderr.strip().splitlines():
                    log.info("dbmate", set_name=migration_set.name, output=line)

        if not applied_any:
            log.warning("migrations_skipped", reason="no_migration_sets_found")
            return

        log.info(
            "migrations_applied", migration_sets=[ms.name for ms in migration_sets]
        )
    except FileNotFoundError:
        log.warning(
            "dbmate_not_found", msg="dbmate binary not in PATH, skipping migrations"
        )


async def check_schema_compatibility(pool: asyncpg.Pool) -> dict[str, object]:
    """Verify DB schema invariants required by current API runtime code."""

    report: dict[str, object] = {
        "compatible": False,
        "required_states_missing": [],
        "required_columns_missing": [],
        "required_migrations_missing": [],
        "constraint_present": False,
        "errors": [],
    }

    try:
        row = await pool.fetchrow(
            "SELECT pg_get_constraintdef(c.oid) AS definition "
            "FROM pg_constraint c "
            "JOIN pg_class t ON t.oid = c.conrelid "
            "WHERE t.relname = 'sandbox_sessions' "
            "AND c.conname = 'sandbox_sessions_state_check' "
            "LIMIT 1"
        )
        definition = row["definition"] if row else None
        report["constraint_present"] = bool(definition)
        if definition:
            present_states = set(re.findall(r"'([^']+)'", str(definition)))
            missing_states = sorted(REQUIRED_SANDBOX_SESSION_STATES - present_states)
        else:
            missing_states = sorted(REQUIRED_SANDBOX_SESSION_STATES)
        report["required_states_missing"] = missing_states
    except Exception as exc:
        report["required_states_missing"] = sorted(REQUIRED_SANDBOX_SESSION_STATES)
        report["errors"].append(f"state_constraint_check_failed:{exc}")

    try:
        col_rows = await pool.fetch(
            "SELECT column_name FROM information_schema.columns "
            "WHERE table_schema = 'public' AND table_name = 'sandbox_sessions'"
        )
        present_columns = {r["column_name"] for r in col_rows}
        report["required_columns_missing"] = sorted(
            REQUIRED_SANDBOX_SESSION_COLUMNS - present_columns
        )
    except Exception as exc:
        report["required_columns_missing"] = sorted(REQUIRED_SANDBOX_SESSION_COLUMNS)
        report["errors"].append(f"column_check_failed:{exc}")

    try:
        migration_rows = await pool.fetch(
            f"SELECT version FROM {BASE_MIGRATIONS_TABLE}"
        )
        applied = {r["version"] for r in migration_rows}
        report["required_migrations_missing"] = sorted(REQUIRED_MIGRATIONS - applied)
    except Exception as exc:
        report["required_migrations_missing"] = sorted(REQUIRED_MIGRATIONS)
        report["errors"].append(f"migration_check_failed:{exc}")

    report["compatible"] = not (
        report["required_states_missing"]
        or report["required_columns_missing"]
        or report["required_migrations_missing"]
        or report["errors"]
    )

    return report
