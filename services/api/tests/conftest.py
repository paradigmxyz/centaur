"""Shared test fixtures for API integration tests.

Spins up an ephemeral Postgres instance on the host for the test session,
runs migrations, and provides an httpx client against the real app.
"""

from __future__ import annotations

import asyncio
import os
import re
import socket
import shutil
import subprocess
import tempfile
import time
import uuid
from pathlib import Path
from urllib.parse import SplitResult, urlsplit, urlunsplit

import asyncpg
import httpx
import pytest
import pytest_asyncio


def pytest_configure(config):
    config.addinivalue_line("markers", "sandbox: requires running sandbox container")


def _pick_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _dsn_with_db(dsn: str, database: str) -> str:
    parts = urlsplit(dsn)
    return urlunsplit(
        SplitResult(
            scheme=parts.scheme,
            netloc=parts.netloc,
            path=f"/{database}",
            query=parts.query,
            fragment=parts.fragment,
        )
    )


def _have_local_pg_binaries() -> bool:
    return all(shutil.which(cmd) for cmd in ("initdb", "pg_ctl", "pg_isready"))


def _have_docker() -> bool:
    return shutil.which("docker") is not None


async def _can_connect(dsn: str) -> bool:
    try:
        conn = await asyncpg.connect(dsn)
    except Exception:
        return False
    try:
        await conn.execute("SELECT 1")
        return True
    finally:
        await conn.close()


async def _wait_for_postgres(dsn: str, timeout_s: float = 30.0) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        try:
            conn = await asyncpg.connect(dsn)
        except Exception:
            await asyncio.sleep(0.25)
            continue
        try:
            await conn.execute("SELECT 1")
            return
        finally:
            await conn.close()
    raise RuntimeError("Postgres did not start in time")


async def _ensure_database(admin_dsn: str, database: str) -> None:
    conn = await asyncpg.connect(admin_dsn)
    try:
        exists = await conn.fetchval("SELECT 1 FROM pg_database WHERE datname = $1", database)
        if not exists:
            safe_db = database.replace('"', '""')
            await conn.execute(f'CREATE DATABASE "{safe_db}"')
    finally:
        await conn.close()


async def _run_migrations_async(dsn: str, migrations_dir: Path) -> None:
    conn = await asyncpg.connect(dsn)
    try:
        for migration_file in sorted(migrations_dir.glob("*.sql")):
            up_sql = _extract_up_sql(migration_file)
            await conn.execute(up_sql)
    finally:
        await conn.close()


