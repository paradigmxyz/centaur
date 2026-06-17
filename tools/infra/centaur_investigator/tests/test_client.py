from __future__ import annotations

import datetime as dt
import sys
import tomllib
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

import client as centaur_client
from client import CentaurInvestigatorClient, parse_slack_reference


def test_readonly_dsn_manifest_targets_centaur_database() -> None:
    pyproject = tomllib.loads((Path(__file__).resolve().parents[1] / "pyproject.toml").read_text())
    secrets = pyproject["tool"]["centaur"]["secrets"]
    readonly = next(secret for secret in secrets if secret["name"] == "CENTAUR_READONLY_DSN")

    assert readonly["type"] == "pg_dsn"
    assert readonly["secret_ref"] == "DATABASE_URL"
    assert readonly["database"] == "centaur"


class _FakeConnection:
    def __init__(self) -> None:
        self.execute_calls: list[str] = []
        self.fetch_calls: list[tuple[str, tuple]] = []
        self.fetchrow_calls: list[tuple[str, tuple]] = []
        self.closed = False
        self.now = dt.datetime(2026, 6, 17, 12, 0, tzinfo=dt.UTC)

    async def execute(self, query: str) -> None:
        self.execute_calls.append(query)

    async def fetchrow(self, query: str, *args):
        self.fetchrow_calls.append((query, args))
        if "current_setting('role'" in query:
            return {
                "session_user": "centaur",
                "current_user": "centaur_readonly",
                "active_role": "centaur_readonly",
            }
        if "FROM slack_sync_channels" in query:
            return {
                "channel_id": "C123",
                "channel_name": "eng",
                "is_archived": False,
                "is_syncable": True,
                "member_count": 42,
                "first_seen_at": self.now,
                "last_seen_at": self.now,
                "updated_at": self.now,
            }
        if "FROM slack_sync_checkpoints" in query:
            return {
                "channel_id": "C123",
                "watermark_ts": "1778000000.000000",
                "last_run_id": "run_1",
                "last_success_at": self.now,
                "has_error": False,
                "created_at": self.now,
                "updated_at": self.now,
            }
        return None

    async def fetch(self, query: str, *args):
        self.fetch_calls.append((query, args))
        if "FROM sessions" in query and "BETWEEN" not in query:
            return [
                {
                    "thread_key": "slack:C123:1777910337.403889",
                    "sandbox_id": "asbx_1",
                    "harness_type": "codex",
                    "harness_thread_id": "harness_1",
                    "persona_id": "default",
                    "status": "idle",
                    "source": "slack",
                    "platform": "slack",
                    "external_thread_id": "1777910337.403889",
                    "created_at": self.now,
                    "updated_at": self.now,
                }
            ]
        if "FROM session_executions" in query:
            return [
                {
                    "execution_id": "exe_1",
                    "thread_key": "slack:C123:1777910337.403889",
                    "status": "completed",
                    "model": "gpt-test",
                    "created_at": self.now,
                    "started_at": self.now,
                    "completed_at": self.now,
                    "duration_seconds": 42.0,
                }
            ]
        if "FROM session_messages" in query:
            return [
                {
                    "message_id": "msg_1",
                    "thread_key": "slack:C123:1777910337.403889",
                    "role": "user",
                    "part_count": 1,
                    "part_types": ["text"],
                    "source": "slack",
                    "platform": "slack",
                    "created_at": self.now,
                }
            ]
        if "FROM session_events" in query:
            return [
                {
                    "event_id": 1,
                    "thread_key": "slack:C123:1777910337.403889",
                    "execution_id": "exe_1",
                    "event_type": "session.execution_completed",
                    "payload_type": "result",
                    "payload_keys": ["type", "status"],
                    "has_error": False,
                    "created_at": self.now,
                }
            ]
        if "FROM slack_sync_messages" in query:
            return [
                {
                    "channel_id": "C123",
                    "message_ts": "1777910337.403889",
                    "thread_ts": "1777910337.403889",
                    "is_thread_root": True,
                    "user_id": "U123",
                    "message_type": "message",
                    "reply_count": 2,
                    "updated_at": self.now,
                }
            ]
        if "FROM slack_sync_message_attachments" in query:
            return [
                {
                    "channel_id": "C123",
                    "message_ts": "1777910337.403889",
                    "slack_file_id": "F123",
                    "name": "debug.log",
                    "mimetype": "text/plain",
                    "size_bytes": 100,
                    "download_status": "metadata_only",
                    "has_content_hash": False,
                    "updated_at": self.now,
                }
            ]
        if "FROM slack_sync_backfill_jobs" in query:
            return []
        if "FROM slack_sync_runs" in query:
            return []
        if "FROM sessions" in query and "BETWEEN" in query:
            return []
        if "FROM agent_runtime_assignments" in query:
            raise RuntimeError("relation does not exist")
        if "FROM agent_execution_requests" in query:
            raise RuntimeError("relation does not exist")
        if "FROM sandbox_sessions" in query:
            raise RuntimeError("relation does not exist")
        if "FROM thread_traces" in query:
            raise RuntimeError("relation does not exist")
        return []

    async def close(self) -> None:
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


