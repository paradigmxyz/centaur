"""Preview retrieval through the company-context service's query API.

The legacy index is served by ``client.py``. In sandboxes, iron-proxy injects
the principal's API JWT into requests to the service, which limits results to
documents that principal can see.
"""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
from typing import Any
from urllib.parse import quote

API_URL_ENV = "COMPANY_CONTEXT_API_URL"
TIMEOUT_SECONDS = 30


class CompanyContextV2Client:
    """Query the company-context service's POST /query and GET /documents APIs."""

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
                return {"status": "ok", **json.loads(response.read())}
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
    ) -> dict:
        """Search documents visible to the requester; the API validates every field."""
        filters = {
            "types": types,
            "occurred_after": occurred_after,
            "occurred_before": occurred_before,
            "channel_ids": channel_ids,
            "file_ids": file_ids,
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
