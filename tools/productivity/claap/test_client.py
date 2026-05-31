from __future__ import annotations

import importlib.util
from pathlib import Path

import httpx
import pytest

spec = importlib.util.spec_from_file_location(
    "claap_client", Path(__file__).with_name("client.py")
)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
ClaapClient = module.ClaapClient
recording_id_from_input = module.recording_id_from_input


def _mock_client(client: ClaapClient, handler) -> None:
    client._client = httpx.Client(  # type: ignore[attr-defined]
        base_url=client.base_url,
        headers={"X-Claap-Key": client._api_key()},  # type: ignore[attr-defined]
        transport=httpx.MockTransport(handler),
    )


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        ("Rj7xFLInq4B8", "Rj7xFLInq4B8"),
        ("https://app.claap.io/workspace/recordings/Rj7xFLInq4B8", "Rj7xFLInq4B8"),
        ("https://app.claap.io/share/foo?recordingId=Rj7xFLInq4B8", "Rj7xFLInq4B8"),
        ("https://app.claap.io/Rj7xFLInq4B8?from=share", "Rj7xFLInq4B8"),
    ],
)
def test_recording_id_from_input(value: str, expected: str) -> None:
    assert recording_id_from_input(value) == expected


def test_list_recordings_uses_claap_filters_and_header() -> None:
    client = ClaapClient(api_key="test-key")

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "GET"
        assert request.url.path == "/v1/recordings"
        assert request.headers["X-Claap-Key"] == "test-key"
        assert request.url.params["limit"] == "7"
        assert request.url.params["createdAfter"] == "2026-05-27"
        assert request.url.params["createdBefore"] == "2026-05-28"
        assert request.url.params["channelId"] == "chan_123"
        assert request.url.params["recorderEmail"] == "ops@example.com"
        return httpx.Response(200, request=request, json={"result": {"recordings": []}})

    _mock_client(client, handler)
    try:
        result = client.list_recordings(
            limit=7,
            created_after="2026-05-27T12:00:00Z",
            created_before="2026-05-28",
            channel_id="chan_123",
            recorder_email="ops@example.com",
        )
    finally:
        client.close()

    assert result == {"result": {"recordings": []}}


def test_get_recording_requests_ai_fields_by_default() -> None:
    client = ClaapClient(api_key="test-key")

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/v1/recordings/Rj7xFLInq4B8"
        assert request.url.params["returnAiFields"] == "true"
        return httpx.Response(
            200,
            request=request,
            json={"result": {"recording": {"id": "Rj7xFLInq4B8"}}},
        )

    _mock_client(client, handler)
    try:
        result = client.get_recording("https://app.claap.io/recordings/Rj7xFLInq4B8")
    finally:
        client.close()

    assert result["result"]["recording"]["id"] == "Rj7xFLInq4B8"


def test_text_transcript_returns_wrapped_text() -> None:
    client = ClaapClient(api_key="test-key")

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/v1/recordings/Rj7xFLInq4B8/transcript"
        assert request.url.params["format"] == "text"
        assert request.url.params["lang"] == "en"
        return httpx.Response(200, request=request, text="Speaker: hello")

    _mock_client(client, handler)
    try:
        result = client.get_transcript("Rj7xFLInq4B8", format="text", lang="en")
    finally:
        client.close()

    assert result == {
        "recording_id": "Rj7xFLInq4B8",
        "format": "text",
        "transcript": "Speaker: hello",
    }


def test_invalid_date_filter_fails_before_request() -> None:
    client = ClaapClient(api_key="test-key")
    try:
        with pytest.raises(ValueError, match="YYYY-MM-DD"):
            client.list_recordings(created_after="May 27")
    finally:
        client.close()
