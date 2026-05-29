"""Tests for DB schema compatibility checks used by readyz."""

from __future__ import annotations

from unittest.mock import AsyncMock

import pytest


@pytest.mark.asyncio
async def test_schema_compatibility_ok() -> None:
    from api.db import check_schema_compatibility

    pool = AsyncMock()
    pool.fetchrow = AsyncMock(
        return_value={
            "definition": (
                "CHECK ((state = ANY (ARRAY['creating'::text, 'running'::text, "
                "'idle'::text, 'error'::text, 'stopped'::text, 'gone'::text, "
                "'delivering'::text, 'suspended'::text])))"
            )
        }
    )
    pool.fetch = AsyncMock(
        side_effect=[
            [
                {"column_name": "agent_thread_id"},
                {"column_name": "inflight_turn_id"},
                {"column_name": "inflight_turn_input"},
                {"column_name": "inflight_started_at"},
                {"column_name": "inflight_attempts"},
                {"column_name": "last_result"},
                {"column_name": "last_result_at"},
                {"column_name": "model"},
                {"column_name": "trace_id"},
            ],
            [
                {"version": "005"},
                {"version": "006"},
                {"version": "007"},
                {"version": "008"},
                {"version": "009"},
                {"version": "010"},
                {"version": "011"},
                {"version": "035"},
                {"version": "037"},
            ],
        ]
    )

    report = await check_schema_compatibility(pool)

    assert report["compatible"] is True
    assert report["required_states_missing"] == []
    assert report["required_columns_missing"] == []
    assert report["required_migrations_missing"] == []
    assert report["errors"] == []


@pytest.mark.asyncio
async def test_schema_compatibility_detects_missing_state_column_and_migration() -> (
    None
):
    from api.db import check_schema_compatibility

    pool = AsyncMock()
    pool.fetchrow = AsyncMock(
        return_value={
            "definition": (
                "CHECK ((state = ANY (ARRAY['creating'::text, 'running'::text, "
                "'idle'::text, 'error'::text, 'stopped'::text, 'gone'::text, "
                "'delivering'::text])))"
            )
        }
    )
    pool.fetch = AsyncMock(
        side_effect=[
            [
                {"column_name": "agent_thread_id"},
                {"column_name": "inflight_turn_id"},
                {"column_name": "inflight_turn_input"},
                {"column_name": "inflight_started_at"},
                {"column_name": "inflight_attempts"},
                {"column_name": "last_result"},
                # last_result_at intentionally missing
                # model intentionally missing
                # trace_id intentionally missing
            ],
            [
                {"version": "005"},
                # 006/007/008/009/010/011/035/037 intentionally missing
            ],
        ]
    )

    report = await check_schema_compatibility(pool)

    assert report["compatible"] is False
    assert "suspended" in report["required_states_missing"]
    assert "last_result_at" in report["required_columns_missing"]
    assert "model" in report["required_columns_missing"]
    assert "trace_id" in report["required_columns_missing"]
    assert "006" in report["required_migrations_missing"]
    assert "007" in report["required_migrations_missing"]
    assert "008" in report["required_migrations_missing"]
    assert "009" in report["required_migrations_missing"]
    assert "010" in report["required_migrations_missing"]
    assert "011" in report["required_migrations_missing"]
    assert "035" in report["required_migrations_missing"]
    assert "037" in report["required_migrations_missing"]


@pytest.mark.asyncio
async def test_schema_compatibility_handles_query_failures() -> None:
    from api.db import check_schema_compatibility

    pool = AsyncMock()
    pool.fetchrow = AsyncMock(side_effect=RuntimeError("constraint query failed"))
    pool.fetch = AsyncMock(
        side_effect=[
            RuntimeError("column query failed"),
            RuntimeError("migration query failed"),
        ]
    )

    report = await check_schema_compatibility(pool)

    assert report["compatible"] is False
    assert len(report["errors"]) == 3
    assert any(
        str(err).startswith("state_constraint_check_failed:")
        for err in report["errors"]
    )
    assert any(str(err).startswith("column_check_failed:") for err in report["errors"])
    assert any(
        str(err).startswith("migration_check_failed:") for err in report["errors"]
    )
