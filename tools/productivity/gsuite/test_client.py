import base64
import tomllib
from pathlib import Path

import pytest

from gsuite import client


def test_revision_export_authenticates_google_docs_request():
    pyproject = tomllib.loads(
        Path(client.__file__).with_name("pyproject.toml").read_text()
    )
    centaur_config = pyproject["tool"]["centaur"]
    oauth_hosts = centaur_config["secrets"][0]["hosts"]

    assert "docs.google.com" in centaur_config["hosts"]
    assert "docs.google.com" in oauth_hosts


class _CreateRequest:
    def __init__(self, result: dict):
        self._result = result

    def execute(self) -> dict:
        return self._result


class _FakeFilesApi:
    def __init__(self):
        self.create_calls: list[dict] = []
        self.list_calls: list[dict] = []

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

    def list(self, **kwargs):
        self.list_calls.append(kwargs)
        return _CreateRequest(
            {
                "files": [
                    {
                        "id": "file-123",
                        "name": "Quinn's paper",
                        "mimeType": "application/pdf",
                        "size": "1024",
                        "modifiedTime": "2026-07-21T10:00:00Z",
                        "webViewLink": "https://drive.google.com/file/file-123",
                        "parents": ["folder-123"],
                    }
                ]
            }
        )

class _FakeRevisionsApi:
    def __init__(
        self,
        list_results: list[dict] | None = None,
        get_result: dict | None = None,
    ):
        self.list_results = list(list_results or [])
        self.get_result = get_result or {}
        self.list_calls: list[dict] = []
        self.get_calls: list[dict] = []

    def list(self, **kwargs):
        self.list_calls.append(kwargs)
        if not self.list_results:
            raise AssertionError("Unexpected extra revisions.list call")
        return _CreateRequest(self.list_results.pop(0))

    def get(self, **kwargs):
        self.get_calls.append(kwargs)
        return _CreateRequest(self.get_result)


class _FakeDriveService:
    def __init__(
        self,
        revision_list_results: list[dict] | None = None,
        revision_get_result: dict | None = None,
    ):
        self.files_api = _FakeFilesApi()
        self.revisions_api = _FakeRevisionsApi(
            revision_list_results,
            revision_get_result,
        )

    def files(self):
        return self.files_api

    def revisions(self):
        return self.revisions_api


class _FakeGmailMessagesApi:
    def __init__(self, full_result: dict, raw_result: dict):
        self.full_result = full_result
        self.raw_result = raw_result
        self.get_calls: list[dict] = []

    def get(self, **kwargs):
        self.get_calls.append(kwargs)
        if kwargs.get("format") == "full":
            return _CreateRequest(self.full_result)
        if kwargs.get("format") == "raw":
            return _CreateRequest(self.raw_result)
        raise AssertionError(f"Unexpected Gmail format: {kwargs.get('format')}")


class _FakeGmailUsersApi:
    def __init__(self, messages_api: _FakeGmailMessagesApi):
        self.messages_api = messages_api

    def messages(self):
        return self.messages_api


class _FakeGmailService:
    def __init__(self, full_result: dict, raw_result: dict):
        self.messages_api = _FakeGmailMessagesApi(full_result, raw_result)
        self.users_api = _FakeGmailUsersApi(self.messages_api)

    def users(self):
        return self.users_api


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


def test_gmail_read_returns_plain_html_and_raw_content(monkeypatch):
    plain = base64.urlsafe_b64encode(b"Plain body").decode().rstrip("=")
    html = base64.urlsafe_b64encode(b"<html><body><p>HTML body</p></body></html>").decode().rstrip("=")
    raw = (
        b"From: sender@example.com\n"
        b"To: recipient@example.com\n"
        b"Subject: Newsletter\n"
        b"MIME-Version: 1.0\n"
        b"Content-Type: text/html\n\n"
        b"<html><body><p>HTML body</p></body></html>"
    )
    raw_encoded = base64.urlsafe_b64encode(raw).decode().rstrip("=")
    full_result = {
        "id": "msg-1",
        "threadId": "thread-1",
        "labelIds": ["INBOX"],
        "payload": {
            "headers": [
                {"name": "Subject", "value": "Newsletter"},
                {"name": "From", "value": "sender@example.com"},
                {"name": "To", "value": "recipient@example.com"},
                {"name": "Date", "value": "Thu, 02 Jul 2026 18:15:51 +0000"},
            ],
            "parts": [
                {
                    "mimeType": "multipart/alternative",
                    "parts": [
                        {"mimeType": "text/plain", "body": {"data": plain}},
                        {"mimeType": "text/html", "body": {"data": html}},
                    ],
                }
            ],
        },
    }
    fake_service = _FakeGmailService(full_result, {"raw": raw_encoded})
    monkeypatch.setattr(client, "get_gmail_service", lambda: fake_service)

    result = client.gmail_read("msg-1")

    assert result["id"] == "msg-1"
    assert result["subject"] == "Newsletter"
    assert result["body"] == "Plain body"
    assert result["html"] == "<html><body><p>HTML body</p></body></html>"
    assert result["raw"] == raw_encoded
    assert fake_service.messages_api.get_calls == [
        {"userId": "me", "id": "msg-1", "format": "full"},
        {"userId": "me", "id": "msg-1", "format": "raw"},
    ]


