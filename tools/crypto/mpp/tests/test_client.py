from __future__ import annotations

import httpx
import pytest
from mpp import client as client_module
from mpp.client import MppCatalogError, MppClient

CATALOG = {
    "services": [
        {
            "id": "fal",
            "name": "Fal AI",
            "description": "Image generation models",
            "serviceUrl": "https://fal.mpp.example",
            "categories": ["AI", "media"],
            "tags": ["image", "generation"],
            "status": "active",
            "paidEndpoints": 2,
            "endpoints": [
                {
                    "method": "POST",
                    "path": "/generate",
                    "payment": {"intent": "charge", "method": "tempo", "amount": "100"},
                }
            ],
        },
        {
            "id": "exa",
            "name": "Exa",
            "description": "Web search API",
            "url": "https://exa.example",
            "categories": ["search"],
            "tags": ["web", "research"],
            "status": "active",
            "endpoints": [{"method": "POST", "path": "/search", "payment": {"intent": "charge"}}],
        },
        {
            "id": "exa-archive",
            "name": "Exa",
            "description": "Archived Exa service",
            "serviceUrl": "https://archive.example",
            "categories": ["search"],
            "tags": ["archive"],
            "status": "inactive",
            "endpoints": [],
        },
    ]
}


def make_client(handler) -> MppClient:
    client = MppClient()
    client._catalog_http = httpx.Client(transport=httpx.MockTransport(handler))
    return client


def test_list_services_filters_summarizes_and_never_loads_a_private_key(monkeypatch) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        assert str(request.url) == "https://mpp.dev/api/services"
        return httpx.Response(200, json=CATALOG)

    monkeypatch.setattr(
        client_module,
        "secret",
        lambda _: pytest.fail("service discovery must not load MPP_PRIVATE_KEY"),
    )
    client = make_client(handler)

    assert client.list_services(query="image", category="ai", tag="generation") == [
        {
            "id": "fal",
            "name": "Fal AI",
            "description": "Image generation models",
            "service_url": "https://fal.mpp.example",
            "categories": ["AI", "media"],
            "tags": ["image", "generation"],
            "status": "active",
            "paid_endpoints": 2,
        }
    ]


def test_search_matches_all_supported_metadata_and_honors_limit() -> None:
    client = make_client(lambda _: httpx.Response(200, json=CATALOG))

    assert [service["id"] for service in client.search_services("web", limit=1)] == ["exa"]
    assert [service["id"] for service in client.list_services(query="media")] == ["fal"]
    assert [service["id"] for service in client.list_services(query="exa")] == [
        "exa",
        "exa-archive",
    ]


def test_get_service_returns_raw_endpoint_and_payment_metadata() -> None:
    client = make_client(lambda _: httpx.Response(200, json=CATALOG))

    assert client.get_service("fal") == CATALOG["services"][0]


def test_get_service_resolves_an_unambiguous_name_and_rejects_ambiguous_or_unknown_names() -> None:
    single = {"services": [CATALOG["services"][0], CATALOG["services"][1]]}
    client = make_client(lambda _: httpx.Response(200, json=single))
    assert client.get_service("fal ai")["id"] == "fal"

    ambiguous = make_client(lambda _: httpx.Response(200, json=CATALOG))
    with pytest.raises(ValueError, match="ambiguous"):
        ambiguous.get_service("Exa")

    with pytest.raises(ValueError, match="was not found"):
        client.get_service("missing")


@pytest.mark.parametrize("payload", [{}, {"services": {}}, {"services": ["not-a-service"]}])
def test_catalog_rejects_invalid_shapes(payload) -> None:
    client = make_client(lambda _: httpx.Response(200, json=payload))

    with pytest.raises(MppCatalogError, match="invalid services list"):
        client.list_services()


def test_catalog_errors_are_concise() -> None:
    def http_error(request: httpx.Request) -> httpx.Response:
        return httpx.Response(503, request=request)

    client = make_client(http_error)
    with pytest.raises(MppCatalogError, match="HTTP 503"):
        client.list_services()

    malformed = make_client(lambda _: httpx.Response(200, content=b"not-json"))
    with pytest.raises(MppCatalogError, match="invalid JSON"):
        malformed.list_services()


@pytest.mark.parametrize("limit", [0, 101])
def test_list_services_rejects_unsafe_limits(limit: int) -> None:
    client = MppClient()

    with pytest.raises(ValueError, match="between 1 and 100"):
        client.list_services(limit=limit)
