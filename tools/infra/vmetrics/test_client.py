import httpx
import pytest
from client import VictoriaMetricsClient


def make_client(handler):
    return VictoriaMetricsClient(
        url="http://victoriametrics:8428",
        transport=httpx.MockTransport(handler),
    )


def json_response(payload, status_code=200):
    return httpx.Response(status_code, json=payload)


def test_series_accepts_limit_for_tool_bridge():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/api/v1/series"
        assert request.url.params.get("match[]") == '{__name__="up"}'
        return json_response(
            {
                "status": "success",
                "data": [
                    {"__name__": "up", "instance": "one"},
                    {"__name__": "up", "instance": "two"},
                ],
            }
        )

    assert make_client(handler).series('{__name__="up"}', limit=1) == [
        {"__name__": "up", "instance": "one"}
    ]


def test_series_wraps_http_errors_with_response_body():
    def handler(request: httpx.Request) -> httpx.Response:
        return json_response({"error": "too many series selected"}, status_code=422)

    with pytest.raises(RuntimeError, match="HTTP 422"):
        make_client(handler).series('{__name__=~".+"}')


def test_health_checks_query_metric_names_and_series():
    seen_paths: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen_paths.append(request.url.path)
        if request.url.path == "/health":
            return httpx.Response(200)
        if request.url.path == "/api/v1/query":
            return json_response(
                {
                    "status": "success",
                    "data": {
                        "resultType": "vector",
                        "result": [{"metric": {}, "value": [1, "3"]}],
                    },
                }
            )
        if request.url.path == "/api/v1/label/__name__/values":
            return json_response({"status": "success", "data": ["up", "centaur_sessions_total"]})
        if request.url.path == "/api/v1/series":
            return json_response({"status": "success", "data": [{"__name__": "up"}]})
        raise AssertionError(f"unexpected path: {request.url.path}")

    result = make_client(handler).health()

    assert result["ok"] is True
    assert result["details"]["ready"] is True
    assert result["details"]["query_ok"] is True
    assert result["details"]["metric_names_count"] == 2
    assert result["details"]["series_count"] == 1
    assert seen_paths == [
        "/health",
        "/api/v1/query",
        "/api/v1/label/__name__/values",
        "/api/v1/series",
    ]
