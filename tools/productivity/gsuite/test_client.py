import base64

import pytest

from gsuite import client


class _CreateRequest:
    def __init__(self, result: dict):
        self._result = result

    def execute(self) -> dict:
        return self._result


class _FakeFilesApi:
    def __init__(self):
        self.create_calls: list[dict] = []

    def create(self, **kwargs):
        self.create_calls.append(kwargs)
        if kwargs["body"].get("mimeType") == "application/vnd.google-apps.folder":
            return _CreateRequest(
                {
                    "id": "folder-123",
                    "name": kwargs["body"]["name"],
                    "webViewLink": "https://drive.google.com/folder/folder-123",
                    "parents": kwargs["body"].get("parents", []),
                }
            )

        return _CreateRequest(
            {
                "id": "file-123",
                "name": kwargs["body"]["name"],
                "webViewLink": "https://drive.google.com/file/file-123",
            }
        )


class _FakeDriveService:
    def __init__(self):
        self.files_api = _FakeFilesApi()

    def files(self):
        return self.files_api


class _FakeSheetsValuesApi:
    def __init__(self):
        self.update_calls: list[dict] = []

    def update(self, **kwargs):
        self.update_calls.append(kwargs)
        values = kwargs["body"]["values"]
        updated_columns = max((len(row) for row in values), default=0)
        updated_cells = sum(len(row) for row in values)
        return _CreateRequest(
            {
                "updatedRange": kwargs["range"],
                "updatedRows": len(values),
                "updatedColumns": updated_columns,
                "updatedCells": updated_cells,
            }
        )


class _FakeSpreadsheetsApi:
    def __init__(self):
        self.values_api = _FakeSheetsValuesApi()
        self.batch_update_calls: list[dict] = []

    def values(self):
        return self.values_api

    def batchUpdate(self, **kwargs):
        self.batch_update_calls.append(kwargs)
        properties = kwargs["body"]["requests"][0]["addSheet"]["properties"]
        return _CreateRequest(
            {
                "replies": [
                    {
                        "addSheet": {
                            "properties": {
                                "sheetId": 789,
                                "title": properties["title"],
                                "index": properties.get("index", 0),
                                "sheetType": "GRID",
                                "gridProperties": {"rowCount": 1000, "columnCount": 26},
                            }
                        }
                    }
                ]
            }
        )


class _FakeSheetsService:
    def __init__(self):
        self.spreadsheets_api = _FakeSpreadsheetsApi()

    def spreadsheets(self):
        return self.spreadsheets_api


class _FakeDocsDocumentsApi:
    def __init__(self, get_results: list[dict]):
        self.get_results = list(get_results)
        self.get_calls: list[dict] = []
        self.batch_update_calls: list[dict] = []

    def get(self, **kwargs):
        self.get_calls.append(kwargs)
        if not self.get_results:
            raise AssertionError("Unexpected extra documents.get call")
        return _CreateRequest(self.get_results.pop(0))

    def batchUpdate(self, **kwargs):
        self.batch_update_calls.append(kwargs)
        request_count = len(kwargs["body"]["requests"])
        return _CreateRequest(
            {
                "documentId": kwargs["documentId"],
                "replies": [{} for _ in range(request_count)],
            }
        )


class _FakeDocsService:
    def __init__(self, get_results: list[dict]):
        self.documents_api = _FakeDocsDocumentsApi(get_results)

    def documents(self):
        return self.documents_api


def _paragraph(start_index: int, text: str, *, bullet: bool = False) -> dict:
    paragraph = {"elements": [{"textRun": {"content": text}}]}
    if bullet:
        paragraph["bullet"] = {"listId": "list-123"}
    return {
        "startIndex": start_index,
        "endIndex": start_index + len(text),
        "paragraph": paragraph,
    }


def _tab(tab_id: str, content: list[dict], *, child_tabs: list[dict] | None = None) -> dict:
    return {
        "tabProperties": {"tabId": tab_id},
        "documentTab": {"body": {"content": content}},
        "childTabs": child_tabs or [],
    }


