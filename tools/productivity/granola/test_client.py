from client import (
    GranolaClient,
    _normalize_note_ref,
    _parse_meeting_date,
    _parse_meetings,
    _parse_participants,
)


def test_normalize_note_ref_accepts_both_granola_share_link_forms():
    meeting_id = "57db2e7a-2aee-40b4-9db2-a20ec81c41c5"

    assert _normalize_note_ref(f"https://notes.granola.ai/d/{meeting_id}") == meeting_id
    assert (
        _normalize_note_ref(f"<https://notes.granola.ai/t/{meeting_id}-008umkv4|notes.granola.ai>")
        == meeting_id
    )


def test_rest_client_resolves_share_link_before_getting_note():
    meeting_id = "57db2e7a-2aee-40b4-9db2-a20ec81c41c5"
    client = GranolaClient(api_key="test")
    calls = []

    def fake_get(path, params=None):
        calls.append((path, params))
        if path == "/v1/notes":
            return {
                "notes": [
                    {
                        "id": "not_1234567890abcd",
                        "web_url": f"https://notes.granola.ai/d/{meeting_id}",
                    }
                ],
                "hasMore": False,
                "cursor": None,
            }
        return {"id": "not_1234567890abcd", "title": "Stripe risk"}

    client._get = fake_get
    try:
        note = client.get_note(f"https://notes.granola.ai/t/{meeting_id}-008umkv4")
    finally:
        client.close()

    assert note["title"] == "Stripe risk"
    assert calls == [
        ("/v1/notes", {"page_size": 30}),
        ("/v1/notes/not_1234567890abcd", {}),
    ]


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
