from __future__ import annotations

import json

import httpx
from centaur_tool_dune.client import DuneClient


def test_execute_query_sends_empty_json_body_without_parameters() -> None:
    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={"execution_id": "execution-1"})

    client = DuneClient(api_key="test-key")
    client._client = httpx.Client(
        base_url="https://api.dune.com/api/v1",
        transport=httpx.MockTransport(handler),
    )

    result = client.execute_query(2408388)

    assert captured["body"] == {}
    assert result == {"execution_id": "execution-1"}


def test_execute_query_sends_parameters() -> None:
    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={"execution_id": "execution-2"})

    client = DuneClient(api_key="test-key")
    client._client = httpx.Client(
        base_url="https://api.dune.com/api/v1",
        transport=httpx.MockTransport(handler),
    )

    result = client.execute_query(123, {"chain": "ethereum"})

    assert captured["body"] == {"query_parameters": {"chain": "ethereum"}}
    assert result == {"execution_id": "execution-2"}
