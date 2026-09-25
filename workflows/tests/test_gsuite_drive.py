from __future__ import annotations

import pytest

from workflows.gsuite import drive


class _Request:
    def execute(self) -> dict:
        return {"files": [], "incompleteSearch": True}


class _FilesApi:
    def __init__(self) -> None:
        self.list_args: dict = {}

    def list(self, **kwargs) -> _Request:
        self.list_args = kwargs
        return _Request()


class _DriveService:
    def __init__(self) -> None:
        self.files_api = _FilesApi()

    def files(self) -> _FilesApi:
        return self.files_api


def test_list_docs_rejects_incomplete_all_drives_search(monkeypatch):
    service = _DriveService()
    monkeypatch.setattr(drive, "get_drive_service", lambda: service)

    with pytest.raises(RuntimeError, match="could not search all drives"):
        drive.GoogleDriveReadonlyClient().list_docs(
            query="trashed = false",
            page_size=100,
        )

    assert "incompleteSearch" in service.files_api.list_args["fields"]
