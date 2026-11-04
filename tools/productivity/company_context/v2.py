"""Preview retrieval and custom documents through the company-context service's API.

The legacy index is served by ``client.py``. In sandboxes, iron-proxy injects
the principal's API JWT into requests to the service, which limits search results
to documents that principal can see, and custom document changes to the documents
that principal created.
"""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
from typing import Any
from urllib.parse import quote, urlencode

API_URL_ENV = "COMPANY_CONTEXT_API_URL"
TIMEOUT_SECONDS = 30


class CompanyContextV2Client:
    """Call the company-context service's query and custom document APIs."""

    def __init__(self, base_url: str | None = None) -> None:
        self._base_url = base_url

    def _request(self, method: str, path: str, body: dict[str, Any] | None = None) -> dict:
        base_url = self._base_url or os.getenv(API_URL_ENV, "")  # noqa: TID251 - non-secret
        base_url = base_url.strip().rstrip("/")
        if not base_url:
            return {
                "status": "error",
                "error": f"{API_URL_ENV} is not configured; the company-context API is not deployed",
            }
        headers = {"Accept": "application/json"}
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body).encode()
        request = urllib.request.Request(
            f"{base_url}{path}", data=data, headers=headers, method=method
        )
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
                payload = response.read()
                return {"status": "ok", **(json.loads(payload) if payload else {})}
        except urllib.error.HTTPError as exc:
            # The API reports failures as {"error": "..."}.
            try:
                message = json.loads(exc.read())["error"]
            except (ValueError, KeyError, TypeError):
                message = exc.reason
            return {"status": "error", "error": f"HTTP {exc.code}: {message}"}
        except (urllib.error.URLError, TimeoutError, ValueError) as exc:
            return {"status": "error", "error": str(exc)}

    def search(
        self,
        query: str,
        limit: int | None = None,
        types: list[str] | None = None,
        occurred_after: str | None = None,
        occurred_before: str | None = None,
        channel_ids: list[str] | None = None,
        file_ids: list[str] | None = None,
        sources: list[str] | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> dict:
        """Search documents visible to the requester; the API validates every field."""
        filters = {
            "types": types,
            "occurred_after": occurred_after,
            "occurred_before": occurred_before,
            "channel_ids": channel_ids,
            "file_ids": file_ids,
            "sources": sources,
            "metadata": metadata,
        }
        body: dict[str, Any] = {
            "query": query,
            "filters": {name: value for name, value in filters.items() if value},
        }
        if limit is not None:
            body["limit"] = limit
        return self._request("POST", "/query", body)

    def read_document(self, document_id: str) -> dict:
        """Read one visible document by a ``document_id`` from ``search``."""
        if not document_id.strip():
            return {"status": "error", "error": "document_id cannot be empty"}
        return self._request("GET", f"/documents/{quote(document_id.strip(), safe='')}")

    def create_document(
        self,
        source: str,
        body: str,
        title: str | None = None,
        occurred_at: str | None = None,
        readers: list[str] | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> dict:
        """Create a custom document, readable only by the requester unless ``readers`` is set."""
        return self._request(
            "POST",
            _documents_path(source),
            _document(body, title, occurred_at, readers, metadata),
        )

    def update_document(
        self,
        source: str,
        external_id: str,
        body: str,
        title: str | None = None,
        occurred_at: str | None = None,
        readers: list[str] | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> dict:
        """Replace all of a custom document the requester created."""
        return self._request(
            "PUT",
            _documents_path(source, external_id),
            _document(body, title, occurred_at, readers, metadata),
        )

    def delete_document(self, source: str, external_id: str) -> dict:
        """Delete a custom document the requester created."""
        return self._request("DELETE", _documents_path(source, external_id))

    def list_documents(
        self, source: str, limit: int | None = None, before: str | None = None
    ) -> dict:
        """List the requester's custom documents in ``source``, newest first."""
        params = {
            name: value for name, value in {"limit": limit, "before": before}.items() if value
        }
        query = f"?{urlencode(params)}" if params else ""
        return self._request("GET", f"{_documents_path(source)}{query}")


def _documents_path(source: str, external_id: str | None = None) -> str:
    path = f"/custom-documents/{quote(source.strip(), safe='')}"
    if external_id is not None:
        path += f"/{quote(external_id.strip(), safe='')}"
    return path


def _document(
    body: str,
    title: str | None,
    occurred_at: str | None,
    readers: list[str] | None,
    metadata: dict[str, Any] | None,
) -> dict[str, Any]:
    document: dict[str, Any] = {"body": body}
    optional = {"title": title, "occurred_at": occurred_at, "metadata": metadata}
    document.update({name: value for name, value in optional.items() if value})
    # An empty list hides the document from everyone, so keep it.
    if readers is not None:
        document["readers"] = readers
    return document