def test_drive_upload_sets_supports_all_drives(tmp_path, monkeypatch):
    upload_file = tmp_path / "example.txt"
    upload_file.write_text("hello")
    fake_service = _FakeDriveService()

    monkeypatch.setattr(client, "get_drive_service", lambda: fake_service)
    monkeypatch.setattr(
        client,
        "MediaIoBaseUpload",
        lambda fd, mimetype, resumable: {
            "content": fd.getvalue(),
            "mimetype": mimetype,
            "resumable": resumable,
        },
    )

    result = client.drive_upload(
        content_base64=base64.b64encode(upload_file.read_bytes()).decode("ascii"),
        filename="example.txt",
        folder_id="parent-123",
    )

    create_call = fake_service.files_api.create_calls[0]
    assert create_call["supportsAllDrives"] is True
    assert create_call["body"]["parents"] == ["parent-123"]
    assert create_call["media_body"]["content"] == b"hello"
    assert result["id"] == "file-123"
    assert result["name"] == "example.txt"


def test_drive_upload_rejects_local_path_argument():
    with pytest.raises(TypeError, match="unexpected keyword argument 'file_path'"):
        client.drive_upload(file_path="/tmp/secret.txt")


def test_drive_upload_requires_a_content_source():
    with pytest.raises(ValueError, match="content_base64, attachment_id, or attachment_url"):
        client.drive_upload()


def test_drive_upload_accepts_attachment_id(monkeypatch):
    fake_service = _FakeDriveService()
    monkeypatch.setattr(client, "get_drive_service", lambda: fake_service)
    monkeypatch.setattr(client, "_download_attachment_bytes", lambda **_: b"from-attachment")
    monkeypatch.setattr(
        client,
        "MediaIoBaseUpload",
        lambda fd, mimetype, resumable: {
            "content": fd.getvalue(),
            "mimetype": mimetype,
            "resumable": resumable,
        },
    )

    result = client.drive_upload(attachment_id="att-123", filename="report.csv")

    create_call = fake_service.files_api.create_calls[0]
    assert create_call["body"]["name"] == "report.csv"
    assert create_call["media_body"]["content"] == b"from-attachment"
    assert create_call["media_body"]["mimetype"] == "text/csv"
    assert result["id"] == "file-123"


def test_drive_create_folder_uses_folder_mime_type(monkeypatch):
    fake_service = _FakeDriveService()
    monkeypatch.setattr(client, "get_drive_service", lambda: fake_service)

    result = client.drive_create_folder("Closing Docs", parent_id="parent-123")

    create_call = fake_service.files_api.create_calls[0]
    assert create_call["supportsAllDrives"] is True
    assert create_call["body"] == {
        "name": "Closing Docs",
        "mimeType": "application/vnd.google-apps.folder",
        "parents": ["parent-123"],
    }
    assert result == {
        "id": "folder-123",
        "name": "Closing Docs",
        "web_view_link": "https://drive.google.com/folder/folder-123",
        "parent_ids": ["parent-123"],
    }


def test_sheets_add_tab_uses_batch_update(monkeypatch):
    fake_service = _FakeSheetsService()
    monkeypatch.setattr(client, "get_sheets_service", lambda: fake_service)

    result = client.sheets_add_tab(
        "spreadsheet-123", "Missing From Original List", index=2
    )

    batch_update_call = fake_service.spreadsheets_api.batch_update_calls[0]
    assert batch_update_call == {
        "spreadsheetId": "spreadsheet-123",
        "body": {
            "requests": [
                {
                    "addSheet": {
                        "properties": {"title": "Missing From Original List", "index": 2}
                    }
                }
            ]
        },
    }
    assert result == {
        "spreadsheet_id": "spreadsheet-123",
        "sheet_id": 789,
        "title": "Missing From Original List",
        "index": 2,
        "sheet_type": "GRID",
        "grid_properties": {"rowCount": 1000, "columnCount": 26},
        "sheet_properties": {
            "sheetId": 789,
            "title": "Missing From Original List",
            "index": 2,
            "sheetType": "GRID",
            "gridProperties": {"rowCount": 1000, "columnCount": 26},
        },
        "url": "https://docs.google.com/spreadsheets/d/spreadsheet-123/edit#gid=789",
    }


