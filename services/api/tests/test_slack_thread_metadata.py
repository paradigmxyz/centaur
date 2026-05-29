"""Unit tests for slack thread_key parsing in runtime_control metadata."""

from __future__ import annotations

from api.runtime_control import _slack_thread_metadata


def test_team_scoped_thread_key() -> None:
    # slack:<team>:<channel>:<thread_ts> — the format the slackbot emits.
    assert _slack_thread_metadata(
        "slack:T0AQQ46PL4C:C0B0XS7BLA3:1780035646.228899"
    ) == {
        "slack_team_id": "T0AQQ46PL4C",
        "slack_channel_id": "C0B0XS7BLA3",
        "slack_thread_ts": "1780035646.228899",
    }


def test_legacy_thread_key_without_team() -> None:
    assert _slack_thread_metadata("slack:C0B0XS7BLA3:1780035646.228899") == {
        "slack_channel_id": "C0B0XS7BLA3",
        "slack_thread_ts": "1780035646.228899",
    }


def test_non_slack_thread_key() -> None:
    assert _slack_thread_metadata("task:do-something-123") == {}
