import json
import sys
from pathlib import Path
from typing import ClassVar

import pytest
from typer.testing import CliRunner

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from granola import cli
from granola import client as granola_client


class FakeClient:
    notes: ClassVar[list[dict]] = [
        {
            "id": "meeting-1",
            "title": "Roadmap",
            "created_at": "2026-09-10T16:00:00Z",
            "owner": {"name": "Alex"},
            "attendees": [{"name": "Sam"}],
            "summary_markdown": "Launch plans",
        }
    ]
    transcript: ClassVar[list[dict]] = [{"speaker": {"source": "Alex"}, "text": "Ship it."}]

    def list_all_notes(self, **kwargs):
        return self.notes

    def get_note(self, note_id, include_transcript=False):
        note = dict(self.notes[0])
        if include_transcript:
            note["transcript"] = self.transcript
        return note

    def get_transcript(self, note_id):
        return self.transcript


@pytest.mark.parametrize(
    ("args", "payload"),
    [
        (["list"], FakeClient.notes),
        (["get", "meeting-1"], FakeClient.notes[0]),
        (["transcript", "meeting-1"], FakeClient.transcript),
        (["search", "road"], FakeClient.notes),
    ],
)
def test_note_commands_default_to_json(monkeypatch, args, payload):
    monkeypatch.setattr(granola_client, "_client", FakeClient)

    result = CliRunner().invoke(cli.app, args)

    assert result.exit_code == 0, result.output
    assert json.loads(result.output) == payload


def test_list_table_flag_uses_human_readable_output(monkeypatch):
    monkeypatch.setattr(granola_client, "_client", FakeClient)

    result = CliRunner().invoke(cli.app, ["list", "--table"])

    assert result.exit_code == 0, result.output
    assert "Granola Notes (1)" in result.output
    assert "Roadmap" in result.output


def test_get_raw_still_uses_markdown_output(monkeypatch):
    monkeypatch.setattr(granola_client, "_client", FakeClient)

    result = CliRunner().invoke(cli.app, ["get", "meeting-1", "--raw"])

    assert result.exit_code == 0, result.output
    assert result.output.startswith("# Roadmap\n")
    assert "Launch plans" in result.output