def test_sheets_write_table_writes_headers_and_rows_to_named_tab(monkeypatch):
    fake_service = _FakeSheetsService()
    monkeypatch.setattr(client, "get_sheets_service", lambda: fake_service)

    result = client.sheets_write_table(
        "spreadsheet-123",
        "Missing From Original's List",
        ["Asset", "Status"],
        [
            {"Asset": "ETH", "Status": "missing"},
            {"Asset": "SOL", "Status": None},
            {"Asset": "ARB"},
        ],
        start_cell="B2",
    )

    update_call = fake_service.spreadsheets_api.values_api.update_calls[0]
    assert update_call == {
        "spreadsheetId": "spreadsheet-123",
        "range": "'Missing From Original''s List'!B2",
        "valueInputOption": "USER_ENTERED",
        "body": {
            "values": [
                ["Asset", "Status"],
                ["ETH", "missing"],
                ["SOL", ""],
                ["ARB", ""],
            ]
        },
    }
    assert result == {
        "spreadsheet_id": "spreadsheet-123",
        "updated_range": "'Missing From Original''s List'!B2",
        "updated_rows": 4,
        "updated_columns": 2,
        "updated_cells": 8,
        "sheet_title": "Missing From Original's List",
        "headers": ["Asset", "Status"],
        "row_count": 3,
        "header_count": 2,
    }


def test_docs_bullets_builds_google_docs_list_requests(monkeypatch):
    fake_service = _FakeDocsService(
        [
            {
                "revisionId": "rev-1",
                "body": {
                    "content": [
                        _paragraph(1, "Intro\n"),
                        _paragraph(7, "- First item\n"),
                        _paragraph(20, "\t- Nested item\n"),
                    ]
                }
            },
            {
                "body": {
                    "content": [
                        _paragraph(1, "Intro\n"),
                        _paragraph(7, "First item\n", bullet=True),
                        _paragraph(17, "Nested item\n", bullet=True),
                    ]
                }
            },
        ]
    )
    monkeypatch.setattr(client, "get_docs_service", lambda: fake_service)

    result = client.docs_bullets("doc-123")

    assert fake_service.documents_api.get_calls == [
        {"documentId": "doc-123", "includeTabsContent": True},
        {"documentId": "doc-123", "includeTabsContent": True},
    ]
    assert fake_service.documents_api.batch_update_calls == [
        {
            "documentId": "doc-123",
            "body": {
                "requests": [
                    {
                        "deleteContentRange": {
                            "range": {"startIndex": 21, "endIndex": 23}
                        }
                    },
                    {
                        "createParagraphBullets": {
                            "range": {"startIndex": 20, "endIndex": 33},
                            "bulletPreset": "BULLET_DISC_CIRCLE_SQUARE",
                        }
                    },
                    {
                        "deleteContentRange": {
                            "range": {"startIndex": 7, "endIndex": 9}
                        }
                    },
                    {
                        "createParagraphBullets": {
                            "range": {"startIndex": 7, "endIndex": 18},
                            "bulletPreset": "BULLET_DISC_CIRCLE_SQUARE",
                        }
                    },
                ],
                "writeControl": {"requiredRevisionId": "rev-1"},
            },
        }
    ]
    assert result == {
        "document_id": "doc-123",
        "match_prefix": "- ",
        "bullet_preset": "BULLET_DISC_CIRCLE_SQUARE",
        "matched_paragraphs": 2,
        "updated_paragraphs": 2,
        "verified_paragraphs": 2,
        "already_bulleted_paragraphs": 0,
        "dry_run": False,
        "paragraphs": [
            {
                "tab_id": None,
                "paragraph_index": 1,
                "before": "- First item",
                "after": "First item",
                "nesting_level": 0,
            },
            {
                "tab_id": None,
                "paragraph_index": 2,
                "before": "\t- Nested item",
                "after": "Nested item",
                "nesting_level": 1,
            },
        ],
    }


def test_docs_bullets_rejects_empty_match_prefix_before_read(monkeypatch):
    monkeypatch.setattr(
        client,
        "docs_get",
        lambda document_id: (_ for _ in ()).throw(AssertionError("docs_get should not run")),
    )

    with pytest.raises(ValueError, match="match_prefix must not be empty"):
        client.docs_bullets("doc-123", match_prefix="")


def test_docs_bullets_rejects_unknown_bullet_preset_before_read(monkeypatch):
    monkeypatch.setattr(
        client,
        "docs_get",
        lambda document_id: (_ for _ in ()).throw(AssertionError("docs_get should not run")),
    )

    with pytest.raises(ValueError, match="Unsupported bullet_preset"):
        client.docs_bullets("doc-123", bullet_preset="BULLET_MYSTERY")


