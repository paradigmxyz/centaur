from __future__ import annotations

import datetime as dt

import pytest
from fastapi import HTTPException

from api.routers import monitoring


class Row(dict):
    def __getattr__(self, name: str):
        return self[name]


def test_window_validation_accepts_expected_values() -> None:
    assert monitoring._window_seconds("24h") == 86_400
    assert monitoring._window_seconds("7d") == 604_800
    assert monitoring._window_seconds("30d") == 2_592_000


def test_window_validation_rejects_unknown_value() -> None:
    with pytest.raises(HTTPException):
        monitoring._window_seconds("90d")


def test_execution_item_is_metadata_first() -> None:
    now = dt.datetime(2026, 5, 26, 12, 0, tzinfo=dt.timezone.utc)
    item = monitoring._execution_item(
        Row(
            event_id=12,
            execution_id="exec-1",
            thread_key="slack:C:123",
            created_at=now,
            requested_at=now,
            started_at=now,
            completed_at=now,
            request_status="completed",
            terminal_reason="done",
            event_json={
                "status": "completed",
                "terminal_reason": "done",
                "harness": "codex",
                "engine": "codex",
                "persona_id": "eng",
                "user_id": "U123",
                "duration_s": 4.2,
                "total_tokens": 1000,
                "cost_usd": 0.1234567,
                "assistant_tool_use_events": 3,
                "tool_error_events": 1,
                "models": ["gpt-5"],
                "tool_calls_by_name": {"websearch": 2},
                "tool_errors_by_name": {"websearch": 1},
                "raw_text_that_must_not_leak": "secret prompt",
            },
        )
    )

    assert item["execution_id"] == "exec-1"
    assert item["status"] == "completed"
    assert item["cost_usd"] == 0.123457
    assert item["tool_calls_by_name"] == {"websearch": 2}
    assert "raw_text_that_must_not_leak" not in item


def test_tool_event_item_pairs_use_and_result_without_raw_payloads() -> None:
    now = dt.datetime(2026, 5, 26, 12, 0, tzinfo=dt.timezone.utc)
    item = monitoring._tool_event_item(
        Row(
            use_event_id=7,
            result_event_id=8,
            thread_key="slack:C:123",
            execution_id="exec-1",
            use_created_at=now,
            result_created_at=now,
            use_json={
                "tool_use_id": "toolu-1",
                "tool_name": "linear",
                "input_keys": ["query"],
                "input_size_bytes": 42,
                "harness": "codex",
            },
            result_json={
                "tool_use_id": "toolu-1",
                "is_error": True,
                "error_category": "rate_limit",
                "content_size_bytes": 128,
                "content": "raw content should not be returned",
            },
            summary_json={"user_id": "U123", "engine": "codex"},
        )
    )

    assert item["tool_name"] == "linear"
    assert item["result_status"] == "error"
    assert item["error_category"] == "rate_limit"
    assert item["user_id"] == "U123"
    assert "content" not in item


def test_timeline_summary_allows_only_known_metadata_keys() -> None:
    item = monitoring._timeline_item(
        Row(
            event_id=1,
            execution_id="exec-1",
            event_kind="command_execution_observed",
            created_at=dt.datetime(2026, 5, 26, 12, 0, tzinfo=dt.timezone.utc),
            event_json={
                "type": "obs.command_execution",
                "command_size_bytes": 18,
                "output_size_bytes": 20,
                "command": "cat private.txt",
            },
        )
    )

    assert item["summary"] == {
        "type": "obs.command_execution",
        "command_size_bytes": 18,
        "output_size_bytes": 20,
    }
