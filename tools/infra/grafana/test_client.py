from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import httpx
import pytest

ROOT = Path(__file__).resolve().parents[3]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

spec = importlib.util.spec_from_file_location(
    "grafana_client", Path(__file__).with_name("client.py")
)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
GrafanaClient = module.GrafanaClient

LOKI_STREAMS = {
    "status": "success",
    "data": {
        "resultType": "streams",
        "result": [
            {
                "stream": {"app": "api", "level": "error"},
                "values": [
                    ["1722180003000000000", "third"],
                    ["1722180001000000000", "first"],
                ],
            },
            {
                "stream": {"app": "worker"},
                "values": [["1722180001500000000", "second", {"trace_id": "abc123"}]],
            },
        ],
    },
}

EMPTY_STREAMS = {"status": "success", "data": {"resultType": "streams", "result": []}}


def make_client(handler) -> GrafanaClient:
    client = GrafanaClient(url="http://grafana.test", api_key="test-token")
    client._client = httpx.Client(
        base_url="http://grafana.test", transport=httpx.MockTransport(handler)
    )
    return client


def test_query_loki_builds_query_range_request() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "GET"
        assert request.url.path == "/api/datasources/proxy/uid/loki/loki/api/v1/query_range"
        assert dict(request.url.params) == {"query": '{app="api"}', "limit": "100"}
        return httpx.Response(200, json=EMPTY_STREAMS)

    client = make_client(handler)

    assert client.query_loki('{app="api"}') == []


def test_query_loki_passes_range_and_datasource_uid() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/api/datasources/proxy/uid/loki-prod/loki/api/v1/query_range"
        params = dict(request.url.params)
        assert params["start"] == "2024-07-28T00:00:00Z"
        assert params["end"] == "2024-07-28T16:00:00Z"
        assert params["limit"] == "10"
        return httpx.Response(200, json=EMPTY_STREAMS)

    client = make_client(handler)

    client.query_loki(
        '{app="api"}',
        datasource_uid="loki-prod",
        start="2024-07-28T00:00:00Z",
        end="2024-07-28T16:00:00Z",
        limit=10,
    )


def test_query_loki_flattens_streams_sorted_by_time() -> None:
    client = make_client(lambda _: httpx.Response(200, json=LOKI_STREAMS))

    assert client.query_loki('{app=~".+"}') == [
        {
            "time": "2024-07-28T15:20:01+00:00",
            "stream": {"app": "api", "level": "error"},
            "line": "first",
        },
        {
            "time": "2024-07-28T15:20:01.500000+00:00",
            "stream": {"app": "worker"},
            "line": "second",
        },
        {
            "time": "2024-07-28T15:20:03+00:00",
            "stream": {"app": "api", "level": "error"},
            "line": "third",
        },
    ]


def test_query_loki_rejects_matrix_results() -> None:
    matrix = {
        "status": "success",
        "data": {
            "resultType": "matrix",
            "result": [{"metric": {"app": "api"}, "values": [[1722180001, "0.5"]]}],
        },
    }
    client = make_client(lambda _: httpx.Response(200, json=matrix))

    with pytest.raises(ValueError, match="resultType 'matrix'"):
        client.query_loki('rate({app="api"}[5m])')


def test_query_loki_raises_on_http_error() -> None:
    client = make_client(lambda _: httpx.Response(500, text="boom"))

    with pytest.raises(RuntimeError, match=r"Grafana API error \(500\)"):
        client.query_loki('{app="api"}')


def test_loki_labels_returns_names() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/api/datasources/proxy/uid/loki/loki/api/v1/labels"
        return httpx.Response(200, json={"status": "success", "data": ["app", "level"]})

    client = make_client(handler)

    assert client.loki_labels() == ["app", "level"]


def test_loki_label_values_returns_values() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/api/datasources/proxy/uid/loki/loki/api/v1/label/app/values"
        return httpx.Response(200, json={"status": "success", "data": ["api", "worker"]})

    client = make_client(handler)

    assert client.loki_label_values("app") == ["api", "worker"]
