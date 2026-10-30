from __future__ import annotations

import json
from functools import partial

import httpx
import pytest
from centaur_tool_dune import cli
from typer.testing import CliRunner

from centaur_sdk.tool_sdk import ToolContext, reset_tool_context, set_tool_context


@pytest.mark.parametrize(
    "args",
    [["query", "123", "--json"], ["raw", "/query/123"]],
)
def test_query_commands_use_sdk_credentials_without_local_key(monkeypatch, args) -> None:
    monkeypatch.delenv("DUNE_API_KEY", raising=False)
    monkeypatch.setattr(cli, "_client", None)
    requests = []
    payload = {"query_id": 123, "name": "Example query"}

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        return httpx.Response(200, json=payload)

    monkeypatch.setattr(
        httpx,
        "Client",
        partial(httpx.Client, transport=httpx.MockTransport(handler)),
    )
    token = set_tool_context(
        ToolContext(name="dune", secrets={"DUNE_API_KEY": "proxy-placeholder"})
    )
    try:
        result = CliRunner().invoke(cli.app, args)

        assert result.exit_code == 0, result.output
        assert json.loads(result.output) == payload
        assert len(requests) == 1
        assert requests[0].method == "GET"
        assert str(requests[0].url) == "https://api.dune.com/api/v1/query/123"
        assert requests[0].headers["X-Dune-API-Key"] == "proxy-placeholder"
    finally:
        if cli._client is not None:
            cli._client.close()
        reset_tool_context(token)
