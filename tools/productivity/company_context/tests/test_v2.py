from __future__ import annotations

import json
import sys
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import ClassVar

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from company_context.v2 import CompanyContextV2Client

DOCUMENT = {
    "document_id": "google-drive:file-1:0",
    "type": "drive_doc",
    "title": "Roadmap",
    "url": "https://drive.google.com/file/d/file-1/view",
    "text": "Roadmap covers launch sequencing.",
    "occurred_at": "2026-05-08T12:00:00Z",
    "metadata": {"file_id": "file-1"},
}

CUSTOM_DOCUMENT = {
    "source": "memory",
    "external_id": "0190f0c4-7a6e-7cc3-8000-000000000001",
    "body": "Ada prefers Neovim",
    "readers": ["principal:prn_5CO4fITZ"],
    "status": "pending",
}


class _FakeApi(BaseHTTPRequestHandler):
    """Stands in for the company-context query API."""

    requests: ClassVar[list[tuple[str, str, object]]] = []

    def _reply(self, status: int, body: dict) -> None:
        payload = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_POST(self) -> None:
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.requests.append(("POST", self.path, body))
        if self.path.startswith("/custom-documents/"):
            self._reply(201, {**CUSTOM_DOCUMENT, **body})
        elif not body["query"].strip():
            self._reply(400, {"error": "query cannot be empty"})
        else:
            self._reply(200, {"results": [{**DOCUMENT, "score": 0.03}]})

    def do_PUT(self) -> None:
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.requests.append(("PUT", self.path, body))
        self._reply(404, {"error": "document not found"})

    def do_DELETE(self) -> None:
        self.requests.append(("DELETE", self.path, None))
        self.send_response(204)
        self.end_headers()

    def do_GET(self) -> None:
        self.requests.append(("GET", self.path, None))
        if self.path == "/documents/google-drive%3Afile-1%3A0":
            self._reply(200, DOCUMENT)
        elif self.path == "/custom-documents/memory?limit=2":
            self._reply(200, {"documents": [CUSTOM_DOCUMENT]})
        else:
            self._reply(404, {"error": "document not found"})

    def log_message(self, *_args) -> None:
        pass


@pytest.fixture
def api_url(monkeypatch) -> Iterator[str]:
    for name in ("HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"):
        monkeypatch.delenv(name, raising=False)
    _FakeApi.requests = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), _FakeApi)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/"
    finally:
        server.shutdown()
        server.server_close()


def test_search_posts_filters_and_returns_results(api_url):
    result = CompanyContextV2Client(api_url).search(
        "roadmap",
        limit=5,
        types=["drive_doc", "custom_doc"],
        occurred_after="2026-05-01T00:00:00Z",
        sources=["archive"],
        metadata={"author": "Ada"},
    )

    assert result == {"status": "ok", "results": [{**DOCUMENT, "score": 0.03}]}
    assert _FakeApi.requests == [
        (
            "POST",
            "/query",
            {
                "query": "roadmap",
                "filters": {
                    "types": ["drive_doc", "custom_doc"],
                    "occurred_after": "2026-05-01T00:00:00Z",
                    "sources": ["archive"],
                    "metadata": {"author": "Ada"},
                },
                "limit": 5,
            },
        )
    ]


def test_search_reports_api_errors(api_url):
    result = CompanyContextV2Client(api_url).search(" ")

    assert result == {"status": "error", "error": "HTTP 400: query cannot be empty"}


def test_read_document_escapes_document_id(api_url):
    client = CompanyContextV2Client(api_url)

    assert client.read_document("google-drive:file-1:0") == {"status": "ok", **DOCUMENT}
    assert client.read_document("slack:C1/../x") == {
        "status": "error",
        "error": "HTTP 404: document not found",
    }
    assert _FakeApi.requests[-1] == ("GET", "/documents/slack%3AC1%2F..%2Fx", None)


def test_unconfigured_api_url_reports_error(monkeypatch):
    monkeypatch.delenv("COMPANY_CONTEXT_API_URL", raising=False)

    result = CompanyContextV2Client().search("roadmap")

    assert result["status"] == "error"
    assert "COMPANY_CONTEXT_API_URL is not configured" in result["error"]


def test_custom_documents_are_created_listed_updated_and_deleted(api_url):
    client = CompanyContextV2Client(api_url)
    external_id = CUSTOM_DOCUMENT["external_id"]

    created = client.create_document(
        "memory", "Ada prefers Neovim", title="Editor", metadata={"topic": "tools"}
    )
    listed = client.list_documents("memory", limit=2)
    updated = client.update_document("memory", external_id, "Ada prefers Helix", readers=[])
    deleted = client.delete_document("memory", external_id)

    assert created == {
        "status": "ok",
        **CUSTOM_DOCUMENT,
        "title": "Editor",
        "metadata": {"topic": "tools"},
    }
    assert listed == {"status": "ok", "documents": [CUSTOM_DOCUMENT]}
    assert updated == {"status": "error", "error": "HTTP 404: document not found"}
    assert deleted == {"status": "ok"}
    assert _FakeApi.requests == [
        (
            "POST",
            "/custom-documents/memory",
            {"body": "Ada prefers Neovim", "title": "Editor", "metadata": {"topic": "tools"}},
        ),
        ("GET", "/custom-documents/memory?limit=2", None),
        (
            "PUT",
            f"/custom-documents/memory/{external_id}",
            {"body": "Ada prefers Helix", "readers": []},
        ),
        ("DELETE", f"/custom-documents/memory/{external_id}", None),
    ]