def test_docs_bullets_dry_run_does_not_write_or_verify(monkeypatch):
    fake_service = _FakeDocsService(
        [
            {
                "body": {
                    "content": [
                        _paragraph(1, "- First item\n"),
                    ]
                }
            }
        ]
    )
    monkeypatch.setattr(client, "get_docs_service", lambda: fake_service)

    result = client.docs_bullets("doc-123", dry_run=True)

    assert fake_service.documents_api.get_calls == [
        {"documentId": "doc-123", "includeTabsContent": True},
    ]
    assert fake_service.documents_api.batch_update_calls == []
    assert result["matched_paragraphs"] == 1
    assert result["updated_paragraphs"] == 0
    assert result["verified_paragraphs"] == 0
    assert result["dry_run"] is True


def test_docs_bullets_scopes_requests_to_tab(monkeypatch):
    fake_service = _FakeDocsService(
        [
            {
                "revisionId": "rev-tab",
                "tabs": [
                    _tab("tab-a", [_paragraph(1, "- Other tab\n")]),
                    _tab("tab-b", [_paragraph(1, "- Target\n")]),
                ]
            },
            {
                "tabs": [
                    _tab("tab-b", [_paragraph(1, "Target\n", bullet=True)]),
                ]
            },
        ]
    )
    monkeypatch.setattr(client, "get_docs_service", lambda: fake_service)

    result = client.docs_bullets("doc-123", tab_id="tab-b")

    assert fake_service.documents_api.batch_update_calls == [
        {
            "documentId": "doc-123",
            "body": {
                "requests": [
                    {
                        "deleteContentRange": {
                            "range": {"startIndex": 1, "endIndex": 3, "tabId": "tab-b"}
                        }
                    },
                    {
                        "createParagraphBullets": {
                            "range": {"startIndex": 1, "endIndex": 8, "tabId": "tab-b"},
                            "bulletPreset": "BULLET_DISC_CIRCLE_SQUARE",
                        }
                    },
                ],
                "writeControl": {"requiredRevisionId": "rev-tab"},
            },
        }
    ]
    assert result["matched_paragraphs"] == 1
    assert result["updated_paragraphs"] == 1
    assert result["verified_paragraphs"] == 1
    assert result["paragraphs"] == [
        {
            "tab_id": "tab-b",
            "paragraph_index": 0,
            "before": "- Target",
            "after": "Target",
            "nesting_level": 0,
        }
    ]


def test_docs_bullets_requires_revision_for_writes(monkeypatch):
    fake_service = _FakeDocsService(
        [
            {
                "body": {
                    "content": [
                        _paragraph(1, "- First item\n"),
                    ]
                }
            }
        ]
    )
    monkeypatch.setattr(client, "get_docs_service", lambda: fake_service)

    with pytest.raises(RuntimeError, match="revisionId"):
        client.docs_bullets("doc-123")

    assert fake_service.documents_api.batch_update_calls == []


def test_docs_append_passes_expected_revision_id_through(monkeypatch):
    fake_service = _FakeDocsService([])
    monkeypatch.setattr(client, "get_docs_service", lambda: fake_service)

    client.docs_append("doc-123", "hello")
    client.docs_append("doc-123", "hello", expected_revision_id="rev-42")

    calls = fake_service.documents_api.batch_update_calls
    assert len(calls) == 2
    assert "writeControl" not in calls[0]["body"]
    assert calls[1]["body"]["writeControl"] == {"requiredRevisionId": "rev-42"}


def test_docs_replace_passes_expected_revision_id_through(monkeypatch):
    fake_service = _FakeDocsService([])
    monkeypatch.setattr(client, "get_docs_service", lambda: fake_service)

    client.docs_replace("doc-123", "old", "new")
    client.docs_replace("doc-123", "old", "new", expected_revision_id="rev-7")

    calls = fake_service.documents_api.batch_update_calls
    assert len(calls) == 2
    assert "writeControl" not in calls[0]["body"]
    assert calls[1]["body"]["writeControl"] == {"requiredRevisionId": "rev-7"}


def test_docs_insert_passes_expected_revision_id_through(monkeypatch):
    fake_service = _FakeDocsService([])
    monkeypatch.setattr(client, "get_docs_service", lambda: fake_service)

    client.docs_insert("doc-123", "hello", 1)
    client.docs_insert("doc-123", "hello", 1, expected_revision_id="rev-99")

    calls = fake_service.documents_api.batch_update_calls
    assert len(calls) == 2
    assert "writeControl" not in calls[0]["body"]
    assert calls[1]["body"]["writeControl"] == {"requiredRevisionId": "rev-99"}
