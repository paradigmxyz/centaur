from __future__ import annotations

import sys
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import client as centaur_client
from client import CentaurInvestigatorClient, parse_slack_reference


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


def test_investigation_never_returns_message_context() -> None:
    result = CentaurInvestigatorClient().investigate_slack_thread(
        "https://example.slack.com/archives/C123/p1777910337403889",
        include_observability=False,
    )

    assert result["status"] == "ok"
    assert result["postgres"]["status"] == "role_only"
    assert "session_messages" not in str(result)
    assert "session_events" not in str(result)
    assert "slack_sync_messages" not in str(result)
    assert "attachments" not in str(result)
    assert "raw_payload" not in str(result)
    assert result["analysis"]["primary_source"] == "identifiers_and_observability"


def test_observability_never_requests_raw_log_context(monkeypatch) -> None:
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

    monkeypatch.setattr(centaur_client, "_safe_load_module", fake_load_module)

    result = CentaurInvestigatorClient().investigate_slack_thread(
        "https://example.slack.com/archives/C123/p1777910337403889",
        include_observability=True,
    )

    assert result["status"] == "ok"
    assert result["observability"]["vlogs"]["status"] == "ok"
    assert "thread_trace" not in str(result)
    assert "execution_logs" not in str(result)
    assert "raw_payload" not in str(result)