def test_drive_list_searches_name_by_default(monkeypatch):
    fake_service = _FakeDriveService()
    monkeypatch.setattr(client, "get_drive_service", lambda: fake_service)

    result = client.drive_list(query="report")

    list_call = fake_service.files_api.list_calls[0]
    assert list_call["q"] == "name contains 'report' and trashed = false"
    assert list_call["includeItemsFromAllDrives"] is True
    assert list_call["supportsAllDrives"] is True
    assert result[0]["id"] == "file-123"
    assert result[0]["size"] == 1024


def test_drive_list_supports_full_text_contains_and_escapes_literals(monkeypatch):
    fake_service = _FakeDriveService()
    monkeypatch.setattr(client, "get_drive_service", lambda: fake_service)

    client.drive_list(
        query="quinn's paper\\essay",
        folder_id="folder'123",
        file_type="application/pdf",
        full_text=True,
    )

    list_call = fake_service.files_api.list_calls[0]
    assert list_call["q"] == (
        "fullText contains 'quinn\\'s paper\\\\essay' and "
        "'folder\\'123' in parents and "
        "mimeType = 'application/pdf' and "
        "trashed = false"
    )


def test_gsuite_client_drive_search_supports_full_text(monkeypatch):
    calls: list[dict] = []

    def fake_drive_list(**kwargs):
        calls.append(kwargs)
        return []

    monkeypatch.setattr(client, "drive_list", fake_drive_list)

    assert client.GSuiteClient().drive_search("contract language", full_text=True) == []
    assert calls == [
        {"query": "contract language", "max_results": 50, "full_text": True}
    ]


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


def test_drive_upload_accepts_content_path(tmp_path, monkeypatch):
    upload_file = tmp_path / "example.txt"
    upload_file.write_text("hello from disk")
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
        content_path=str(upload_file),
        folder_id="parent-123",
    )

    create_call = fake_service.files_api.create_calls[0]
    assert create_call["body"]["name"] == "example.txt"
    assert create_call["body"]["parents"] == ["parent-123"]
    assert create_call["media_body"]["content"] == b"hello from disk"
    assert result["id"] == "file-123"


def test_drive_upload_rejects_missing_content_path(tmp_path):
    with pytest.raises(ValueError, match="content_path does not exist or is not a file"):
        client.drive_upload(content_path=str(tmp_path / "missing.txt"))


def test_drive_upload_requires_a_content_source():
    with pytest.raises(
        ValueError,
        match="content_base64, content_path, attachment_id, or attachment_url",
    ):
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


def test_drive_list_revisions_paginates_and_normalizes_results(monkeypatch):
    fake_service = _FakeDriveService(
        revision_list_results=[
            {
                "revisions": [
                    {
                        "id": "rev-1",
                        "mimeType": "application/vnd.google-apps.document",
                        "modifiedTime": "2026-08-10T10:00:00Z",
                        "published": True,
                        "publishedOutsideDomain": True,
                        "publishedLink": "https://docs.google.com/document/published/rev-1",
                        "lastModifyingUser": {
                            "displayName": "Ada Lovelace",
                            "emailAddress": "ada@example.com",
                            "me": True,
                        },
                        "exportLinks": {
                            "application/pdf": "https://docs.google.com/export/rev-1.pdf"
                        },
                    },
                    {
                        "id": "rev-2",
                        "modifiedTime": "2026-08-10T11:00:00Z",
                    },
                ],
                "nextPageToken": "page-2",
            },
            {
                "revisions": [
                    {
                        "id": "rev-3",
                        "modifiedTime": "2026-08-10T12:00:00Z",
                    }
                ]
            },
        ]
    )
    monkeypatch.setattr(client, "get_drive_service", lambda: fake_service)

    result = client.drive_list_revisions("file-123", max_results=3)

    fields = f"nextPageToken,revisions({client.DRIVE_REVISION_FIELDS})"
    assert fake_service.revisions_api.list_calls == [
        {"fileId": "file-123", "pageSize": 3, "fields": fields},
        {
            "fileId": "file-123",
            "pageSize": 1,
            "fields": fields,
            "pageToken": "page-2",
        },
    ]
    assert [revision["id"] for revision in result] == ["rev-1", "rev-2", "rev-3"]
    assert result[0] == {
        "id": "rev-1",
        "mime_type": "application/vnd.google-apps.document",
        "modified_time": "2026-08-10T10:00:00Z",
        "keep_forever": False,
        "published": True,
        "publish_auto": False,
        "published_outside_domain": True,
        "published_link": "https://docs.google.com/document/published/rev-1",
        "original_filename": "",
        "md5_checksum": "",
        "size": 0,
        "last_modifying_user": {
            "display_name": "Ada Lovelace",
            "email": "ada@example.com",
            "photo_link": "",
            "is_me": True,
            "permission_id": "",
        },
        "export_links": {"application/pdf": "https://docs.google.com/export/rev-1.pdf"},
    }


