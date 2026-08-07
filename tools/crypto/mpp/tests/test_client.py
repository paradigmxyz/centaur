from __future__ import annotations

import copy
import json
from pathlib import Path

import httpx
import pytest
from mpp.client import MppCatalogError, MppClient, MppPolicyError, MppRequestError

CATALOG = {
    "version": 1,
    "services": [
        {
            "id": "catalog",
            "name": "Catalog",
            "description": "Read and update catalog records",
            "serviceUrl": "https://api.catalog.example",
            "realm": "api.catalog.example",
            "categories": ["data"],
            "tags": ["records"],
            "status": "active",
            "endpoints": [
                {
                    "method": "GET",
                    "path": "/v1/records",
                    "payment": {"intent": "charge", "method": "tempo", "amount": "100"},
                },
                {
                    "method": "GET",
                    "path": "/v1/records/:id",
                    "payment": {"intent": "charge", "method": "tempo", "amount": "25"},
                },
                {
                    "method": "POST",
                    "path": "/v1/records",
                    "payment": {"intent": "charge", "method": "tempo", "amount": "200"},
                },
                {
                    "method": "GET",
                    "path": "/v1/session",
                    "payment": {"intent": "session", "method": "tempo"},
                },
            ],
        },
        {
            "id": "search",
            "name": "Search",
            "description": "Search the web",
            "url": "https://search.example",
            "categories": ["search"],
            "tags": ["web"],
            "status": "active",
            "endpoints": [],
        },
    ],
}


def make_client(
    tmp_path: Path,
    handler,
    *,
    now=lambda: 1_000.0,
    policy_rules=None,
) -> MppClient:
    client = MppClient(
        cache_path=tmp_path / "registry.json",
        cache_ttl_seconds=900,
        max_stale_seconds=86_400,
        policy_rules=policy_rules,
        now=now,
    )
    client._catalog_http = httpx.Client(transport=httpx.MockTransport(handler))
    return client


def test_list_search_and_show_include_cache_and_availability(tmp_path: Path) -> None:
    client = make_client(tmp_path, lambda _: httpx.Response(200, json=CATALOG))

    listed = client.list_services(query="record", category="DATA", tag="records")
    searched = client.search_services("web")
    shown = client.show_service("catalog")

    assert [service["id"] for service in listed["services"]] == ["catalog"]
    assert listed["services"][0]["executable_endpoints"] == 2
    assert listed["services"][0]["available"] is True
    assert listed["services"][0]["unavailable_reasons"] == [
        "POST requires an operator policy rule",
        "unsupported payment intent 'session'",
    ]
    assert listed["cache"] == {
        "fetched_at": 1_000.0,
        "age_seconds": 0,
        "from_cache": False,
        "stale": False,
    }
    assert [service["id"] for service in searched["services"]] == ["search"]
    assert searched["services"][0]["available"] is False
    assert searched["services"][0]["unavailable_reasons"] == ["service has no registered endpoints"]
    assert shown["endpoints"][0]["availability"] == {
        "executable": True,
        "reason": "GET is enabled by default",
    }
    assert shown["endpoints"][2]["availability"] == {
        "executable": False,
        "reason": "POST requires an operator policy rule",
    }
    assert shown["endpoints"][3]["availability"]["reason"] == (
        "unsupported payment intent 'session'"
    )


def test_cache_is_atomic_fresh_and_uses_conditional_refresh(tmp_path: Path) -> None:
    clock = [1_000.0]
    requests: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        if len(requests) == 1:
            return httpx.Response(
                200,
                json=CATALOG,
                headers={"ETag": '"catalog-v1"', "Last-Modified": "Thu, 30 Jul 2026 00:00:00 GMT"},
            )
        assert request.headers["If-None-Match"] == '"catalog-v1"'
        assert request.headers["If-Modified-Since"] == "Thu, 30 Jul 2026 00:00:00 GMT"
        return httpx.Response(304)

    client = make_client(tmp_path, handler, now=lambda: clock[0])
    client.list_services()
    assert len(requests) == 1
    assert json.loads((tmp_path / "registry.json").read_text())["catalog"] == CATALOG

    second = client.list_services()
    assert len(requests) == 1
    assert second["cache"]["from_cache"] is True

    clock[0] += 901
    third = client.list_services()
    assert len(requests) == 2
    assert third["cache"]["fetched_at"] == clock[0]
    assert third["cache"]["stale"] is False


