"""Tests for huddle-transcript parsing and its failure modes.

The parser is pure, so it is tested against real payload shapes. The fetch is a thin shell, so what
matters about it is not the happy path but the *failures*: a lapsed session must be impossible to
mistake for a silent meeting, and a missing credential must say so rather than returning nothing.
"""

from __future__ import annotations

import pytest
from slack.huddle_transcript import (
    HuddleTranscriptError,
    parse_segments,
    render,
    speakers,
)


def _section(user: str | None, stamp: str | None, *words: str) -> dict:
    elements: list[dict] = []
    if user:
        elements.append({"type": "user", "user_id": user})
    if stamp:
        # Slack marks the timestamp with a BOLD text element — that is what distinguishes the marker
        # from speech, not its position.
        elements.append({"type": "text", "text": f" [{stamp}]: ", "style": {"bold": True}})
    elements += [{"type": "text", "text": w} for w in words]
    return {"type": "rich_text_section", "elements": elements}


def _payload(*sections: dict) -> dict:
    return {"file": {"huddle_transcription": {"blocks": {"elements": list(sections)}}}}


def test_it_reads_speaker_time_and_words():
    segments = parse_segments(
        _payload(
            _section("U1", "0:04", "we ship on Friday"),
            _section("U2", "0:11", "the migration is not done"),
        )
    )
    assert segments == [
        {"user_id": "U1", "at": "0:04", "text": "we ship on Friday"},
        {"user_id": "U2", "at": "0:11", "text": "the migration is not done"},
    ]


def test_a_long_huddle_keeps_its_hour():
    (segment,) = parse_segments(_payload(_section("U1", "1:02:33", "still here")))
    assert segment["at"] == "1:02:33"


def test_bold_speech_is_not_mistaken_for_the_timestamp():
    """The marker is the bold element that IS a timestamp. A bold word inside the speech is speech,
    and dropping it would silently delete words from the record."""
    section = _section("U1", "0:07", "we are ")
    section["elements"].append({"type": "text", "text": "not", "style": {"bold": True}})
    section["elements"].append({"type": "text", "text": " shipping"})
    (segment,) = parse_segments(_payload(section))
    assert segment["text"] == "we are not shipping"
    assert segment["at"] == "0:07"


def test_words_split_across_elements_are_rejoined():
    (segment,) = parse_segments(_payload(_section("U1", "0:01", "roll ", "it ", "back")))
    assert segment["text"] == "roll it back"


def test_an_empty_turn_is_dropped():
    """Slack emits sections with no words (a join, a noise blip). They are not turns."""
    assert parse_segments(_payload(_section("U1", "0:01", "   "))) == []


def test_a_speakerless_turn_survives():
    """Attribution can be missing; the words still happened and must not be discarded."""
    (segment,) = parse_segments(_payload(_section(None, "0:02", "someone said this")))
    assert segment["user_id"] is None
    assert segment["text"] == "someone said this"


def test_a_transcriptless_payload_is_empty_not_an_error():
    assert parse_segments({}) == []
    assert parse_segments({"file": {}}) == []
    assert parse_segments({"file": {"huddle_transcription": {}}}) == []


def test_non_section_blocks_are_ignored():
    payload = _payload({"type": "rich_text_preformatted", "elements": []})
    assert parse_segments(payload) == []


def test_render_uses_the_mention_form_so_a_quote_names_the_person():
    text = render([{"user_id": "U1", "at": "0:04", "text": "we ship Friday"}])
    assert text == "<@U1> [0:04]: we ship Friday"


def test_render_tolerates_a_missing_speaker_or_stamp():
    assert render([{"user_id": None, "at": None, "text": "hi"}]) == "someone: hi"


def test_speakers_are_first_spoke_order_and_deduplicated():
    segments = [
        {"user_id": "U2", "at": None, "text": "a"},
        {"user_id": "U1", "at": None, "text": "b"},
        {"user_id": "U2", "at": None, "text": "c"},
    ]
    assert speakers(segments) == ["U2", "U1"]


def test_the_error_payload_survives_stringification():
    """The tool boundary stringifies exceptions, so the structure has to live in the payload — a
    caller must still be able to tell `needs_reauth` from a bug after that round trip."""
    err = HuddleTranscriptError("session expired", slack_error="invalid_auth", needs_reauth=True)
    assert err.payload["needs_reauth"] is True
    assert err.payload["slack_error"] == "invalid_auth"
    assert "session expired" in str(err)


def test_a_missing_session_is_loud_rather_than_an_empty_transcript():
    """The failure that matters. A silent empty read looks exactly like "nobody spoke", and an agent
    that believes a meeting was silent is worse than one that admits it cannot see."""
    from slack.client import SlackClient

    client = SlackClient.__new__(SlackClient)  # no network, no bot token needed for this branch
    client.web_token = ""
    client.web_cookie = ""

    with pytest.raises(HuddleTranscriptError) as caught:
        client.get_huddle_transcript("F123")

    assert caught.value.payload["needs_reauth"] is True
    assert set(caught.value.payload["missing"]) == {"SLACK_WEB_TOKEN", "SLACK_WEB_COOKIE"}
