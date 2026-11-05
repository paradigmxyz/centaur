from __future__ import annotations

import asyncio
import importlib
import json
import sys
import types
from pathlib import Path


def _load_sync():
    repo_root = Path(__file__).resolve().parents[3]
    if str(repo_root) not in sys.path:
        sys.path.insert(0, str(repo_root))

    api_module = sys.modules.setdefault("api", types.ModuleType("api"))

    runtime_control = types.ModuleType("api.runtime_control")
    runtime_control.canonical_json = lambda value: json.dumps(value, sort_keys=True)
    api_module.runtime_control = runtime_control
    sys.modules["api.runtime_control"] = runtime_control

    etl_metrics = types.ModuleType("workflows.etl_metrics")
    for name in (
        "record_etl_items_enqueued",
        "record_etl_items_failed",
        "record_etl_items_seen",
        "record_etl_items_upserted",
        "set_etl_active_scopes",
        "set_etl_failed_scopes",
        "set_etl_scope_sync_freshness_seconds",
    ):
        setattr(etl_metrics, name, lambda *_args, **_kwargs: None)
    sys.modules["workflows.etl_metrics"] = etl_metrics

    slack_metrics = types.ModuleType("workflows.slack.metrics")
    for name in (
        "observe_slack_retention_run_duration",
        "record_slack_etl_rate_limit",
        "record_slack_retention_api_rate_limited",
        "record_slack_retention_api_request",
        "record_slack_retention_channel_failure",
        "record_slack_retention_failure",
        "record_slack_retention_messages_processed",
        "record_slack_retention_run",
        "set_slack_retention_last_failure_timestamp",
        "set_slack_retention_watermark_lag_seconds",
    ):
        setattr(slack_metrics, name, lambda *_args, **_kwargs: None)
    sys.modules["workflows.slack.metrics"] = slack_metrics

    workflow_engine = types.ModuleType("api.workflow_engine")
    workflow_engine.WorkflowContext = object
    api_module.workflow_engine = workflow_engine
    sys.modules["api.workflow_engine"] = workflow_engine

    centaur_sdk = sys.modules.setdefault("centaur_sdk", types.ModuleType("centaur_sdk"))
    centaur_sdk.secret = lambda _name, default=None: default

    return importlib.import_module("workflows.slack.sync")


sync = _load_sync()


class FakeConn:
    """Records statements issued inside ``_upsert_channels``."""

    def __init__(self) -> None:
        self.execute_calls: list[tuple] = []

    async def execute(self, sql, *args):
        self.execute_calls.append((sql, args))

    def transaction(self):
        conn = self

        class _Txn:
            async def __aenter__(self_):
                return conn

            async def __aexit__(self_, *_exc):
                return False

        return _Txn()


class FakePool:
    def __init__(self, conn: FakeConn) -> None:
        self._conn = conn

    def acquire(self):
        conn = self._conn

        class _Acquire:
            async def __aenter__(self_):
                return conn

            async def __aexit__(self_, *_exc):
                return False

        return _Acquire()


def test_channels_leaving_discovery_fail_closed_on_privacy():
    """Reader policy keys on is_private alone: undiscovered channels must be
    marked private, not left with a stale public classification."""
    conn = FakeConn()
    asyncio.run(sync._upsert_channels(FakePool(conn), [], set()))

    assert conn.execute_calls, "expected the out-of-scope update"
    scope_sql = conn.execute_calls[0][0]
    assert "is_syncable = FALSE" in scope_sql
    assert "is_private = TRUE" in scope_sql


def test_discovered_channel_restores_its_live_privacy_flag():
    """A rediscovered channel gets its current is_private from the Slack
    payload — the fail-closed update only covers absent channels."""
    conn = FakeConn()
    channels = [
        {"id": "C1", "name": "general", "is_private": False},
        {"id": "G1", "name": "secret-team", "is_private": True},
    ]
    asyncio.run(
        sync._upsert_channels(FakePool(conn), channels, {"C1", "G1"})
    )

    upserts = [
        (sql, args)
        for sql, args in conn.execute_calls
        if "INSERT INTO slack_sync_channels" in sql
    ]
    assert len(upserts) == 2
    # $4 is is_private and $5 is is_syncable in the INSERT parameter list.
    assert upserts[0][1][3] is False
    assert upserts[0][1][4] is True
    assert upserts[1][1][3] is True
    assert "is_private = EXCLUDED.is_private" in upserts[0][0]


def test_discovered_but_filtered_channel_keeps_verified_privacy():
    """A channel that was discovered but excluded from sync scope (exclusion
    pattern or bot non-membership) still has its privacy state verified — it
    must keep its live is_private while being marked unsyncable."""
    conn = FakeConn()
    channels = [
        {"id": "C1", "name": "general", "is_private": False},
        {"id": "C2", "name": "excluded-public", "is_private": False},
    ]
    asyncio.run(sync._upsert_channels(FakePool(conn), channels, {"C1"}))

    upserts = [
        (sql, args)
        for sql, args in conn.execute_calls
        if "INSERT INTO slack_sync_channels" in sql
    ]
    assert len(upserts) == 2
    by_id = {args[0]: args for _sql, args in upserts}
    assert by_id["C1"][3] is False and by_id["C1"][4] is True
    assert by_id["C2"][3] is False and by_id["C2"][4] is False
    assert "is_syncable = EXCLUDED.is_syncable" in upserts[0][0]


def test_users_leaving_directory_fail_closed_as_deleted():
    """A user absent from a complete listing can no longer be verified —
    their row is marked is_deleted so the profile stops presenting as active."""
    conn = FakeConn()
    users = [{"id": "U1", "name": "ada"}]
    asyncio.run(sync._upsert_users(FakePool(conn), users, listing_complete=True))

    reset_sql = conn.execute_calls[0][0]
    assert "is_deleted = TRUE" in reset_sql


def test_deactivated_user_in_listing_writes_is_deleted():
    """users.list reports deactivated accounts — the flag must reach the row."""
    conn = FakeConn()
    users = [{"id": "U1", "name": "ada", "deleted": True}]
    asyncio.run(sync._upsert_users(FakePool(conn), users))

    upserts = [
        (sql, args)
        for sql, args in conn.execute_calls
        if "INSERT INTO slack_sync_users" in sql
    ]
    assert len(upserts) == 1
    # $6 is is_deleted in the INSERT parameter list.
    assert upserts[0][1][5] is True
    assert "is_deleted = EXCLUDED.is_deleted" in upserts[0][0]


def test_users_reset_skipped_on_empty_or_partial_listing():
    """An empty listing or a cap-truncated one must not mass-mark users."""
    conn = FakeConn()
    asyncio.run(sync._upsert_users(FakePool(conn), []))
    assert conn.execute_calls == []

    conn = FakeConn()
    users = [{"id": "U1", "name": "ada"}]
    asyncio.run(sync._upsert_users(FakePool(conn), users, listing_complete=False))
    assert not any(
        sql.startswith("UPDATE slack_sync_users")
        for sql, _args in conn.execute_calls
    )