def test_invalid_refresh_never_replaces_valid_stale_cache(tmp_path: Path) -> None:
    clock = [1_000.0]
    responses = [
        httpx.Response(200, json=CATALOG),
        httpx.Response(200, json={"services": "invalid"}),
    ]
    client = make_client(
        tmp_path,
        lambda _: responses.pop(0),
        now=lambda: clock[0],
    )
    client.list_services()
    original = (tmp_path / "registry.json").read_text()

    clock[0] += 901
    result = client.list_services()

    assert result["cache"]["stale"] is True
    assert (tmp_path / "registry.json").read_text() == original


def test_corrupt_or_expired_cache_fails_closed(tmp_path: Path) -> None:
    cache_path = tmp_path / "registry.json"
    cache_path.write_text("{broken")
    client = make_client(
        tmp_path,
        lambda request: httpx.Response(503, request=request),
    )
    with pytest.raises(MppCatalogError, match="HTTP 503"):
        client.list_services()

    cache_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "fetched_at": -100_000.0,
                "etag": None,
                "last_modified": None,
                "catalog": CATALOG,
            }
        )
    )
    with pytest.raises(MppCatalogError, match="HTTP 503"):
        client.request("catalog", "GET", "/v1/records")


def test_policy_allow_and_deny_precedence(tmp_path: Path) -> None:
    client = make_client(
        tmp_path,
        lambda _: httpx.Response(200, json=CATALOG),
        policy_rules=[
            {
                "effect": "allow",
                "service": "catalog",
                "methods": ["POST"],
                "path": "/v1/*",
            },
            {
                "effect": "deny",
                "service": "catalog",
                "methods": ["POST"],
                "path": "/v1/records",
            },
        ],
    )
    client._service_http = httpx.Client(
        transport=httpx.MockTransport(lambda _: httpx.Response(200, json={"ok": True}))
    )

    with pytest.raises(MppPolicyError, match="denied by operator policy"):
        client.request("catalog", "POST", "/v1/records", body={"name": "test"})

    allow = make_client(
        tmp_path / "allowed",
        lambda _: httpx.Response(200, json=CATALOG),
        policy_rules=[
            {
                "effect": "allow",
                "service": "catalog",
                "methods": ["POST"],
                "path": "/v1/records",
            }
        ],
    )
    allow._service_http = httpx.Client(
        transport=httpx.MockTransport(lambda _: httpx.Response(200, json={"ok": True}))
    )
    assert allow.request("catalog", "POST", "/v1/records", body={"name": "test"})["data"] == {
        "ok": True
    }


def test_request_resolves_registered_path_and_never_follows_redirects(tmp_path: Path) -> None:
    captured: list[httpx.Request] = []

    def service_handler(request: httpx.Request) -> httpx.Response:
        captured.append(request)
        return httpx.Response(
            200,
            json={"id": "a/b"},
            headers={"Payment-Receipt": "redacted-receipt"},
        )

    client = make_client(tmp_path, lambda _: httpx.Response(200, json=CATALOG))
    client._service_http = httpx.Client(transport=httpx.MockTransport(service_handler))

    result = client.request(
        "catalog",
        "GET",
        "/v1/records/:id",
        path_params={"id": "abc 123"},
        query={"include": "summary"},
    )

    assert str(captured[0].url) == (
        "https://api.catalog.example/v1/records/abc%20123?include=summary"
    )
    assert result["payment_receipt_present"] is True
    assert "redacted-receipt" not in json.dumps(result)

    redirecting = make_client(tmp_path / "redirect", lambda _: httpx.Response(200, json=CATALOG))
    redirecting._service_http = httpx.Client(
        transport=httpx.MockTransport(
            lambda _: httpx.Response(302, headers={"Location": "https://evil.example"})
        )
    )
    with pytest.raises(MppRequestError, match="redirects"):
        redirecting.request("catalog", "GET", "/v1/records")


