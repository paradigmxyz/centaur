"""Unit tests for `--slack-card` and the Slack hint on search/answer. No network."""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from tools.research.tako import cli
from tools.research.tako._coverage import DOMAINS, TOOL_COMMAND

PUB = "m-p-JX5qxsBStJgywrqI"

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


class TestHint:
    """The hint is the only nudge; this tool renders but never posts."""

    def test_names_the_pipeline_that_posts(self, capsys):
        cli.emit_or_reject(lambda: RESULT)
        hint = _emitted(capsys)["slack_card"]["hint"]
        assert f"{TOOL_COMMAND} slack-card {PUB}" in hint
        assert "slack send" in hint
        assert "--blocks-json -" in hint

    def test_points_at_the_session_context_ids(self, capsys):
        # The sandbox is never told its thread, so the hint names where the
        # caller finds the destination rather than guessing it.
        cli.emit_or_reject(lambda: RESULT)
        hint = _emitted(capsys)["slack_card"]["hint"]
        assert "session_context.slack.channel_id" in hint
        assert "session_context.slack.thread_ts" in hint

    def test_is_phrased_conditionally_for_non_slack_surfaces(self, capsys):
        # Discord, Teams, Linear and GitHub turns get this output too, and the
        # tool cannot tell which surface it is on.
        cli.emit_or_reject(lambda: RESULT)
        assert "if this turn is in a Slack thread" in _emitted(capsys)["slack_card"]["hint"]

    def test_no_note_when_there_is_no_card(self, capsys):
        cli.emit_or_reject(lambda: WEB_ONLY)
        assert "slack_card" not in _emitted(capsys)

    def test_the_result_itself_is_untouched(self, capsys):
        cli.emit_or_reject(lambda: RESULT)
        emitted = _emitted(capsys)
        assert emitted["cards"] == RESULT["cards"]
        assert emitted["meta"] == RESULT["meta"]

    def test_the_note_is_the_first_key(self, capsys):
        cli.emit_or_reject(lambda: RESULT)
        assert next(iter(_emitted(capsys))) == "slack_card"

    def test_the_note_survives_head_truncation(self, capsys):
        # Callers pipe this through `head -N`. A search result is well over a
        # hundred lines of JSON, so a note appended at the end is never read.
        cli.emit_or_reject(lambda: _big_result())
        first_lines = capsys.readouterr().out.splitlines()[:6]
        assert any("slack_card" in line for line in first_lines)


class TestSlackCardFlag:
    """`--slack-card` adds the ready-to-post message; it never sends."""

    def test_includes_the_rendered_message(self, capsys):
        cli.emit_or_reject(lambda: RESULT, slack_card=True)
        note = _emitted(capsys)["slack_card"]
        assert note["message"]["blocks"][0]["type"] == "container"
        assert note["message"]["text"] == CARD["title"]

    def test_omits_the_message_without_the_flag(self, capsys):
        cli.emit_or_reject(lambda: RESULT)
        assert "message" not in _emitted(capsys)["slack_card"]

    def test_the_message_carries_no_channel_or_thread(self, capsys):
        # The posting tool supplies the destination, so the payload must not
        # pin one here.
        cli.emit_or_reject(lambda: RESULT, slack_card=True)
        message = _emitted(capsys)["slack_card"]["message"]
        assert "channel" not in message
        assert "thread_ts" not in message

    def test_flat_switches_the_layout(self, capsys):
        cli.emit_or_reject(lambda: RESULT, slack_card=True, flat=True)
        blocks = _emitted(capsys)["slack_card"]["message"]["blocks"]
        assert [b["type"] for b in blocks] == ["image", "context", "actions"]

    def test_the_hint_is_present_alongside_the_message(self, capsys):
        cli.emit_or_reject(lambda: RESULT, slack_card=True)
        note = _emitted(capsys)["slack_card"]
        assert "hint" in note
        assert "message" in note

    def test_the_flag_does_nothing_for_web_only_results(self, capsys):
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
    return json.loads(capsys.readouterr().out)