@pytest.fixture(scope="session")
def pg():
    """Start an ephemeral Postgres on a random port, yield the DSN, tear down after."""
    database = "centaur_test"
    migrations_dir = Path(__file__).resolve().parent.parent / "db" / "migrations"

    explicit_dsn = os.environ.get("CENTAUR_TEST_DATABASE_URL", "").strip()
    if explicit_dsn:
        admin_dsn = _dsn_with_db(explicit_dsn, "postgres")
        dsn = _dsn_with_db(explicit_dsn, database)
        if not asyncio.run(_can_connect(admin_dsn)):
            pytest.skip(
                f"CENTAUR_TEST_DATABASE_URL is set but unreachable: {explicit_dsn}",
                allow_module_level=False,
            )
        asyncio.run(_ensure_database(admin_dsn, database))
        asyncio.run(_run_migrations_async(dsn, migrations_dir))
        yield dsn
        return

    if _have_local_pg_binaries():
        tmpdir = tempfile.mkdtemp(prefix="centaur-test-pg-")
        port = _pick_free_port()
        admin_dsn = f"postgresql://localhost:{port}/postgres?host={tmpdir}"
        dsn = _dsn_with_db(admin_dsn, database)

        try:
            subprocess.run(
                ["initdb", "-D", tmpdir, "--no-locale", "-E", "UTF8"],
                check=True,
                capture_output=True,
            )
            subprocess.run(
                [
                    "pg_ctl", "-D", tmpdir, "-o", f"-p {port} -k {tmpdir}",
                    "-l", f"{tmpdir}/pg.log", "start",
                ],
                check=True,
                capture_output=True,
            )
            asyncio.run(_wait_for_postgres(admin_dsn))
            asyncio.run(_ensure_database(admin_dsn, database))
            asyncio.run(_run_migrations_async(dsn, migrations_dir))
            yield dsn
        finally:
            subprocess.run(
                ["pg_ctl", "-D", tmpdir, "stop", "-m", "immediate"],
                capture_output=True,
            )
            shutil.rmtree(tmpdir, ignore_errors=True)
        return

    if not _have_docker():
        raise RuntimeError(
            "Postgres test bootstrap requires either local postgres binaries "
            "(initdb/pg_ctl/pg_isready) or Docker"
        )

    port = _pick_free_port()
    container_name = f"centaur-test-pg-{uuid.uuid4().hex[:8]}"
    image = os.environ.get("CENTAUR_TEST_PG_IMAGE", "paradedb/paradedb:0.23.0-pg16")
    admin_dsn = f"postgresql://postgres@127.0.0.1:{port}/postgres?sslmode=disable"
    dsn = _dsn_with_db(admin_dsn, database)

    try:
        result = subprocess.run(
            [
                "docker",
                "run",
                "--rm",
                "-d",
                "--name",
                container_name,
                "-e",
                "POSTGRES_HOST_AUTH_METHOD=trust",
                "-e",
                f"POSTGRES_DB={database}",
                "-p",
                f"127.0.0.1:{port}:5432",
                image,
                "-c",
                "shared_preload_libraries=pg_search,pg_cron",
            ],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.skip(
                "Docker Postgres bootstrap failed. Set CENTAUR_TEST_DATABASE_URL to an existing Postgres, "
                f"or pre-pull the test image ({image}). stderr: {result.stderr.strip()}",
                allow_module_level=False,
            )
        asyncio.run(_wait_for_postgres(dsn, timeout_s=60.0))
        asyncio.run(_run_migrations_async(dsn, migrations_dir))
        yield dsn
    finally:
        subprocess.run(["docker", "rm", "-f", container_name], capture_output=True)


def _extract_up_sql(path: Path) -> str:
    """Extract the ``-- migrate:up`` section from a dbmate-style migration file."""
    text = path.read_text()
    match = re.search(r"-- migrate:up\s*\n(.*?)(?=-- migrate:down|$)", text, re.DOTALL)
    if not match:
        raise ValueError(f"No '-- migrate:up' section found in {path}")
    return match.group(1).strip()


@pytest.fixture(scope="session")
def run_migrations(pg):
    """Compatibility no-op: migrations are applied during Postgres bootstrap."""


@pytest.fixture(scope="session")
def _setup_env(pg, run_migrations):
    """Set env vars before any app code is imported."""
    os.environ["DATABASE_URL"] = pg
    os.environ["API_SECRET_KEY"] = "test-secret-key"
    # Seed a real admin key. bootstrap_service_api_keys() turns LOCAL_DEV_API_KEY
    # into a DB-backed key with admin scope on app startup, so tests authenticate
    # the same way production callers do (there is no localhost auth bypass).
    os.environ["LOCAL_DEV_API_KEY"] = "aiv2_test_local_dev_admin_key"
    os.environ["EXECUTION_WORKER_ENABLED"] = "0"
    os.environ["WORKFLOW_WORKER_ENABLED"] = "0"
    os.environ["WARM_POOL_ENABLED"] = "0"


@pytest.fixture(scope="session")
def app(_setup_env):
    """Import and return the real FastAPI app (after env is configured)."""
    from api.app import app as real_app

    return real_app


@pytest_asyncio.fixture
async def managed_app(app):
    """Manage app lifespan once per test."""
    from asgi_lifespan import LifespanManager

    async with LifespanManager(app):
        yield app


@pytest_asyncio.fixture
async def client(managed_app):
    """Async httpx client against the lifespan-managed app.

    Authenticated as the seeded admin key by default so tests exercise real
    callers. Tests covering auth failures pass their own Authorization header,
    which overrides this default.
    """
    transport = httpx.ASGITransport(app=managed_app)
    async with httpx.AsyncClient(
        transport=transport,
        base_url="http://test",
        headers={"Authorization": f"Bearer {os.environ['LOCAL_DEV_API_KEY']}"},
    ) as c:
        yield c


@pytest.fixture
def api_key():
    """Return the seeded admin API key."""
    return os.environ["LOCAL_DEV_API_KEY"]


@pytest_asyncio.fixture
async def db_pool(managed_app):
    """Yield the live asyncpg pool from the running app."""
    yield managed_app.state.db_pool