def test_request_requires_registry_service_id(tmp_path: Path) -> None:
    client = make_client(tmp_path, lambda _: httpx.Response(200, json=CATALOG))

    for value in ("Catalog Service", "CATALOG"):
        with pytest.raises(ValueError, match="service id"):
            client.request(value, "GET", "/v1/records")


def test_request_supports_registered_gateway_realm_and_base_path(tmp_path: Path) -> None:
    catalog = copy.deepcopy(CATALOG)
    catalog["services"][0]["serviceUrl"] = "https://gateway.example/provider"
    catalog["services"][0]["realm"] = "payments.example"
    catalog["services"][0]["endpoints"] = [catalog["services"][0]["endpoints"][0]]
    captured: list[httpx.Request] = []

    def service_handler(request: httpx.Request) -> httpx.Response:
        captured.append(request)
        return httpx.Response(200, json={"ok": True})

    client = make_client(tmp_path, lambda _: httpx.Response(200, json=catalog))
    client._service_http = httpx.Client(transport=httpx.MockTransport(service_handler))

    result = client.request("catalog", "GET", "/v1/records")

    assert result["status"] == 200
    assert str(captured[0].url) == "https://gateway.example/provider/v1/records"


@pytest.mark.parametrize(
    ("path_params", "message"),
    [
        ({}, "missing"),
        ({"id": "ok", "extra": "no"}, "unexpected"),
        ({"id": "../admin"}, "invalid"),
        ({"id": "%2Fadmin"}, "invalid"),
    ],
)
def test_path_parameter_validation(
    tmp_path: Path, path_params: dict[str, str], message: str
) -> None:
    client = make_client(tmp_path, lambda _: httpx.Response(200, json=CATALOG))
    with pytest.raises(ValueError, match=message):
        client.request(
            "catalog",
            "GET",
            "/v1/records/:id",
            path_params=path_params,
        )


def test_payment_402_is_concise_and_does_not_expose_challenge(tmp_path: Path) -> None:
    client = make_client(tmp_path, lambda _: httpx.Response(200, json=CATALOG))
    client._service_http = httpx.Client(
        transport=httpx.MockTransport(
            lambda _: httpx.Response(
                402,
                headers={"WWW-Authenticate": "Payment secret-challenge"},
                json={"detail": "sensitive"},
            )
        )
    )

    with pytest.raises(MppRequestError, match="payment was not authorized") as error:
        client.request("catalog", "GET", "/v1/records")
    assert "secret-challenge" not in str(error.value)


class ChunkedStream(httpx.SyncByteStream):
    def __iter__(self):
        yield b"1234"
        yield b"5678"


@pytest.mark.parametrize("advertise_size", [False, True])
def test_response_size_limit_is_enforced_while_streaming(
    tmp_path: Path, advertise_size: bool
) -> None:
    headers = {"Content-Length": "8"} if advertise_size else {}
    client = make_client(tmp_path, lambda _: httpx.Response(200, json=CATALOG))
    client.max_response_bytes = 5
    client._service_http = httpx.Client(
        transport=httpx.MockTransport(
            lambda _: httpx.Response(200, headers=headers, stream=ChunkedStream())
        )
    )

    with pytest.raises(MppRequestError, match="size limit"):
        client.request("catalog", "GET", "/v1/records")