def test_drive_list_revisions_rejects_non_positive_limit(monkeypatch):
    monkeypatch.setattr(
        client,
        "get_drive_service",
        lambda: (_ for _ in ()).throw(AssertionError("Drive API should not be called")),
    )

    with pytest.raises(ValueError, match="max_results must be at least 1"):
        client.drive_list_revisions("file-123", max_results=0)


def test_drive_get_revision_returns_metadata_and_export_links(monkeypatch):
    fake_service = _FakeDriveService(
        revision_get_result={
            "id": "rev-42",
            "mimeType": "application/vnd.google-apps.document",
            "modifiedTime": "2026-08-11T09:30:00Z",
            "lastModifyingUser": {"displayName": "Grace Hopper"},
            "exportLinks": {"text/plain": "https://docs.google.com/export/rev-42.txt"},
        }
    )
    monkeypatch.setattr(client, "get_drive_service", lambda: fake_service)

    result = client.drive_get_revision("file-123", "rev-42")

    assert fake_service.revisions_api.get_calls == [
        {
            "fileId": "file-123",
            "revisionId": "rev-42",
            "fields": client.DRIVE_REVISION_FIELDS,
        }
    ]
    assert result["id"] == "rev-42"
    assert result["last_modifying_user"]["display_name"] == "Grace Hopper"
    assert result["export_links"] == {"text/plain": "https://docs.google.com/export/rev-42.txt"}


def test_drive_export_revision_bytes_downloads_requested_format(monkeypatch):
    request_calls: list[dict] = []

    class FakeResponse:
        status = 200

    class FakeHttp:
        def request(self, url, method):
            request_calls.append({"url": url, "method": method})
            return FakeResponse(), b"historical pdf"

    monkeypatch.setattr(
        client,
        "drive_get",
        lambda file_id: {
            "id": file_id,
            "name": "Quarterly Plan",
            "web_view_link": "https://docs.google.com/document/d/file-123/edit",
        },
    )
    monkeypatch.setattr(
        client,
        "drive_get_revision",
        lambda file_id, revision_id: {
            "id": revision_id,
            "export_links": {
                "application/pdf": (
                    "https://docs.google.com/feeds/download/documents/export/"
                    "Export?id=file-123&revision=rev-42&exportFormat=pdf"
                ),
                "text/plain": "https://docs.google.com/export/rev-42.txt",
            },
        },
    )
    monkeypatch.setattr(client, "_build_http", lambda: FakeHttp())

    metadata, mime_type, data = client._drive_export_revision_bytes(
        "file-123",
        "rev-42",
        "pdf",
    )

    assert metadata["name"] == "Quarterly Plan"
    assert mime_type == "application/pdf"
    assert data == b"historical pdf"
    assert request_calls == [
        {
            "url": (
                "https://docs.google.com/feeds/download/documents/export/"
                "Export?id=file-123&revision=rev-42&exportFormat=pdf"
            ),
            "method": "GET",
        }
    ]


def test_drive_export_revision_bytes_reports_available_formats(monkeypatch):
    monkeypatch.setattr(client, "drive_get", lambda file_id: {"id": file_id})
    monkeypatch.setattr(
        client,
        "drive_get_revision",
        lambda file_id, revision_id: {
            "id": revision_id,
            "export_links": {"text/plain": "https://docs.google.com/export/rev-42.txt"},
        },
    )

    with pytest.raises(ValueError, match="Available formats: txt"):
        client._drive_export_revision_bytes("file-123", "rev-42", "pdf")


