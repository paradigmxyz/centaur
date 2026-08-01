from typer.testing import CliRunner

from gsuite import client
from gsuite.cli import app

runner = CliRunner()


def test_drive_list_full_text_flag_is_passed_to_client(monkeypatch):
    calls: list[dict] = []

    def fake_drive_list(**kwargs):
        calls.append(kwargs)
        return [
            {
                "id": "file-123",
                "name": "Contract Notes",
                "mime_type": "application/vnd.google-apps.document",
                "size": 0,
                "modified_time": "2026-07-21T10:00:00Z",
                "web_view_link": "https://drive.google.com/file/file-123",
                "parent_ids": [],
            }
        ]

    monkeypatch.setattr(client, "drive_list", fake_drive_list)

    result = runner.invoke(
        app,
        ["drive", "list", "--query", "contract language", "--full-text", "--limit", "5"],
    )

    assert result.exit_code == 0
    assert calls == [
        {
            "query": "contract language",
            "folder_id": None,
            "max_results": 5,
            "file_type": None,
            "full_text": True,
        }
    ]


def test_docs_bullets_command_prints_verification_summary(monkeypatch):
    monkeypatch.setattr(
        client,
        "docs_bullets",
        lambda document_id, match_prefix, bullet_preset, tab_id, dry_run: {
            "document_id": document_id,
            "match_prefix": match_prefix,
            "bullet_preset": bullet_preset,
            "matched_paragraphs": 2,
            "updated_paragraphs": 2,
            "verified_paragraphs": 2,
            "already_bulleted_paragraphs": 1,
            "dry_run": dry_run,
            "paragraphs": [
                {
                    "tab_id": None,
                    "paragraph_index": 1,
                    "before": "- First item",
                    "after": "First item",
                },
                {
                    "tab_id": "tab-2",
                    "paragraph_index": 3,
                    "before": "- Second item",
                    "after": "Second item",
                },
            ],
        },
    )

    result = runner.invoke(app, ["docs", "bullets", "doc-123"])

    assert result.exit_code == 0
    assert "Converted 2 paragraph(s) into Google Docs bullets" in result.output
    assert "Verification: matched 2, updated 2, verified 2, already bulleted 1" in result.output
    assert "paragraph 2:" in result.output
    assert "tab tab-2 paragraph 4:" in result.output
