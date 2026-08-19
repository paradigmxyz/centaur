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