def test_drive_download_revision_bytes_uses_alt_media(monkeypatch):
    request_calls: list[dict] = []

    class FakeResponse:
        status = 200

    class FakeHttp:
        def request(self, url, method):
            request_calls.append({"url": url, "method": method})
            return FakeResponse(), b"historical binary"

    monkeypatch.setattr(
        client,
        "drive_get",
        lambda file_id: {
            "id": file_id,
            "name": "diagram.png",
            "mime_type": "image/png",
        },
    )
    monkeypatch.setattr(
        client,
        "drive_get_revision",
        lambda file_id, revision_id: {
            "id": revision_id,
            "mime_type": "image/png",
            "original_filename": "diagram.png",
        },
    )
    monkeypatch.setattr(client, "_build_http", lambda: FakeHttp())

    metadata, revision, data = client._drive_download_revision_bytes(
        "file/id", "rev 42"
    )

    assert metadata["name"] == "diagram.png"
    assert revision["id"] == "rev 42"
    assert data == b"historical binary"
    assert request_calls == [
        {
            "url": (
                "https://www.googleapis.com/drive/v3/files/file%2Fid/"
                "revisions/rev%2042?alt=media"
            ),
            "method": "GET",
        }
    ]


def test_drive_download_revision_bytes_rejects_google_workspace_files(monkeypatch):
    monkeypatch.setattr(
        client,
        "drive_get",
        lambda file_id: {
            "id": file_id,
            "mime_type": "application/vnd.google-apps.presentation",
        },
    )
    monkeypatch.setattr(
        client,
        "drive_get_revision",
        lambda file_id, revision_id: {
            "id": revision_id,
            "mime_type": "application/vnd.google-apps.presentation",
        },
    )

    with pytest.raises(ValueError, match="export-revision"):
        client._drive_download_revision_bytes("slides-123", "rev-42")


def test_drive_export_revision_saves_attachment(monkeypatch):
    attachment_calls: list[dict] = []
    monkeypatch.setattr(
        client,
        "_drive_export_revision_bytes",
        lambda file_id, revision_id, export_format: (
            {
                "name": "Quarterly Plan",
                "web_view_link": "https://docs.google.com/document/d/file-123/edit",
            },
            "application/pdf",
            b"historical pdf",
        ),
    )
    monkeypatch.setattr(
        client,
        "save_attachment",
        lambda **kwargs: attachment_calls.append(kwargs) or {"id": "attachment-123"},
    )

    result = client.drive_export_revision("file-123", "rev-42", "pdf")

    assert result == {"id": "attachment-123"}
    assert attachment_calls == [
        {
            "name": "Quarterly Plan-revision-rev-42.pdf",
            "mime_type": "application/pdf",
            "data": b"historical pdf",
            "source_url": "https://docs.google.com/document/d/file-123/edit",
        }
    ]


def test_gsuite_client_exposes_drive_revisions(monkeypatch):
    list_calls: list[dict] = []
    get_calls: list[dict] = []
    export_calls: list[dict] = []
    download_calls: list[dict] = []
    monkeypatch.setattr(
        client,
        "drive_list_revisions",
        lambda file_id, max_results: (
            list_calls.append({"file_id": file_id, "max_results": max_results})
            or [{"id": "rev-1"}]
        ),
    )
    monkeypatch.setattr(
        client,
        "drive_get_revision",
        lambda file_id, revision_id: (
            get_calls.append({"file_id": file_id, "revision_id": revision_id})
            or {"id": revision_id}
        ),
    )
    monkeypatch.setattr(
        client,
        "drive_export_revision",
        lambda file_id, revision_id, export_format: (
            export_calls.append(
                {
                    "file_id": file_id,
                    "revision_id": revision_id,
                    "export_format": export_format,
                }
            )
            or {"id": "attachment-123"}
        ),
    )
    monkeypatch.setattr(
        client,
        "drive_download_revision",
        lambda file_id, revision_id: (
            download_calls.append({"file_id": file_id, "revision_id": revision_id})
            or {"id": "attachment-456"}
        ),
    )

    gsuite = client.GSuiteClient()
    assert gsuite.drive_list_revisions("file-123", max_results=25) == [{"id": "rev-1"}]
    assert gsuite.drive_get_revision("file-123", "rev-1") == {"id": "rev-1"}
    assert gsuite.drive_export_revision("file-123", "rev-1", "pdf") == {
        "id": "attachment-123"
    }
    assert gsuite.drive_download_revision("file-123", "rev-1") == {
        "id": "attachment-456"
    }
    assert list_calls == [{"file_id": "file-123", "max_results": 25}]
    assert get_calls == [{"file_id": "file-123", "revision_id": "rev-1"}]
    assert export_calls == [
        {"file_id": "file-123", "revision_id": "rev-1", "export_format": "pdf"}
    ]
    assert download_calls == [{"file_id": "file-123", "revision_id": "rev-1"}]


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
