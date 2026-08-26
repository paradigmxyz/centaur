import json
from datetime import UTC, datetime
from pathlib import Path
from unittest.mock import patch

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


def test_replace_record_values_uses_put() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "PUT"
        assert request.url.path == "/v2/objects/deals/records/deal-123"
        assert request.read() == b'{"data":{"values":{"dependencies_4":[]}}}'
        return httpx.Response(200, json={"data": {"id": {"record_id": "deal-123"}}})

    client = AttioClient(api_key="test-key")
    client._client = httpx.Client(
        base_url="https://api.attio.com/v2",
        headers={"Authorization": "Bearer test-key"},
        transport=httpx.MockTransport(handler),
    )

    result = client.replace_record_values("deals", "deal-123", {"dependencies_4": []})

    assert result == {"id": {"record_id": "deal-123"}}


def test_update_record_rejects_unconfirmed_response() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"data": {"id": {"record_id": "wrong-record"}}})

    client = AttioClient(api_key="test-key")
    client._client = httpx.Client(
        base_url="https://api.attio.com/v2",
        headers={"Authorization": "Bearer test-key"},
        transport=httpx.MockTransport(handler),
    )

    with pytest.raises(RuntimeError, match="did not confirm record deal-123"):
        client.update_record("deals", "deal-123", {"name": "Example"})


def test_query_all_records_paginates_to_short_page() -> None:
    offsets: list[int] = []

    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.read())
        offsets.append(body["offset"])
        count = 2 if body["offset"] == 0 else 1
        return httpx.Response(200, json={"data": [{"page": body["offset"]}] * count})

    client = AttioClient(api_key="test-key")
    client._client = httpx.Client(
        base_url="https://api.attio.com/v2",
        headers={"Authorization": "Bearer test-key"},
        transport=httpx.MockTransport(handler),
    )

    result = client.query_all_records("deals", page_size=2)

    assert offsets == [0, 2]
    assert len(result) == 3


def test_list_all_notes_paginates_to_short_page() -> None:
    offsets: list[int] = []

    def handler(request: httpx.Request) -> httpx.Response:
        offset = int(request.url.params["offset"])
        offsets.append(offset)
        count = 2 if offset == 0 else 0
        return httpx.Response(200, json={"data": [{"page": offset}] * count})

    client = AttioClient(api_key="test-key")
    client._client = httpx.Client(
        base_url="https://api.attio.com/v2",
        headers={"Authorization": "Bearer test-key"},
        transport=httpx.MockTransport(handler),
    )

    result = client.list_all_notes("deals", "deal-123", page_size=2)

    assert offsets == [0, 2]
    assert len(result) == 2


def test_rate_limit_response_retries_then_succeeds() -> None:
    attempts = 0

    def handler(request: httpx.Request) -> httpx.Response:
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            return httpx.Response(429, headers={"Retry-After": "0"}, json={"message": "wait"})
        return httpx.Response(200, json={"data": []})

    client = AttioClient(api_key="test-key")
    client._client = httpx.Client(
        base_url="https://api.attio.com/v2",
        headers={"Authorization": "Bearer test-key"},
        transport=httpx.MockTransport(handler),
    )

    with patch("client.time.sleep") as sleep:
        assert client.list_objects() == []

    assert attempts == 2
    sleep.assert_called_once_with(0.0)


def test_retry_after_http_date_is_parsed() -> None:
    client = AttioClient(api_key="test-key")
    now = datetime(2026, 8, 8, 12, 0, tzinfo=UTC)

    assert client._retry_delay("Sat, 08 Aug 2026 12:00:05 GMT", now=now) == 5.0
