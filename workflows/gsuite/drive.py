from __future__ import annotations

from typing import Any

from workflows.gsuite.docs import docs_get_text
from workflows.gsuite.http import build_http

GOOGLE_DOC_MIME_TYPE = "application/vnd.google-apps.document"


def get_drive_service():
    """Return a proxy-authenticated Google Drive v3 service."""
    from googleapiclient.discovery import build

    return build("drive", "v3", http=build_http())


class GoogleDriveReadonlyClient:
    """Read-only Drive/Docs client used by ETL workflows."""

    def list_docs(
        self,
        *,
        query: str,
        page_size: int,
        page_token: str | None = None,
    ) -> dict[str, Any]:
        service = get_drive_service()
        kwargs: dict[str, Any] = {
            "q": query,
            "pageSize": page_size,
            "fields": (
                "nextPageToken, files("
                "id, name, mimeType, webViewLink, driveId, parents, owners, "
                "lastModifyingUser, trashed, createdTime, modifiedTime"
                ")"
            ),
            "includeItemsFromAllDrives": True,
            "supportsAllDrives": True,
            "orderBy": "modifiedTime",
        }
        if page_token:
            kwargs["pageToken"] = page_token
        return service.files().list(**kwargs).execute()

    def docs_get_text(self, document_id: str) -> str:
        return str(docs_get_text(document_id) or "")
