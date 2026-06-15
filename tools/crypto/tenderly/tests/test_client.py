from __future__ import annotations

import json

import httpx
import pytest
from tenderly.client import (
    TenderlyClient,
    error_path,
    extract_call_trace,
    find_failures,
    flatten_call_trace,
    trace_skeleton,
)


def make_client(handler) -> TenderlyClient:
    client = TenderlyClient(access_key="test-key", account="acct", project="proj")
    client._http = httpx.Client(transport=httpx.MockTransport(handler))
    return client


def test_simulate_posts_payload_with_auth() -> None:
    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["url"] = str(request.url)
        captured["access_key"] = request.headers.get("X-Access-Key")
        captured["body"] = json.loads(request.content)
        return httpx.Response(
            200, json={"transaction": {"status": True, "gas_used": 21000}}
        )

    client = make_client(handler)
    result = client.simulate(
        network_id="1",
        from_address="0xabc",
        to_address="0xdef",
        input_data="0x1234",
        value=5,
        gas=1_000_000,
        block_number=42,
    )

    assert captured["url"] == (
        "https://api.tenderly.co/api/v1/account/acct/project/proj/simulate"
    )
    assert captured["access_key"] == "test-key"
    assert captured["body"]["network_id"] == "1"
    assert captured["body"]["from"] == "0xabc"
    assert captured["body"]["to"] == "0xdef"
    assert captured["body"]["input"] == "0x1234"
    assert captured["body"]["value"] == 5
    assert captured["body"]["gas"] == 1_000_000
    assert captured["body"]["block_number"] == 42
    assert captured["body"]["simulation_type"] == "full"
    assert result["transaction"]["status"] is True


def test_simulate_omits_optional_fields() -> None:
    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={})

    client = make_client(handler)
    client.simulate(network_id="1", from_address="0xabc", to_address="0xdef")

    assert "block_number" not in captured["body"]
    assert "gas_price" not in captured["body"]
    assert "state_objects" not in captured["body"]


def test_api_error_raises_runtime_error() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(400, text="invalid simulation")

    client = make_client(handler)
    with pytest.raises(RuntimeError, match="400 - invalid simulation"):
        client.simulate(network_id="1", from_address="0xabc", to_address="0xdef")


def test_get_contract_lowercases_address() -> None:
    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["url"] = str(request.url)
        return httpx.Response(200, json={"contract_name": "WETH9"})

    client = make_client(handler)
    client.get_contract("1", "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

    assert captured["url"] == (
        "https://api.tenderly.co/api/v1/public-contract/1/"
        "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
    )


def test_get_networks_handles_list_response() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json=[{"id": "1", "name": "Mainnet"}])

    client = make_client(handler)
    assert client.get_networks() == [{"id": "1", "name": "Mainnet"}]


def test_create_vnet_payload() -> None:
    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["url"] = str(request.url)
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={"id": "vnet-1", "slug": "my-fork"})

    client = make_client(handler)
    vnet = client.create_vnet(network_id="8453", slug="my-fork", chain_id=73571)

    assert captured["url"].endswith("/account/acct/project/proj/vnets")
    assert captured["body"]["slug"] == "my-fork"
    assert captured["body"]["display_name"] == "my-fork"
    assert captured["body"]["fork_config"] == {
        "network_id": 8453,
        "block_number": "latest",
    }
    assert (
        captured["body"]["virtual_network_config"]["chain_config"]["chain_id"] == 73571
    )
    assert vnet["id"] == "vnet-1"


def test_vnet_rpc_uses_admin_rpc_and_hex_encodes() -> None:
    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.host == "api.tenderly.co":
            return httpx.Response(
                200,
                json={
                    "id": "vnet-1",
                    "rpcs": [
                        {"name": "Public RPC", "url": "https://virtual.mainnet.rpc.tenderly.co/pub"},
                        {"name": "Admin RPC", "url": "https://virtual.mainnet.rpc.tenderly.co/admin"},
                    ],
                },
            )
        captured["url"] = str(request.url)
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={"jsonrpc": "2.0", "id": 1, "result": "0x1"})

    client = make_client(handler)
    client.set_balance("vnet-1", ["0xabc"], 10**18)

    assert captured["url"] == "https://virtual.mainnet.rpc.tenderly.co/admin"
    assert captured["body"]["method"] == "tenderly_setBalance"
    assert captured["body"]["params"] == [["0xabc"], hex(10**18)]


def test_vnet_rpc_error_raises() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.host == "api.tenderly.co":
            return httpx.Response(
                200,
                json={"id": "vnet-1", "rpcs": [{"name": "Admin RPC", "url": "https://rpc.test/x"}]},
            )
        return httpx.Response(
            200, json={"jsonrpc": "2.0", "id": 1, "error": {"message": "boom"}}
        )

    client = make_client(handler)
    with pytest.raises(RuntimeError, match="RPC error"):
        client.snapshot("vnet-1")


SAMPLE_TRACE = {
    "contract_name": "Router",
    "function_name": "swap",
    "from": "0xaaa",
    "to": "0xbbb",
    "gas_used": 100,
    "calls": [
        {
            "contract_name": "Token",
            "function_name": "transfer",
            "gas_used": 40,
            "calls": [],
        },
        {
            "contract_name": "Pool",
            "function_name": "mint",
            "gas_used": 50,
            "error": "execution reverted",
            "calls": [
                {
                    "contract_name": "Token",
                    "function_name": "transferFrom",
                    "gas_used": 10,
                    "error": "ERC20: insufficient allowance",
                    "calls": [],
                }
            ],
        },
    ],
}


def test_flatten_call_trace_depths_and_labels() -> None:
    flat = flatten_call_trace(SAMPLE_TRACE)

    assert [(f["depth"], f["label"]) for f in flat] == [
        (0, "Router.swap"),
        (1, "Token.transfer"),
        (1, "Pool.mint"),
        (2, "Token.transferFrom"),
    ]


def test_find_failures_collects_all_errors() -> None:
    failures = find_failures(SAMPLE_TRACE)

    assert len(failures) == 2
    assert failures[0]["label"] == "Pool.mint"
    assert failures[1]["error"] == "ERC20: insufficient allowance"


def test_error_path_requires_root_error() -> None:
    assert error_path(SAMPLE_TRACE) == []


def test_error_path_follows_blame_chain() -> None:
    errored_root = {**SAMPLE_TRACE, "error": "execution reverted"}
    path = error_path(errored_root)

    assert [p["label"] for p in path] == ["Router.swap", "Pool.mint", "Token.transferFrom"]
    assert path[-1]["error"] == "ERC20: insufficient allowance"


def test_trace_skeleton_truncates_at_max_depth() -> None:
    skeleton = trace_skeleton(SAMPLE_TRACE, max_depth=1)

    assert [s["label"] for s in skeleton] == ["Router.swap", "Token.transfer", "Pool.mint"]
    truncated = [s for s in skeleton if s["truncated"]]
    assert len(truncated) == 1
    assert truncated[0]["label"] == "Pool.mint"


def test_extract_call_trace_from_simulation_response() -> None:
    result = {
        "transaction": {"transaction_info": {"call_trace": SAMPLE_TRACE}},
    }
    assert extract_call_trace(result)["function_name"] == "swap"


def test_extract_call_trace_from_trace_response_list() -> None:
    result = {"call_trace": [SAMPLE_TRACE]}
    assert extract_call_trace(result)["function_name"] == "swap"