@pytest.mark.parametrize("payload", [{}, {"services": {}}, {"services": ["bad"]}])
def test_catalog_rejects_invalid_shapes(tmp_path: Path, payload: object) -> None:
    client = make_client(tmp_path, lambda _: httpx.Response(200, json=payload))
    with pytest.raises(MppCatalogError, match="invalid services list"):
        client.list_services()


def test_catalog_keeps_specific_and_generic_routes_available(tmp_path: Path) -> None:
    catalog = copy.deepcopy(CATALOG)
    generic = copy.deepcopy(catalog["services"][0])
    generic["id"] = "generic"
    generic["endpoints"] = [copy.deepcopy(generic["endpoints"][0])]
    generic["endpoints"][0]["path"] = "/v1/:resource"
    catalog["services"].append(generic)
    client = make_client(tmp_path, lambda _: httpx.Response(200, json=catalog))

    listed = client.list_services()

    services = {service["id"]: service for service in listed["services"]}
    assert services["catalog"]["available"] is True
    assert services["generic"]["available"] is True


def test_equal_specificity_routes_are_unavailable_but_catalog_remains_usable(
    tmp_path: Path,
) -> None:
    catalog = copy.deepcopy(CATALOG)
    ambiguous = copy.deepcopy(catalog["services"][0])
    ambiguous["id"] = "ambiguous"
    ambiguous["endpoints"] = [copy.deepcopy(ambiguous["endpoints"][1])]
    ambiguous["endpoints"][0]["path"] = "/v1/records/:record"
    catalog["services"].append(ambiguous)
    client = make_client(tmp_path, lambda _: httpx.Response(200, json=catalog))

    shown = client.show_service("catalog")
    route = next(endpoint for endpoint in shown["endpoints"] if endpoint["path"].endswith(":id"))

    assert route["availability"] == {
        "executable": False,
        "reason": "route overlaps another equally specific registry entry",
    }
    assert shown["endpoints"][0]["availability"]["executable"] is True
    with pytest.raises(MppPolicyError, match="equally specific"):
        client.request("catalog", "GET", "/v1/records/:id", {"id": "record-1"})


def test_root_route_does_not_overlap_nonempty_parameter(tmp_path: Path) -> None:
    catalog = {
        "services": [
            {
                "id": "storage",
                "serviceUrl": "https://storage.example",
                "status": "active",
                "endpoints": [
                    {
                        "method": "GET",
                        "path": "/",
                        "payment": {"intent": "charge", "method": "tempo"},
                    },
                    {
                        "method": "GET",
                        "path": "/:key",
                        "payment": {"intent": "charge", "method": "tempo"},
                    },
                ],
            }
        ]
    }
    client = make_client(tmp_path, lambda _: httpx.Response(200, json=catalog))

    shown = client.show_service("storage")

    assert all(endpoint["availability"]["executable"] for endpoint in shown["endpoints"])


@pytest.mark.parametrize(
    "service_url",
    [
        "https://api.catalog.example/base?target=other",
        "https://api.catalog.example/base#fragment",
        "https://api.catalog.example/base/%2e%2e/admin",
        "https://api.catalog.example:invalid/base",
    ],
)
def test_catalog_rejects_unsafe_service_base_urls(tmp_path: Path, service_url: str) -> None:
    catalog = copy.deepcopy(CATALOG)
    catalog["services"][0]["serviceUrl"] = service_url
    client = make_client(tmp_path, lambda _: httpx.Response(200, json=catalog))

    with pytest.raises(MppCatalogError, match="invalid URL"):
        client.list_services()


@pytest.mark.parametrize("limit", [0, 101])
def test_list_services_rejects_unsafe_limits(tmp_path: Path, limit: int) -> None:
    client = make_client(tmp_path, lambda _: httpx.Response(200, json=CATALOG))
    with pytest.raises(ValueError, match="between 1 and 100"):
        client.list_services(limit=limit)
