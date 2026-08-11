"""Unit tests for `--slack-card` and the Slack hint on search/answer. No network."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from tools.research.tako import cli
from tools.research.tako._coverage import DOMAINS, TOOL_COMMAND

PUB = "m-p-JX5qxsBStJgywrqI"
THREAD_KEY = "slack:T03322H91D4:C0BL2RK9329:1786469053.584729"

CARD = {
    "card_id": PUB,
    "title": "Apple Inc. Total Revenues (Annual)",
    "description": "latest value was $416.2B on Sep 27, 2025.",
    "webpage_url": f"https://tako.com/card/{PUB}/",
    "image_url": f"https://tako.com/api/v1/image/{PUB}/",
    "sources": [{"source_name": "Fiscal.ai"}],
}
RESULT = {"cards": [CARD], "web_results": [], "meta": {"backend": "tako:sdk"}}
WEB_ONLY = {"cards": [], "web_results": [{"url": "https://example.com"}], "meta": {}}


@pytest.fixture
def in_slack_thread(monkeypatch):
    monkeypatch.setenv("CENTAUR_THREAD_KEY", THREAD_KEY)


@pytest.fixture
def posted(monkeypatch):
    """Capture what would have been posted instead of calling Slack."""
    sent = {}

    def fake_post(body, **kwargs):
        sent["body"] = body
        return {"ok": True, "channel": body["channel"], "ts": "1786470000.000100"}

    monkeypatch.setattr("tools.research.tako._slack.post_message", fake_post)
    return sent


class TestHint:
    def test_hint_appears_in_a_slack_thread_with_a_card(self, in_slack_thread, capsys):
        cli.emit_or_reject(lambda: RESULT)
        note = _emitted(capsys)["slack_card"]
        assert note["posted"] is False
        assert "--slack-card" in note["hint"]
        assert f"{TOOL_COMMAND} slack-card {PUB} --post" in note["hint"]

    def test_no_hint_outside_a_slack_thread(self, monkeypatch, capsys):
        monkeypatch.delenv("CENTAUR_THREAD_KEY", raising=False)
        cli.emit_or_reject(lambda: RESULT)
        assert "slack_card" not in _emitted(capsys)

    def test_no_hint_for_a_non_slack_source(self, monkeypatch, capsys):
        monkeypatch.setenv("CENTAUR_THREAD_KEY", "discord:C0456DEF:1754870000.001200")
        cli.emit_or_reject(lambda: RESULT)
        assert "slack_card" not in _emitted(capsys)

    def test_no_hint_when_there_is_no_card(self, in_slack_thread, capsys):
        cli.emit_or_reject(lambda: WEB_ONLY)
        assert "slack_card" not in _emitted(capsys)

    def test_the_result_itself_is_untouched(self, in_slack_thread, capsys):
        cli.emit_or_reject(lambda: RESULT)
        emitted = _emitted(capsys)
        assert emitted["cards"] == RESULT["cards"]
        assert emitted["meta"] == RESULT["meta"]


class TestSlackCardFlag:
    def test_posts_the_lead_card_into_the_thread(self, in_slack_thread, posted, capsys):
        cli.emit_or_reject(lambda: RESULT, slack_card=True)
        note = _emitted(capsys)["slack_card"]
        assert note["posted"] is True
        assert note["ts"] == "1786470000.000100"
        assert note["layout"] == "container"
        body = posted["body"]
        assert body["channel"] == "C0BL2RK9329"
        assert body["thread_ts"] == "1786469053.584729"
        assert body["blocks"][0]["type"] == "container"

    def test_flat_switches_the_layout(self, in_slack_thread, posted, capsys):
        cli.emit_or_reject(lambda: RESULT, slack_card=True, flat=True)
        assert _emitted(capsys)["slack_card"]["layout"] == "flat"
        assert [b["type"] for b in posted["body"]["blocks"]] == ["image", "context", "actions"]

    def test_a_failed_post_still_prints_the_data(self, in_slack_thread, monkeypatch, capsys):
        from tools.research.tako._slack import SlackPostError

        def boom(body, **kwargs):
            raise SlackPostError("Slack rejected the post: not_in_channel")

        monkeypatch.setattr("tools.research.tako._slack.post_message", boom)
        cli.emit_or_reject(lambda: RESULT, slack_card=True)
        emitted = _emitted(capsys)
        # The retrieved data must survive a posting failure.
        assert emitted["cards"] == RESULT["cards"]
        assert emitted["slack_card"]["posted"] is False
        assert "not_in_channel" in emitted["slack_card"]["error"]

    def test_the_flag_is_inert_outside_a_slack_thread(self, monkeypatch, capsys):
        monkeypatch.delenv("CENTAUR_THREAD_KEY", raising=False)
        monkeypatch.setattr(
            "tools.research.tako._slack.post_message",
            lambda *a, **k: pytest.fail("must not post without a Slack thread"),
        )
        cli.emit_or_reject(lambda: RESULT, slack_card=True)
        assert "slack_card" not in _emitted(capsys)

    def test_the_flag_does_nothing_for_web_only_results(self, in_slack_thread, monkeypatch, capsys):
        monkeypatch.setattr(
            "tools.research.tako._slack.post_message",
            lambda *a, **k: pytest.fail("must not post without a card"),
        )
        cli.emit_or_reject(lambda: WEB_ONLY, slack_card=True)
        assert "slack_card" not in _emitted(capsys)


class TestHelpDescribesDomainsNotSources:
    def test_help_lists_every_domain(self):
        help_text = cli.app.info.help
        for domain in DOMAINS:
            assert domain in help_text

    def test_help_names_no_licensed_source(self):
        # Sources change without coverage changing, and naming three made the
        # tool read as narrower than it is.
        help_text = cli.app.info.help.lower()
        for source in ("s&p", "fred", "similarweb", "fiscal.ai"):
            assert source not in help_text

    def test_help_points_at_the_free_coverage_probe(self):
        assert f"{TOOL_COMMAND} available-data" in cli.app.info.help


def _emitted(capsys) -> dict:
    import json

    return json.loads(capsys.readouterr().out)
