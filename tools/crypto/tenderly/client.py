"""Tenderly API client."""

from __future__ import annotations

from typing import Any

import httpx

from centaur_sdk import secret

API_BASE = "https://api.tenderly.co/api/v1"


class TenderlyClient:
    """Client for the Tenderly API and Virtual TestNet RPC endpoints."""

    def __init__(
        self,
        access_key: str | None = None,
        account: str | None = None,
        project: str | None = None,
        timeout: float = 60.0,
    ):
        self.access_key = access_key
        self.account = account
        self.project = project
        self.timeout = timeout
        self._http: httpx.Client | None = None
        self._vnet_rpc_cache: dict[str, dict] = {}

    def _get_access_key(self) -> str:
        """Get access key from env var."""
        if self.access_key:
            return self.access_key
        key = secret("TENDERLY_ACCESS_KEY", "")
        if key:
            return key
        raise RuntimeError("TENDERLY_ACCESS_KEY not set.")

    def _get_account(self) -> str:
        """Get account slug from env var."""
        account = self.account or secret("TENDERLY_ACCOUNT_SLUG", "")
        if account:
            return account
        raise RuntimeError("TENDERLY_ACCOUNT_SLUG not set.")

    def _get_project(self) -> str:
        """Get project slug from env var."""
        project = self.project or secret("TENDERLY_PROJECT_SLUG", "")
        if project:
            return project
        raise RuntimeError("TENDERLY_PROJECT_SLUG not set.")

    def _project_path(self) -> str:
        return f"/account/{self._get_account()}/project/{self._get_project()}"

    @property
    def http(self) -> httpx.Client:
        """Get or create the HTTP client."""
        if self._http is None:
            self._http = httpx.Client(
                headers={"Content-Type": "application/json"},
                timeout=self.timeout,
            )
        return self._http

    def close(self) -> None:
        """Close the HTTP client."""
        if self._http is not None:
            self._http.close()
            self._http = None

    def __enter__(self) -> TenderlyClient:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()

    def _request(
        self,
        method: str,
        path: str,
        json: dict | None = None,
        params: dict | None = None,
    ) -> Any:
        """Make an authenticated request against the Tenderly API."""
        response = self.http.request(
            method,
            f"{API_BASE}{path}",
            json=json,
            params=params,
            headers={"X-Access-Key": self._get_access_key()},
        )
        if response.status_code >= 400:
            raise RuntimeError(
                f"Tenderly API error: {response.status_code} - {response.text}"
            )
        if not response.content:
            return None
        return response.json()

    # --- Account & networks ---

    def get_user(self) -> dict:
        """Get info about the authenticated user."""
        return self._request("GET", "/user")

    def list_projects(self) -> list[dict]:
        """List projects for the configured account."""
        result = self._request("GET", f"/account/{self._get_account()}/projects")
        return result.get("projects", []) if isinstance(result, dict) else result

    def get_networks(self) -> list[dict]:
        """List public EVM networks supported by Tenderly."""
        result = self._request("GET", "/public-networks")
        return result if isinstance(result, list) else result.get("networks", [])

    # --- Contracts ---

    def get_contract(self, network_id: str, address: str) -> dict:
        """Get metadata (name, ABI, compiler) for a verified contract."""
        return self._request(
            "GET", f"/public-contract/{network_id}/{address.lower()}"
        )

    # --- Simulation ---

    def simulate(
        self,
        network_id: str,
        from_address: str,
        to_address: str,
        input_data: str = "0x",
        value: int = 0,
        gas: int = 8_000_000,
        gas_price: int | None = None,
        block_number: int | None = None,
        simulation_type: str = "full",
        state_objects: dict | None = None,
        save: bool = False,
    ) -> dict:
        """Simulate a transaction on a public network."""
        payload: dict[str, Any] = {
            "network_id": str(network_id),
            "from": from_address,
            "to": to_address,
            "input": input_data,
            "value": value,
            "gas": gas,
            "simulation_type": simulation_type,
            "save": save,
            "save_if_fails": save,
        }
        if gas_price is not None:
            payload["gas_price"] = str(gas_price)
        if block_number is not None:
            payload["block_number"] = block_number
        if state_objects:
            payload["state_objects"] = state_objects
        return self._request("POST", f"{self._project_path()}/simulate", json=payload)

    def simulate_bundle(self, simulations: list[dict]) -> dict:
        """Simulate a bundle of transactions executed sequentially."""
        return self._request(
            "POST",
            f"{self._project_path()}/simulate-bundle",
            json={"simulations": simulations},
        )

    # --- Tracing ---

    def trace_transaction(self, network_id: str, tx_hash: str) -> dict:
        """Get the decoded execution trace of an on-chain transaction."""
        return self._request("GET", f"/public-contract/{network_id}/trace/{tx_hash}")

    # --- Virtual TestNets ---

    def create_vnet(
        self,
        network_id: str,
        slug: str,
        display_name: str | None = None,
        block_number: int | str = "latest",
        chain_id: int | None = None,
    ) -> dict:
        """Create a Virtual TestNet by forking a public network."""
        payload: dict[str, Any] = {
            "slug": slug,
            "display_name": display_name or slug,
            "fork_config": {
                "network_id": int(network_id),
                "block_number": block_number,
            },
            "virtual_network_config": {
                "chain_config": {
                    "chain_id": chain_id if chain_id is not None else int(network_id),
                },
            },
            "sync_state_config": {"enabled": False},
            "explorer_page_config": {
                "enabled": False,
                "verification_visibility": "bytecode",
            },
        }
        return self._request("POST", f"{self._project_path()}/vnets", json=payload)

    def list_vnets(self, page: int = 1, per_page: int = 20) -> list[dict]:
        """List Virtual TestNets in the project."""
        result = self._request(
            "GET",
            f"{self._project_path()}/vnets",
            params={"page": page, "perPage": per_page},
        )
        return result if isinstance(result, list) else result.get("vnets", [])

    def get_vnet(self, vnet_id: str) -> dict:
        """Get full details of a Virtual TestNet."""
        return self._request("GET", f"{self._project_path()}/vnets/{vnet_id}")

    def delete_vnet(self, vnet_id: str) -> None:
        """Delete a Virtual TestNet."""
        self._request("DELETE", f"{self._project_path()}/vnets/{vnet_id}")

    def list_vnet_transactions(
        self, vnet_id: str, page: int = 1, per_page: int = 20
    ) -> list[dict]:
        """List transactions executed on a Virtual TestNet."""
        result = self._request(
            "GET",
            f"{self._project_path()}/vnets/{vnet_id}/transactions",
            params={"page": page, "perPage": per_page},
        )
        return result if isinstance(result, list) else result.get("transactions", [])

    def simulate_vnet_transaction(
        self,
        vnet_id: str,
        from_address: str,
        to_address: str,
        input_data: str = "0x",
        value: int = 0,
        gas: int = 8_000_000,
        block_number: int | None = None,
    ) -> dict:
        """Simulate a transaction on a Virtual TestNet without changing state."""
        callargs: dict[str, Any] = {
            "from": from_address,
            "to": to_address,
            "data": input_data,
            "value": hex(value),
            "gas": hex(gas),
        }
        payload: dict[str, Any] = {"callArgs": callargs}
        if block_number is not None:
            payload["blockNumber"] = hex(block_number)
        return self._request(
            "POST",
            f"{self._project_path()}/vnets/{vnet_id}/transactions/simulate",
            json=payload,
        )

    # --- Virtual TestNet RPC ---

    def get_vnet_rpc_url(self, vnet_id: str, admin: bool = True) -> str:
        """Get the (admin) RPC URL for a Virtual TestNet."""
        vnet = self._vnet_rpc_cache.get(vnet_id) or self.get_vnet(vnet_id)
        self._vnet_rpc_cache[vnet_id] = vnet
        rpcs = vnet.get("rpcs", [])
        wanted = "Admin RPC" if admin else "Public RPC"
        for rpc in rpcs:
            if rpc.get("name") == wanted:
                return rpc["url"]
        if rpcs:
            return rpcs[0]["url"]
        raise RuntimeError(f"No RPC endpoints found for vnet {vnet_id}.")

    def vnet_rpc(self, vnet_id: str, method: str, params: list | None = None) -> Any:
        """Make a JSON-RPC call against a Virtual TestNet's admin RPC."""
        url = self.get_vnet_rpc_url(vnet_id, admin=True)
        payload = {
            "jsonrpc": "2.0",
            "method": method,
            "params": params or [],
            "id": 1,
        }
        response = self.http.post(url, json=payload)
        response.raise_for_status()
        result = response.json()
        if "error" in result:
            raise RuntimeError(f"RPC error: {result['error']}")
        return result.get("result")

    def send_vnet_transaction(
        self,
        vnet_id: str,
        from_address: str,
        to_address: str,
        input_data: str = "0x",
        value: int = 0,
        gas: int | None = None,
    ) -> str:
        """Send an unsigned (impersonated) transaction on a Virtual TestNet."""
        tx: dict[str, Any] = {
            "from": from_address,
            "to": to_address,
            "data": input_data,
            "value": hex(value),
        }
        if gas is not None:
            tx["gas"] = hex(gas)
        return self.vnet_rpc(vnet_id, "eth_sendTransaction", [tx])

    def vnet_call(
        self,
        vnet_id: str,
        to_address: str,
        input_data: str,
        from_address: str | None = None,
        block: str = "latest",
    ) -> str:
        """Execute a read-only eth_call on a Virtual TestNet."""
        call: dict[str, Any] = {"to": to_address, "data": input_data}
        if from_address:
            call["from"] = from_address
        return self.vnet_rpc(vnet_id, "eth_call", [call, block])

    def set_balance(self, vnet_id: str, addresses: list[str], amount_wei: int) -> Any:
        """Set the native balance of one or more accounts."""
        return self.vnet_rpc(
            vnet_id, "tenderly_setBalance", [addresses, hex(amount_wei)]
        )

    def set_erc20_balance(
        self, vnet_id: str, token: str, wallet: str, amount: int
    ) -> Any:
        """Set the ERC-20 token balance of a wallet."""
        return self.vnet_rpc(
            vnet_id, "tenderly_setErc20Balance", [token, wallet, hex(amount)]
        )

    def set_storage_at(self, vnet_id: str, contract: str, slot: str, value: str) -> Any:
        """Write a raw value to a contract storage slot."""
        return self.vnet_rpc(vnet_id, "tenderly_setStorageAt", [contract, slot, value])

    def snapshot(self, vnet_id: str) -> str:
        """Save the current Virtual TestNet state and return a snapshot ID."""
        return self.vnet_rpc(vnet_id, "evm_snapshot")

    def revert(self, vnet_id: str, snapshot_id: str) -> Any:
        """Restore a Virtual TestNet to a previously saved snapshot."""
        return self.vnet_rpc(vnet_id, "evm_revert", [snapshot_id])

    def increase_time(self, vnet_id: str, seconds: int) -> Any:
        """Advance the Virtual TestNet clock by the given number of seconds."""
        return self.vnet_rpc(vnet_id, "evm_increaseTime", [hex(seconds)])

    def mine_blocks(self, vnet_id: str, blocks: int = 1) -> Any:
        """Mine one or more blocks on a Virtual TestNet."""
        if blocks == 1:
            return self.vnet_rpc(vnet_id, "evm_mine")
        return self.vnet_rpc(vnet_id, "evm_increaseBlocks", [hex(blocks)])


