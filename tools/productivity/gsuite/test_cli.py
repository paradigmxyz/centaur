import json

from typer.testing import CliRunner

from gsuite import client
from gsuite.cli import app, extract_drive_file_id

runner = CliRunner()


def test_extract_drive_file_id_accepts_editor_and_drive_urls():
    assert (
        extract_drive_file_id("https://docs.google.com/document/d/doc-123/edit")
        == "doc-123"
    )
    assert (
        extract_drive_file_id("https://docs.google.com/spreadsheets/d/sheet-123/edit")
        == "sheet-123"
    )
    assert (
        extract_drive_file_id("https://docs.google.com/presentation/d/slides-123/edit")
        == "slides-123"
    )
    assert extract_drive_file_id("https://drive.google.com/open?id=file-123") == "file-123"
    assert extract_drive_file_id("raw-file-id") == "raw-file-id"


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


def test_drive_revisions_command_accepts_sheets_url_and_outputs_json(monkeypatch):
    calls: list[dict] = []
    monkeypatch.setattr(
        client,
        "drive_list_revisions",
        lambda file_id, max_results: (
            calls.append({"file_id": file_id, "max_results": max_results})
            or [{"id": "rev-1", "modified_time": "2026-08-10T10:00:00Z"}]
        ),
    )

    result = runner.invoke(
        app,
        [
            "drive",
            "revisions",
            "https://docs.google.com/spreadsheets/d/sheet-123/edit",
            "--limit",
            "25",
            "--json",
        ],
    )

    assert result.exit_code == 0
    assert json.loads(result.output) == [{"id": "rev-1", "modified_time": "2026-08-10T10:00:00Z"}]
    assert calls == [{"file_id": "sheet-123", "max_results": 25}]


def test_drive_revision_command_accepts_slides_url_and_outputs_export_links(monkeypatch):
    calls: list[dict] = []
    monkeypatch.setattr(
        client,
        "drive_get_revision",
        lambda file_id, revision_id: (
            calls.append({"file_id": file_id, "revision_id": revision_id})
            or {
                "id": revision_id,
                "mime_type": "application/vnd.google-apps.document",
                "modified_time": "2026-08-10T10:00:00Z",
                "published": False,
                "published_link": "",
                "last_modifying_user": {"display_name": "Ada", "email": "ada@example.com"},
                "export_links": {"application/pdf": "https://docs.google.com/export/rev-42.pdf"},
            }
        ),
    )

    result = runner.invoke(
        app,
        [
            "drive",
            "revision",
            "https://docs.google.com/presentation/d/slides-123/edit",
            "rev-42",
        ],
    )

    assert result.exit_code == 0
    assert "Revision rev-42" in result.output
    assert "Ada" in result.output
    assert "application/pdf" in result.output
    assert "https://docs.google.com/export/rev-42.pdf" in result.output
    assert calls == [{"file_id": "slides-123", "revision_id": "rev-42"}]


def test_drive_export_revision_command_writes_selected_revision(tmp_path, monkeypatch):
    calls: list[dict] = []
    monkeypatch.setattr(
        client,
        "_drive_export_revision_bytes",
        lambda file_id, revision_id, export_format: (
            calls.append(
                {
                    "file_id": file_id,
                    "revision_id": revision_id,
                    "export_format": export_format,
                }
            )
            or (
                {"name": "Quarterly Model"},
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                b"historical workbook",
            )
        ),
    )

    result = runner.invoke(
        app,
        [
            "drive",
            "export-revision",
            "https://docs.google.com/spreadsheets/d/sheet-123/edit",
            "rev-42",
            "--format",
            "xlsx",
            "--output",
            str(tmp_path),
        ],
    )

    output_path = tmp_path / "Quarterly Model-revision-rev-42.xlsx"
    assert result.exit_code == 0
    assert output_path.read_bytes() == b"historical workbook"
    assert "Exported revision rev-42" in result.output
    assert calls == [
        {"file_id": "sheet-123", "revision_id": "rev-42", "export_format": "xlsx"}
    ]


def test_drive_export_revision_command_prints_text_to_stdout(monkeypatch):
    monkeypatch.setattr(
        client,
        "_drive_export_revision_bytes",
        lambda file_id, revision_id, export_format: (
            {"name": "Old Draft"},
            "text/plain",
            b"[Historical draft]",
        ),
    )

    result = runner.invoke(
        app,
        [
            "drive",
            "export-revision",
            "doc-123",
            "rev-7",
            "--format",
            "txt",
            "--stdout",
        ],
    )

    assert result.exit_code == 0
    assert "[Historical draft]" in result.output


def test_drive_download_revision_command_writes_original_binary(tmp_path, monkeypatch):
    monkeypatch.setattr(
        client,
        "_drive_download_revision_bytes",
        lambda file_id, revision_id: (
            {"name": "diagram.png"},
            {"original_filename": "diagram.png"},
            b"historical image",
        ),
    )

    result = runner.invoke(
        app,
        [
            "drive",
            "download-revision",
            "file-123",
            "rev-42",
            "--output",
            str(tmp_path),
        ],
    )

    output_path = tmp_path / "diagram-revision-rev-42.png"
    assert result.exit_code == 0
    assert output_path.read_bytes() == b"historical image"
    assert "Downloaded revision rev-42" in result.output
