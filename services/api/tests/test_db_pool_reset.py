"""Unit tests for ``_SingleStmtResetConnection`` used by the sandbox pool.

The sandbox tool-server sidecar's pool runs ``Connection.reset()`` on every
``pool.release()``. asyncpg's default reset joins several cleanup statements
into one multi-statement query, which the per-sandbox iron-proxy refuses.
This regression test pins the override so reset stays split per-statement.
"""

from __future__ import annotations

from unittest.mock import AsyncMock

import pytest

from api.db import _SingleStmtResetConnection


class _StubConnection(_SingleStmtResetConnection):
    """Bypass ``asyncpg.Connection.__init__`` so we can drive ``reset()`` in tests."""

    def __init__(self) -> None:  # noqa: D401 — minimal stub
        self.executed: list[str] = []
        self._inner_reset_called = 0

    async def _reset(self) -> None:
        self._inner_reset_called += 1

    def get_reset_query(self) -> str:
        return (
            "SELECT pg_advisory_unlock_all();\n"
            "CLOSE ALL;\n"
            "UNLISTEN *;\n"
            "RESET ALL;"
        )

    async def execute(self, query: str, *args: object, **kwargs: object) -> str:  # type: ignore[override]
        self.executed.append(query)
        return "OK"


@pytest.mark.asyncio
async def test_reset_runs_statements_individually() -> None:
    """Each cleanup statement is executed in its own ``execute()`` call.

    Guards against regression to the multi-statement reset, which iron-proxy
    blocks with ``PostgresSyntaxError: blocked by iron-proxy policy:
    multi-statement queries not permitted``.
    """
    conn = _StubConnection()

    await conn.reset()

    assert conn._inner_reset_called == 1
    assert conn.executed == [
        "SELECT pg_advisory_unlock_all();",
        "CLOSE ALL;",
        "UNLISTEN *;",
        "RESET ALL;",
    ]
    # Sanity-check: no element ever contains a newline (i.e. concatenated stmts).
    assert all("\n" not in stmt for stmt in conn.executed)


@pytest.mark.asyncio
async def test_reset_noop_when_query_empty() -> None:
    conn = _StubConnection()
    conn.get_reset_query = lambda: ""  # type: ignore[assignment]

    await conn.reset()

    assert conn._inner_reset_called == 1
    assert conn.executed == []


@pytest.mark.asyncio
async def test_reset_skips_blank_lines() -> None:
    conn = _StubConnection()
    conn.get_reset_query = lambda: "\nCLOSE ALL;\n\nRESET ALL;\n"  # type: ignore[assignment]

    await conn.reset()

    assert conn.executed == ["CLOSE ALL;", "RESET ALL;"]


@pytest.mark.asyncio
async def test_create_pool_uses_override_for_sandbox_path(monkeypatch: pytest.MonkeyPatch) -> None:
    """``apply_migrations=False`` must wire the no-multi-stmt connection class."""
    import api.db as db_mod

    captured: dict[str, object] = {}

    async def fake_create_pool(_url: str, **kwargs: object) -> object:
        captured.update(kwargs)
        return AsyncMock()

    monkeypatch.setattr(db_mod.asyncpg, "create_pool", fake_create_pool)

    await db_mod.create_pool("postgresql://x/y", apply_migrations=False)

    assert captured.get("connection_class") is _SingleStmtResetConnection


@pytest.mark.asyncio
async def test_create_pool_default_path_keeps_stock_connection(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``apply_migrations=True`` (API + shared tool-server) keeps asyncpg's default."""
    import api.db as db_mod

    captured: dict[str, object] = {}

    async def fake_create_pool(_url: str, **kwargs: object) -> object:
        captured.update(kwargs)
        return AsyncMock()

    monkeypatch.setattr(db_mod.asyncpg, "create_pool", fake_create_pool)
    monkeypatch.setattr(db_mod, "run_migrations", lambda _url: None)

    await db_mod.create_pool("postgresql://x/y", apply_migrations=True)

    assert "connection_class" not in captured