# --- Trace navigation helpers ---


def extract_call_trace(result: dict) -> dict | None:
    """Pull the root call trace node out of a simulate/trace response."""
    if not isinstance(result, dict):
        return None
    if "call_trace" in result:
        trace = result["call_trace"]
        return trace[0] if isinstance(trace, list) and trace else trace
    transaction = result.get("transaction", {})
    info = transaction.get("transaction_info", {}) if transaction else {}
    trace = info.get("call_trace")
    if isinstance(trace, list):
        return trace[0] if trace else None
    return trace


def _call_label(node: dict) -> str:
    """Human-readable label for a call trace node."""
    name = node.get("function_name") or (node.get("input") or "")[:10] or "fallback"
    target = node.get("contract_name") or node.get("to") or ""
    return f"{target}.{name}" if target else name


def flatten_call_trace(node: dict | None, depth: int = 0) -> list[dict]:
    """Flatten a nested call trace into a depth-annotated list."""
    if not node:
        return []
    flat = [
        {
            "depth": depth,
            "type": node.get("call_type") or node.get("type", "CALL"),
            "label": _call_label(node),
            "from": node.get("from", ""),
            "to": node.get("to", ""),
            "gas_used": node.get("gas_used", 0),
            "error": node.get("error") or node.get("error_reason") or "",
        }
    ]
    for child in node.get("calls") or []:
        flat.extend(flatten_call_trace(child, depth + 1))
    return flat


