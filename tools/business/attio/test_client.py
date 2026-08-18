from pathlib import Path

import httpx
import pytest
from client import AttioClient


def test_upload_file_sends_multipart_pdf(tmp_path: Path) -> None:
    pdf = tmp_path / "brief.pdf"
    pdf.write_bytes(b"%PDF-1.7\nexample")

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/v2/files/upload"
        assert request.headers["authorization"] == "Bearer test-key"
        assert request.headers["content-type"].startswith("multipart/form-data; boundary=")
        body = request.read()
        assert b'name="object"' in body
        assert b"companies" in body
        assert b'name="record_id"' in body
        assert b"bf071e1f-6035-429d-b874-d83ea64ea13b" in body
        assert b'filename="brief.pdf"' in body
        assert b"Content-Type: application/pdf" in body
        assert b"%PDF-1.7" in body
        return httpx.Response(
            201,
            json={"data": {"id": {"file_id": "file-123"}, "name": "brief.pdf"}},
        )

    client = AttioClient(api_key="test-key")
    client._client = httpx.Client(
        base_url="https://api.attio.com/v2",
        headers={"Authorization": "Bearer test-key"},
        transport=httpx.MockTransport(handler),
    )

    result = client.upload_file(
        "companies",
        "bf071e1f-6035-429d-b874-d83ea64ea13b",
        str(pdf),
    )

    assert result == {"id": {"file_id": "file-123"}, "name": "brief.pdf"}


def test_upload_file_rejects_missing_path(tmp_path: Path) -> None:
    client = AttioClient(api_key="test-key")

    with pytest.raises(ValueError, match="does not exist"):
        client.upload_file("companies", "record-id", str(tmp_path / "missing.pdf"))


def test_update_task_marks_task_complete() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "PATCH"
        assert request.url.path == "/v2/tasks/task-123"
        assert request.read() == b'{"data":{"is_completed":true}}'
        return httpx.Response(
            200,
            json={
                "data": {
                    "id": {"task_id": "task-123"},
                    "content_plaintext": "Follow up",
                    "is_completed": True,
                }
            },
        )

    client = AttioClient(api_key="test-key")
    client._client = httpx.Client(
        base_url="https://api.attio.com/v2",
        transport=httpx.MockTransport(handler),
    )

    result = client.update_task("task-123", is_completed=True)

    assert result["is_completed"] is True


def test_update_task_rejects_empty_update() -> None:
    client = AttioClient(api_key="test-key")

    with pytest.raises(ValueError, match="At least one task field"):
        client.update_task("task-123")


def test_delete_task() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "DELETE"
        assert request.url.path == "/v2/tasks/task-123"
        return httpx.Response(200, json={})

    client = AttioClient(api_key="test-key")
    client._client = httpx.Client(
        base_url="https://api.attio.com/v2",
        transport=httpx.MockTransport(handler),
    )

    assert client.delete_task("task-123") is True
