"""Tests for the sandbox_reaper_report workflow — pure aggregation + render."""

from __future__ import annotations

import datetime as dt
import inspect

import pytest

from api.workflow_engine import Delivery
from api.workflows import sandbox_reaper_report as wf

NOW = dt.datetime(2026, 5, 25, 12, 0, 0, tzinfo=dt.timezone.utc)


def _row(state, harness, started_h_ago, updated_h_ago, sandbox_id="sbx-abcdef123456"):
    return {
        "thread_key": f"slack:T:C:{sandbox_id}",
        "sandbox_id": sandbox_id,
        "harness": harness,
        "engine": harness,
        "state": state,
        "started_at": NOW - dt.timedelta(hours=started_h_ago),
        "updated_at": NOW - dt.timedelta(hours=updated_h_ago),
    }


# ── registration ──────────────────────────────────────────────────────────────


def test_workflow_exports():
    assert wf.WORKFLOW_NAME == "sandbox_reaper_report"
    assert isinstance(wf.SCHEDULE, dict)
    assert wf.SCHEDULE.get("cron")
    assert inspect.iscoroutinefunction(wf.handler)


# ── bucketing ──────────────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    "hours,bucket",
    [(0.5, "<1h"), (1.0, "1-6h"), (5.9, "1-6h"), (6.0, "6-24h"), (23.9, "6-24h"), (24.0, ">24h"), (100, ">24h")],
)
def test_bucket_for_age(hours, bucket):
    assert wf._bucket_for_age(hours * 3600) == bucket


# ── aggregation ─────────────────────────────────────────────────────────────────


def test_aggregate_mixed_fleet():
    rows = [
        _row("running", "codex", started_h_ago=2, updated_h_ago=0.1, sandbox_id="run-1"),
        _row("idle", "hermes", started_h_ago=30, updated_h_ago=8, sandbox_id="idle-1"),
        _row("running", "hermes", started_h_ago=0.4, updated_h_ago=0.05, sandbox_id="run-2"),
        _row("error", "codex", started_h_ago=50, updated_h_ago=49, sandbox_id="err-1"),
        _row("gone", "amp", started_h_ago=100, updated_h_ago=99, sandbox_id="gone-1"),
    ]
    snap = wf._aggregate(rows, NOW)

    assert snap["total"] == 5
    assert snap["active_total"] == 3  # running, idle, running
    assert snap["terminal_total"] == 2  # error, gone
    assert snap["long_idle_total"] == 1  # idle session updated 8h ago

    assert snap["by_state"] == {"running": 2, "idle": 1, "error": 1, "gone": 1}
    assert snap["by_harness"] == {"codex": 2, "hermes": 2, "amp": 1}

    # Active-only age buckets: 2h→1-6h, 30h→>24h, 0.4h→<1h
    assert snap["age_buckets"] == {"<1h": 1, "1-6h": 1, ">24h": 1}

    # Cost = (2 + 30 + 0.4) * 0.05 = 1.62 (terminal sessions excluded)
    assert snap["est_cost_usd"] == pytest.approx(1.62, abs=0.011)

    # long-idle list carries the idle session, sorted by idle desc
    assert [e["sandbox_id"] for e in snap["long_idle"]] == ["idle-1"]
    assert snap["long_idle"][0]["state"] == "idle"
    # terminal list carries the two terminal sessions, oldest first
    assert {e["sandbox_id"] for e in snap["terminal"]} == {"err-1", "gone-1"}


def test_aggregate_empty():
    snap = wf._aggregate([], NOW)
    assert snap["total"] == 0
    assert snap["active_total"] == 0
    assert snap["est_cost_usd"] == 0.0
    assert snap["long_idle"] == [] and snap["terminal"] == []


def test_aggregate_handles_missing_timestamps():
    rows = [{"state": "running", "harness": "codex", "sandbox_id": "x", "started_at": None, "updated_at": None}]
    snap = wf._aggregate(rows, NOW)
    assert snap["active_total"] == 1
    assert snap["age_buckets"] == {}  # no age → no bucket
    assert snap["est_cost_usd"] == 0.0


# ── rendering ───────────────────────────────────────────────────────────────────


def test_render_report_contains_key_fields():
    rows = [
        _row("running", "codex", 2, 0.1, sandbox_id="run-1"),
        _row("idle", "hermes", 30, 8, sandbox_id="idle-1"),
        _row("error", "codex", 50, 49, sandbox_id="err-1"),
    ]
    text = wf._render_report(wf._aggregate(rows, NOW))
    assert "Sandbox fleet report" in text
    assert "Active sandboxes: *2*" in text
    assert "Long-idle" in text and "idle-1" in text
    assert "Terminal/reapable" in text and "err-1" in text
    assert "Estimated active spend" in text


def test_render_report_clean_fleet():
    text = wf._render_report(wf._aggregate([_row("running", "codex", 1, 0.1)], NOW))
    assert "fleet looks clean" in text


# ── channel resolution ───────────────────────────────────────────────────────────


def test_resolve_channel_from_delivery():
    inp = wf.Input(delivery=Delivery(platform="slack", channel="#ops-sandboxes"))
    assert wf._resolve_channel(inp) == "ops-sandboxes"


def test_resolve_channel_from_slack_channel_field():
    inp = wf.Input(slack_channel="team-infra")
    assert wf._resolve_channel(inp) == "team-infra"


def test_resolve_channel_from_env(monkeypatch):
    monkeypatch.setenv("SANDBOX_REPORT_SLACK_CHANNEL", "#fallback")
    assert wf._resolve_channel(wf.Input()) == "fallback"


def test_resolve_channel_empty(monkeypatch):
    monkeypatch.delenv("SANDBOX_REPORT_SLACK_CHANNEL", raising=False)
    assert wf._resolve_channel(wf.Input()) == ""
