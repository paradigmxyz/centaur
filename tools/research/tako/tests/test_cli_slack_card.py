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
def no_thread_env(monkeypatch):
    """A warm-pool sandbox: no thread identity in the environment."""
    monkeypatch.delenv("CENTAUR_THREAD_KEY", raising=False)


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
    def test_hint_appears_with_a_card_and_a_known_destination(self, in_slack_thread, capsys):
        cli.emit_or_reject(lambda: RESULT)
        note = _emitted(capsys)["slack_card"]
        assert note["posted"] is False
        assert "--slack-card" in note["hint"]
        assert f"{TOOL_COMMAND} slack-card {PUB} --post" in note["hint"]

    def test_hint_still_appears_without_a_destination(self, no_thread_env, capsys):
        # A warm-pool sandbox has no CENTAUR_THREAD_KEY, which is the normal case.
        # Suppressing the hint here would mean never showing it at all, so it is
        # shown conditionally and names the options the caller must pass.
        cli.emit_or_reject(lambda: RESULT)
        hint = _emitted(capsys)["slack_card"]["hint"]
        assert "if this turn is in a Slack thread" in hint
        assert f"--channel <{cli.SESSION_CONTEXT_CHANNEL}>" in hint
        assert f"--thread <{cli.SESSION_CONTEXT_THREAD}>" in hint

    def test_a_non_slack_thread_key_falls_back_to_the_generic_hint(self, monkeypatch, capsys):
        monkeypatch.setenv("CENTAUR_THREAD_KEY", "discord:C0456DEF:1754870000.001200")
        cli.emit_or_reject(lambda: RESULT)
        assert "if this turn is in a Slack thread" in _emitted(capsys)["slack_card"]["hint"]

    def test_no_hint_when_there_is_no_card(self, in_slack_thread, capsys):
        cli.emit_or_reject(lambda: WEB_ONLY)
        assert "slack_card" not in _emitted(capsys)

    def test_the_result_itself_is_untouched(self, in_slack_thread, capsys):
        cli.emit_or_reject(lambda: RESULT)
        emitted = _emitted(capsys)
        assert emitted["cards"] == RESULT["cards"]
        assert emitted["meta"] == RESULT["meta"]

    def test_the_note_is_the_first_key(self, in_slack_thread, capsys):
        cli.emit_or_reject(lambda: RESULT)
        assert next(iter(_emitted(capsys))) == "slack_card"

    def test_the_note_survives_head_truncation(self, in_slack_thread, capsys):
        # Callers pipe this through `head -N`. A search result is well over a
        # hundred lines of JSON, so a note appended at the end is never read.
        cli.emit_or_reject(lambda: _big_result())
        first_lines = capsys.readouterr().out.splitlines()[:6]
        assert any("slack_card" in line for line in first_lines)


class TestExplicitDestination:
    """The real path: the caller passes the IDs from the turn's prompt."""

    def test_channel_and_thread_options_target_the_post(self, no_thread_env, posted, capsys):
        cli.emit_or_reject(
            lambda: RESULT,
            slack_card=True,
            channel="C0BL2RK9329",
            thread="1786469053.584729",
        )
        note = _emitted(capsys)["slack_card"]
        assert note["posted"] is True
        body = posted["body"]
        assert body["channel"] == "C0BL2RK9329"
        assert body["thread_ts"] == "1786469053.584729"

    def test_explicit_channel_overrides_the_environment(self, in_slack_thread, posted, capsys):
        cli.emit_or_reject(lambda: RESULT, slack_card=True, channel="C_OVERRIDE")
        assert posted["body"]["channel"] == "C_OVERRIDE"

    def test_channel_without_thread_posts_to_the_channel(self, no_thread_env, posted, capsys):
        cli.emit_or_reject(lambda: RESULT, slack_card=True, channel="C0BL2RK9329")
        body = posted["body"]
        assert body["channel"] == "C0BL2RK9329"
        assert "thread_ts" not in body

    def test_post_without_a_destination_explains_what_to_pass(self, no_thread_env, capsys):
        cli.emit_or_reject(lambda: RESULT, slack_card=True)
        note = _emitted(capsys)["slack_card"]
        assert note["posted"] is False
        assert cli.SESSION_CONTEXT_CHANNEL in note["error"]
        assert cli.SESSION_CONTEXT_THREAD in note["error"]


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

    def test_the_flag_does_not_post_without_a_destination(self, no_thread_env, monkeypatch, capsys):
        monkeypatch.setattr(
            "tools.research.tako._slack.post_message",
            lambda *a, **k: pytest.fail("must not post without a destination"),
        )
        cli.emit_or_reject(lambda: RESULT, slack_card=True)
        assert _emitted(capsys)["slack_card"]["posted"] is False

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


def _big_result() -> dict:
    """A realistic multi-card search result: ~160 lines of JSON when emitted."""

    def card(i):
        pub = f"m-p-CARD{i:04d}exampleid"
        return {
            "card_id": pub,
            "title": f"Card {i} Total Revenues (Annual)",
            "description": "latest value was $416.2B on Sep 27, 2025, up 6.4%.",
            "exportable": True,
            "card_type": "chart",
            "webpage_url": f"https://tako.com/card/{pub}/",
            "image_url": f"https://tako.com/api/v1/image/{pub}/",
            "embed_url": f"https://tako.com/embed/{pub}/",
            "sources": [{"source_name": "Fiscal.ai", "source_index": "data"}],
            "data_freshness": {"data_as_of": "2025-09-27", "last_updated": "2026-08-05"},
        }

    return {
        "cards": [card(i) for i in range(1, 6)],
        "web_results": [{"title": f"R{i}", "url": f"https://example.com/{i}"} for i in range(1, 6)],
        "meta": {"backend": "tako:sdk", "partial_failures": []},
    }


def _emitted(capsys) -> dict:
    import json

    return json.loads(capsys.readouterr().out)
