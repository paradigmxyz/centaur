from client import _parse_meetings


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