def trace_skeleton(node: dict | None, max_depth: int = 3) -> list[dict]:
    """Compressed call tree: only nodes up to max_depth, with child counts."""
    skeleton = []
    for entry_node, depth in _walk(node):
        if depth > max_depth:
            continue
        children = entry_node.get("calls") or []
        skeleton.append(
            {
                "depth": depth,
                "label": _call_label(entry_node),
                "gas_used": entry_node.get("gas_used", 0),
                "error": entry_node.get("error") or "",
                "children": len(children),
                "truncated": depth == max_depth and len(children) > 0,
            }
        )
    return skeleton


def find_failures(node: dict | None) -> list[dict]:
    """Find all errored calls anywhere in a trace."""
    return [
        {
            "depth": depth,
            "label": _call_label(entry_node),
            "from": entry_node.get("from", ""),
            "to": entry_node.get("to", ""),
            "error": entry_node.get("error") or entry_node.get("error_reason") or "",
        }
        for entry_node, depth in _walk(node)
        if entry_node.get("error") or entry_node.get("error_reason")
    ]


def error_path(node: dict | None) -> list[dict]:
    """Blame chain from the root call to the deepest errored call."""
    if not node or not (node.get("error") or node.get("error_reason")):
        return []
    path = [
        {
            "depth": 0,
            "label": _call_label(node),
            "error": node.get("error") or node.get("error_reason") or "",
        }
    ]
    current = node
    depth = 0
    while True:
        errored_children = [
            child
            for child in current.get("calls") or []
            if child.get("error") or child.get("error_reason")
        ]
        if not errored_children:
            return path
        current = errored_children[-1]
        depth += 1
        path.append(
            {
                "depth": depth,
                "label": _call_label(current),
                "error": current.get("error") or current.get("error_reason") or "",
            }
        )


def _walk(node: dict | None, depth: int = 0):
    """Yield (node, depth) pairs over a nested call trace."""
    if not node:
        return
    yield node, depth
    for child in node.get("calls") or []:
        yield from _walk(child, depth + 1)


def _client() -> TenderlyClient:
    access_key = secret("TENDERLY_ACCESS_KEY", "")
    if not access_key:
        raise RuntimeError("TENDERLY_ACCESS_KEY not set.")
    return TenderlyClient(access_key=access_key)
