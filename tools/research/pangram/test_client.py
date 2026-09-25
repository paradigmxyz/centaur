"""Tests for the Pangram API client."""

import json
from unittest.mock import patch

import httpx

from pangram.client import PangramClient


def test_detect_submits_and_polls_until_success():
    requests = []
    statuses = iter(
        [
            {"task_id": "task-123", "stage": "STAGE_PREPROCESSING"},
            {"stage": "STAGE_SUCCESS", "prediction_short": "Human"},
        ]
    )

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        assert request.headers["x-api-key"] == "test-key"
        if request.method == "POST":
            assert request.url == "https://text.external-api.pangram.com/task"
            assert json.loads(request.content) == {
                "text": "A passage",
                "model": "pangram-4",
                "public_dashboard_link": True,
            }
            return httpx.Response(200, json={"task_id": "task-123"})
        assert request.url == "https://text.external-api.pangram.com/task/task-123"
        return httpx.Response(200, json=next(statuses))

    with PangramClient(api_key="test-key") as client:
        client._client = httpx.Client(transport=httpx.MockTransport(handler))
        with patch("pangram.client.time.sleep") as sleep:
            result = client.detect(
                "A passage",
                model="pangram-4",
                public_dashboard_link=True,
                poll_interval=0.1,
            )

    assert result["prediction_short"] == "Human"
    assert [request.method for request in requests] == ["POST", "GET", "GET"]
    sleep.assert_called_once_with(0.1)


def test_models_and_plagiarism_use_their_documented_endpoints():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.headers["x-api-key"] == "test-key"
        if request.url == "https://text.external-api.pangram.com/models":
            assert request.method == "GET"
            return httpx.Response(200, json={"models": ["default", "pangram-4"]})

        assert request.method == "POST"
        assert request.url == "https://plagiarism.api.pangram.com"
        assert json.loads(request.content) == {"text": "A passage"}
        return httpx.Response(200, json={"plagiarism_detected": False})

    with PangramClient(api_key="test-key") as client:
        client._client = httpx.Client(transport=httpx.MockTransport(handler))
        assert client.list_models() == {"models": ["default", "pangram-4"]}
        assert client.check_plagiarism("A passage") == {"plagiarism_detected": False}
