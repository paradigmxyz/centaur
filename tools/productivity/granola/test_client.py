import tomllib
from pathlib import Path

from client import (
    GranolaClient,
    _parse_meeting_date,
    _parse_meetings,
    _parse_participants,
)

from centaur_sdk.backends import StubBackend, configure


def test_parse_meetings_accepts_extra_attributes_and_decodes_entities():
    text = """
    <meetings_data count="1">
      <meeting id="meeting-1" title="Matt &lt;&gt; Daniel" date="Aug 5, 2026 5:00 PM CST"
               captured_by_me="true" listed_as_participant="true">
        <known_participants>Matt (note creator) from Acme &lt;matt@example.com&gt;, Daniel &lt;daniel@example.com&gt;</known_participants>
        <summary>Discuss A &amp; B.</summary>
      </meeting>
    </meetings_data>
    """

    notes = _parse_meetings(text)

    assert len(notes) == 1
    assert notes[0]["id"] == "meeting-1"
    assert notes[0]["title"] == "Matt <> Daniel"
    assert notes[0]["owner"] == {"name": "Matt", "email": "matt@example.com"}
    assert [attendee["email"] for attendee in notes[0]["attendees"]] == [
        "matt@example.com",
        "daniel@example.com",
    ]
    assert notes[0]["summary_markdown"] == "Discuss A & B."


def test_parse_participants_uses_structural_delimiters():
    participants = _parse_participants(
        "Matt (note creator) from Acme <MATT@example.com>, Daniel <daniel@example.com>"
    )

    assert participants == [
        {"name": "Matt (note creator) from Acme", "email": "matt@example.com"},
        {"name": "Daniel", "email": "daniel@example.com"},
    ]


def test_parse_meeting_date_uses_gmt_suffix():
    assert _parse_meeting_date("Jul 8, 2026 5:30 PM GMT+2") == "2026-07-08T17:30:00+02:00"
    assert _parse_meeting_date("Jul 8, 2026 5:30 PM GMT") == "2026-07-08T17:30:00+00:00"
    assert _parse_meeting_date("Jul 8, 2026 5:30 PM CST") == "Jul 8, 2026 5:30 PM CST"


def test_granola_api_key_is_replace_mode_on_authorization():
    """GRANOLA_API_KEY must be replace-mode matched on Authorization (issue #1243).

    The REST client sets `Authorization: Bearer <placeholder>` itself, so the
    proxy has to swap that placeholder in the header the client already sends.
    Inject mode only *adds* a header the client never sends; against a header the
    client already sets it leaves the placeholder in place and the upstream 401s.
    """
    manifest = tomllib.loads(Path(__file__).with_name("pyproject.toml").read_text())
    secrets = manifest["tool"]["centaur"]["secrets"]
    granola = next(s for s in secrets if s["name"] == "GRANOLA_API_KEY")

    assert granola == {
        "type": "http",
        "name": "GRANOLA_API_KEY",
        "match_headers": ["Authorization"],
        "hosts": ["public-api.granola.ai"],
    }
    assert granola.get("mode") != "inject"


def test_rest_client_emits_placeholder_in_matched_authorization_header():
    """The client must place the secret placeholder in the header the proxy matches.

    With the stub backend, `secret("GRANOLA_API_KEY")` resolves to the placeholder
    string `GRANOLA_API_KEY`; the client must send it as the Bearer token so that
    replace-mode injection has a placeholder to swap.
    """
    configure(StubBackend())
    client = GranolaClient()
    try:
        assert client._client.headers["Authorization"] == "Bearer GRANOLA_API_KEY"
    finally:
        client.close()
