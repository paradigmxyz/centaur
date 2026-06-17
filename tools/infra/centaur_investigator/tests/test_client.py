from __future__ import annotations

# ruff: noqa: I001

import datetime as dt
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

import client as investigator_client
from centaur_sdk.tool_sdk import ToolContext, reset_tool_context, set_tool_context
from client import CentaurInvestigatorClient, parse_slack_reference


class _FakeConnection:
    def __init__(self, responses=None) -> None:
        self.responses = list(responses or [])
        self.fetch_calls = []
        self.fetchrow_calls = []
        self.closed = False

    async def fetch(self, query, *args):
        self.fetch_calls.append((query, args))
        if self.responses:
            return self.responses.pop(0)
        return []

    async def fetchrow(self, query, *args):
        self.fetchrow_calls.append((query, args))
        if self.responses:
            rows = self.responses.pop(0)
            return rows[0] if rows else None
        return None

    async def close(self):
        self.closed = True


def test_parse_slack_permalink_prefers_thread_ts_query() -> None:
    result = parse_slack_reference(
        "Investigate https://example.slack.com/archives/C123/p1777910338403889"
        "?thread_ts=1777910337.403889&cid=C123"
    )

    assert result["status"] == "ok"
    assert result["kind"] == "slack_permalink"
    assert result["channel_id"] == "C123"
    assert result["message_ts"] == "1777910338.403889"
    assert result["thread_ts"] == "1777910337.403889"
    assert result["thread_key_candidates"] == [
        "slack:C123:1777910337.403889",
        "chat:C123:1777910337.403889",
    ]
    assert result["thread_key_like"] == "%:C123:1777910337.403889"


def test_parse_slack_thread_key_with_team() -> None:
    result = parse_slack_reference("slack:T0AQQ46PL4C:C0B0XS7BLA3:1780035646.228899")

    assert result["status"] == "ok"
    assert result["team_id"] == "T0AQQ46PL4C"
    assert result["channel_id"] == "C0B0XS7BLA3"
    assert result["thread_key_candidates"][:4] == [
        "slack:T0AQQ46PL4C:C0B0XS7BLA3:1780035646.228899",
        "chat:T0AQQ46PL4C:C0B0XS7BLA3:1780035646.228899",
        "slack:C0B0XS7BLA3:1780035646.228899",
        "chat:C0B0XS7BLA3:1780035646.228899",
    ]


def test_default_database_url_uses_tool_context_secret(monkeypatch) -> None:
    monkeypatch.delenv("CENTAUR_READONLY_DSN", raising=False)
    monkeypatch.setenv("DATABASE_URL", "postgresql://raw-app-db")
    token = set_tool_context(
        ToolContext(
            name="centaur_investigator",
            secrets={"CENTAUR_READONLY_DSN": "postgresql://readonly"},
        )
    )
    try:
        client = CentaurInvestigatorClient()

        assert client._require_database_url() == "postgresql://readonly"
    finally:
        reset_tool_context(token)


def test_default_database_url_does_not_fall_back_to_raw_database_url(monkeypatch) -> None:
    monkeypatch.delenv("CENTAUR_READONLY_DSN", raising=False)
    monkeypatch.setenv("DATABASE_URL", "postgresql://raw-app-db")
    token = set_tool_context(ToolContext(name="centaur_investigator", secrets={}))
    try:
        client = CentaurInvestigatorClient()

        try:
            client._require_database_url()
        except RuntimeError as exc:
            assert "CENTAUR_READONLY_DSN is required" in str(exc)
        else:
            raise AssertionError("expected missing DSN error")
    finally:
        reset_tool_context(token)


def test_investigate_slack_thread_queries_readonly_views(monkeypatch) -> None:
    now = dt.datetime(2026, 6, 16, 12, 0, tzinfo=dt.UTC)
    fake = _FakeConnection(
        responses=[
            [
                {
                    "thread_key": "slack:C123:1777910337.403889",
                    "sandbox_id": "asbx_1",
                    "harness_type": "codex",
                    "status": "idle",
                    "created_at": now,
                    "updated_at": now,
                }
            ],
            [
                {
                    "execution_id": "exe_1",
                    "thread_key": "slack:C123:1777910337.403889",
                    "status": "completed",
                    "created_at": now,
                    "completed_at": now,
                    "duration_seconds": 42.0,
                }
            ],
            [
                {
                    "message_id": "msg_1",
                    "thread_key": "slack:C123:1777910337.403889",
                    "role": "user",
                    "part_count": 1,
                    "part_types": ["text"],
                    "created_at": now,
                }
            ],
            [
                {
                    "event_id": 1,
                    "thread_key": "slack:C123:1777910337.403889",
                    "execution_id": "exe_1",
                    "event_type": "session.execution_completed",
                    "has_error": False,
                    "created_at": now,
                }
            ],
            [],
            [],
            [],
            [],
            [],
            [{"channel_id": "C123", "channel_name": "eng", "is_syncable": True}],
            [{"channel_id": "C123", "watermark_ts": "1778000000.000000"}],
            [
                {
                    "channel_id": "C123",
                    "message_ts": "1777910337.403889",
                    "thread_ts": "1777910337.403889",
                    "is_thread_root": True,
                    "reply_count": 2,
                    "updated_at": now,
                }
            ],
            [],
            [],
            [],
        ]
    )

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(investigator_client.asyncpg, "connect", fake_connect)

    result = CentaurInvestigatorClient("postgresql://example").investigate_slack_thread(
        "https://example.slack.com/archives/C123/p1777910337403889",
        include_observability=False,
    )

    assert result["status"] == "ok"
    assert result["thread_keys"][0] == "slack:C123:1777910337.403889"
    assert result["execution_ids"] == ["exe_1"]
    assert "Found 1 current session row" in result["analysis"]["summary"]
    assert "Slack sync has 1 message row" in result["analysis"]["summary"]
    first_query, first_args = fake.fetch_calls[0]
    assert "centaur_readonly_sessions" in first_query
    assert first_args[0] == [
        "slack:C123:1777910337.403889",
        "chat:C123:1777910337.403889",
    ]
    assert fake.closed is True