def test_investigation_queries_readonly_tables_without_message_context(monkeypatch) -> None:
    fake = _FakeConnection()

    async def fake_connect(*args, **kwargs):
        return fake

    monkeypatch.setattr(centaur_client.asyncpg, "connect", fake_connect)

    result = CentaurInvestigatorClient("postgresql://example").investigate_slack_thread(
        "https://example.slack.com/archives/C123/p1777910337403889",
        include_observability=False,
    )

    assert result["status"] == "ok"
    assert result["postgres"]["status"] == "ok"
    assert result["postgres"]["role"] == "centaur_readonly"
    assert result["postgres"]["connection"]["row"]["current_user"] == "centaur_readonly"
    assert result["analysis"]["primary_source"] == "postgres_readonly_tables"
    assert fake.execute_calls == ["SET ROLE centaur_readonly"]
    assert fake.closed is True

    all_queries = "\n".join(query for query, _args in fake.fetch_calls + fake.fetchrow_calls)
    assert "centaur_readonly_" not in all_queries
    assert "SELECT *" not in all_queries
    assert "FROM sessions" in all_queries
    assert "FROM session_messages" in all_queries
    assert "FROM slack_sync_messages" in all_queries

    assert "raw_payload" not in str(result)
    assert "url_private" not in str(result)
    assert "content_bytes" not in str(result)
    assert "secret user message" not in str(result)


def test_observability_never_requests_raw_log_context(monkeypatch) -> None:
    fake = _FakeConnection()

    async def fake_connect(*args, **kwargs):
        return fake

    class FakeVlogs:
        def hits(self, query: str, step: str | None = None) -> dict:
            return {"query": query, "step": step, "hits": []}

        def field_values(self, field: str, query: str = "*", limit: int = 100) -> list[str]:
            if field == "event":
                return ["message_stored", "execute_completed"]
            return ["api"]

        def tool_usage_by_thread(
            self,
            thread_key: str = "",
            start: str = "24h",
            limit: int = 200,
        ) -> list[dict]:
            return [
                {
                    "_time": "2026-06-17T00:00:00Z",
                    "tool_name": "github",
                    "tool_method": "search",
                    "duration_ms": "42",
                    "success": "true",
                }
            ]

        def thread_trace(self, *args, **kwargs):
            raise AssertionError("raw thread trace should not be requested")

        def errors(self, *args, **kwargs):
            raise AssertionError("raw error logs should not be requested")

        def execution_timeline(self, *args, **kwargs):
            raise AssertionError("raw execution logs should not be requested")

    def fake_load_module(module_name: str, path: Path):
        if "vlogs" in str(path):
            return SimpleNamespace(VictoriaLogsClient=FakeVlogs)
        return None

    monkeypatch.setattr(centaur_client.asyncpg, "connect", fake_connect)
    monkeypatch.setattr(centaur_client, "_safe_load_module", fake_load_module)

    result = CentaurInvestigatorClient("postgresql://example").investigate_slack_thread(
        "https://example.slack.com/archives/C123/p1777910337403889",
        include_observability=True,
    )

    assert result["status"] == "ok"
    assert result["observability"]["vlogs"]["status"] == "ok"
    assert "thread_trace" not in str(result["observability"])
    assert "execution_logs" not in str(result["observability"])
    assert "raw_payload" not in str(result)
