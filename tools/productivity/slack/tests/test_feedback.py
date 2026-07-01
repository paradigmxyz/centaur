from __future__ import annotations

import json
from datetime import datetime, timezone

from slack import feedback


def _sample_item(*, category: str = "cli_bug", severity: str = "high") -> feedback.FeedbackItem:
    now = datetime.now(timezone.utc).isoformat()
    return feedback.FeedbackItem(
        id=None,
        slack_channel="test-bot",
        slack_thread_ts=f"12345.{category}",
        permalink="https://slack.com/archives/C123/p12345",
        amp_thread_id=None,
        category=category,
        severity=severity,
        summary=f"{category} summary",
        cli_involved="slack",
        evidence={"bot_error": category == "cli_bug"},
        reporter_user="alice",
        status="new",
        created_at=now,
        updated_at=now,
    )


def test_mark_feedback_items_dispatched_updates_agent_tracking(tmp_path, monkeypatch):
    monkeypatch.setattr(feedback, "FEEDBACK_DB_PATH", tmp_path / "feedback.db")

    conn = feedback.init_db()
    save_result = feedback.save_feedback_item(conn, _sample_item())
    conn.close()

    feedback.mark_feedback_items_dispatched([save_result.item_id], "thread-123", "exec-456")

    conn = feedback.init_db()
    row = conn.execute(
        "SELECT status, agent_thread_key, agent_execution_id, dispatch_count "
        "FROM feedback_items WHERE id = ?",
        (save_result.item_id,),
    ).fetchone()
    conn.close()

    assert row["status"] == "in_progress"
    assert row["agent_thread_key"] == "thread-123"
    assert row["agent_execution_id"] == "exec-456"
    assert row["dispatch_count"] == 1


def test_run_improvement_cycle_dispatches_only_actionable_items(tmp_path, monkeypatch):
    monkeypatch.setattr(feedback, "FEEDBACK_DB_PATH", tmp_path / "feedback.db")

    conn = feedback.init_db()
    actionable_id = feedback.save_feedback_item(
        conn, _sample_item(category="cli_bug", severity="high")
    ).item_id
    success_id = feedback.save_feedback_item(
        conn, _sample_item(category="success", severity="low")
    ).item_id
    conn.close()

    monkeypatch.setattr(
        feedback,
        "collect_feedback",
        lambda **_: {
            "channels_scanned": 1,
            "threads_analyzed": 2,
            "feedback_items_created": 0,
            "feedback_items_updated": 0,
            "by_category": {},
            "by_severity": {},
        },
    )

    class FakeAgentClient:
        def start_improvement_run(self, prompt: str, **kwargs):
            assert "git-branch paradigmxyz/centaur" in prompt
            return {
                "thread_key": "feedback-improvement:test",
                "execution_id": "exec-test-123",
                "status": "queued",
            }

    result = feedback.run_improvement_cycle(
        channels=["test-bot"],
        since_days=7,
        limit_per_channel=None,
        max_items=5,
        agent_client=FakeAgentClient(),
    )

    assert result["dispatched"] is True
    assert result["actionable_items"] == 1
    assert result["item_ids"] == [actionable_id]
    assert success_id != actionable_id

    conn = feedback.init_db()
    rows = conn.execute(
        "SELECT id, status, agent_execution_id FROM feedback_items ORDER BY id ASC"
    ).fetchall()
    conn.close()

    row_by_id = {row["id"]: row for row in rows}
    assert row_by_id[actionable_id]["status"] == "in_progress"
    assert row_by_id[actionable_id]["agent_execution_id"] == "exec-test-123"
    assert row_by_id[success_id]["status"] == "new"
    assert row_by_id[success_id]["agent_execution_id"] is None


def test_agent_client_uses_session_api_for_improvement_runs():
    class RecordingAgentClient(feedback.CentaurAgentClient):
        def __init__(self):
            super().__init__(base_url="http://api.local", api_key="test-key")
            self.calls = []

        def _request_json(self, method: str, path: str, payload: dict | None = None):
            self.calls.append((method, path, payload))
            if path.endswith("/execute"):
                return {"execution_id": "exec-test-123", "status": "queued"}
            return {"ok": True}

    client = RecordingAgentClient()

    result = client.start_improvement_run(
        "Improve the Slack tool",
        harness="codex",
        persona_id="eng",
        thread_key="feedback-improvement:test:1",
    )

    assert result == {
        "thread_key": "feedback-improvement:test:1",
        "execution_id": "exec-test-123",
        "status": "queued",
    }
    assert [path for _, path, _ in client.calls] == [
        "/api/session/feedback-improvement%3Atest%3A1",
        "/api/session/feedback-improvement%3Atest%3A1/messages",
        "/api/session/feedback-improvement%3Atest%3A1/execute",
    ]
    assert client.calls[0][2] == {
        "harness_type": "codex",
        "persona_id": "eng",
        "metadata": {"source": "slack-feedback-loop"},
        "on_harness_conflict": "restart",
    }
    execute_payload = client.calls[2][2]
    assert execute_payload["metadata"] == {
        "source": "slack-feedback-loop",
        "delivery": {"platform": "dev"},
    }
    assert json.loads(execute_payload["input_lines"][0]) == {
        "type": "user",
        "message": {"content": [{"type": "text", "text": "Improve the Slack tool"}]},
    }


def test_analyze_thread_signals_does_not_treat_exceptional_as_error():
    messages = [
        {"user": "arjun", "text": "@centaur_ai --invest are L2s still investable or cooked"},
        {
            "user": "centaur_ai",
            "bot_id": "B123",
            "text": "This is an exceptional business with strong unit economics.",
        },
    ]

    signals = feedback.analyze_thread_signals(messages)

    assert signals.has_bot_error is False


def test_classify_feedback_prefers_success_for_positive_follow_up_without_error():
    messages = [
        {"user": "arjun", "text": "@centaur_ai --invest dig into this co"},
        {
            "user": "centaur_ai",
            "bot_id": "B123",
            "text": "This is an exceptional manufacturing business with strong margins.",
            "reactions": [{"name": "thumbsup"}],
        },
        {"user": "arjun", "text": "better than the vanilla thread we got before imo"},
        {"user": "arjun", "text": "could be tighter"},
        {"user": "arjun", "text": "synthesis seems better to me"},
    ]

    signals = feedback.analyze_thread_signals(messages)
    category, severity = feedback.classify_feedback(signals, messages)

    assert signals.repeated_requests is True
    assert signals.has_bot_error is False
    assert category == "success"
    assert severity == "low"
