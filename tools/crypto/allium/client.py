"""Allium API client."""

from __future__ import annotations

import json
import re
import time
import uuid
from typing import Any

import httpx

from centaur_sdk import secret


def _parse_sse_events(response_text: str) -> list[dict]:
    """Parse SSE response text into JSON events.

    Handles multi-line data: blocks per SSE spec.
    """
    events: list[dict] = []
    buf: list[str] = []

    for raw in response_text.splitlines():
        line = raw.rstrip("\r")

        if line.startswith("data:"):
            buf.append(line[len("data:") :].lstrip())
            continue

        if line == "" and buf:
            payload = "\n".join(buf)
            buf = []
            try:
                events.append(json.loads(payload))
            except json.JSONDecodeError:
                pass

    if buf:
        payload = "\n".join(buf)
        try:
            events.append(json.loads(payload))
        except json.JSONDecodeError:
            pass

    return events


def _extract_result_list(result: Any, keys: tuple[str, ...] = ("data", "rows", "results")) -> list:
    """Extract a list from result, checking multiple possible keys (1 level of nesting)."""
    if isinstance(result, list):
        return result
    if isinstance(result, dict):
        for key in keys:
            val = result.get(key)
            if isinstance(val, list):
                return val
            if isinstance(val, dict):
                for key2 in keys:
                    val2 = val.get(key2)
                    if isinstance(val2, list):
                        return val2
    return []


class AlliumClient:
    """Client for the Allium on-chain analytics API."""

    def __init__(self, api_key: str | None = None):
        self.api_key = api_key
        self.base_url = "https://api.allium.so"
        self.mcp_url = "https://mcp.allium.so"
        self._http_client: httpx.Client | None = None
        self._mcp_client: httpx.Client | None = None

    def _get_api_key(self) -> str:
        """Get API key from env var."""
        if self.api_key:
            return self.api_key
        key = secret("ALLIUM_API_KEY", "")
        if key:
            return key
        raise RuntimeError("ALLIUM_API_KEY not set.")

    @property
    def http_client(self) -> httpx.Client:
        """Get or create HTTP client with authentication."""
        if self._http_client is None:
            self._http_client = httpx.Client(
                base_url=self.base_url,
                headers={"X-API-KEY": self._get_api_key()},
                timeout=60.0,
            )
        return self._http_client

    @property
    def mcp_client(self) -> httpx.Client:
        """Get or create MCP client for direct SQL execution."""
        if self._mcp_client is None:
            self._mcp_client = httpx.Client(
                base_url=self.mcp_url,
                headers={
                    "X-API-KEY": self._get_api_key(),
                    "Content-Type": "application/json",
                    "Accept": "application/json, text/event-stream",
                },
                timeout=120.0,
            )
        return self._mcp_client

    def close(self) -> None:
        """Close the HTTP clients."""
        if self._http_client is not None:
            self._http_client.close()
            self._http_client = None
        if self._mcp_client is not None:
            self._mcp_client.close()
            self._mcp_client = None

    def __enter__(self) -> "AlliumClient":
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()

    def raw_request(self, method: str, endpoint: str, json_data: dict | None = None) -> dict:
        """Make a raw API request.

        Args:
            method: HTTP method (GET, POST, etc.)
            endpoint: API endpoint path (e.g., /api/v1/explorer/queries)
            json_data: Optional JSON body for POST requests

        Returns:
            Response JSON as dict
        """
        response = self.http_client.request(method, endpoint, json=json_data)
        response.raise_for_status()
        return response.json()

    def _mcp_call(self, tool_name: str, arguments: dict) -> Any:
        """Make an MCP tool call.

        Args:
            tool_name: Name of the MCP tool (e.g., run_sql_query)
            arguments: Arguments to pass to the tool

        Returns:
            Response data from the tool

        Raises:
            RuntimeError: On JSON-RPC errors or tool-level errors (isError),
                e.g. "Unknown tool" or "Not authenticated".
        """
        payload = {
            "jsonrpc": "2.0",
            "id": str(uuid.uuid4()),
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            },
        }
        response = self.mcp_client.post("/", json=payload)
        response.raise_for_status()

        response_text = response.text
        content_type = response.headers.get("content-type", "").lower()

        if "text/event-stream" in content_type or response_text.lstrip().startswith("data:"):
            events = _parse_sse_events(response_text)
            result: dict = {}
            for event in reversed(events):
                if "result" in event or "error" in event:
                    result = event
                    break
            if not result and events:
                result = events[-1]
        elif response_text.strip().startswith("{"):
            try:
                result = response.json()
            except (json.JSONDecodeError, ValueError):
                result = {"text": response_text}
        else:
            result = {}

        if "error" in result:
            raise RuntimeError(f"MCP error: {result['error']}")

        inner = result.get("result", {})

        # Tool-level errors (e.g. "Unknown tool", "Not authenticated") come back
        # as isError + text content, not JSON-RPC errors. Surface them instead of
        # letting them collapse into empty result lists ("No results").
        if isinstance(inner, dict) and inner.get("isError"):
            texts = []
            content = inner.get("content")
            if isinstance(content, list):
                texts = [c.get("text", "") for c in content if isinstance(c, dict)]
            elif isinstance(content, dict):
                texts = [content.get("text", "")]
            message = "; ".join(t for t in texts if t) or json.dumps(inner)
            raise RuntimeError(f"MCP tool '{tool_name}' error: {message}")

        structured = inner.get("structuredContent")
        if structured is not None:
            return structured

        content = result.get("result", {}).get("content", {})
        if isinstance(content, dict) and "text" in content:
            text = content["text"]
            try:
                return json.loads(text)
            except json.JSONDecodeError:
                return {"text": text}
        elif isinstance(content, list) and len(content) > 0:
            text = content[0].get("text", "")
            try:
                return json.loads(text)
            except json.JSONDecodeError:
                return {"text": text}
        return content

    def run_sql(self, sql: str, row_limit: int = 10000, timeout: int = 300) -> list[dict]:
        """Execute arbitrary SQL directly against Allium.

        Uses the MCP endpoint for direct SQL execution. The MCP server runs SQL
        asynchronously: `run_sql_query` returns a run_id immediately and
        `get_query_run_results` polls for completion.

        Args:
            sql: SQL query to execute
            row_limit: Maximum rows to return (default 10000, max 250000)
            timeout: Maximum seconds to wait for the query to complete

        Returns:
            List of result rows as dicts

        Raises:
            RuntimeError: If the query fails
            TimeoutError: If the query doesn't complete within timeout
        """
        started = self._mcp_call("run_sql_query", {"sql": sql})
        run_id = self._extract_run_id(started)

        deadline = time.time() + timeout
        while True:
            # get_query_run_results blocks server-side up to 180s per call.
            poll_seconds = max(1, min(180, int(deadline - time.time())))
            result = self._mcp_call(
                "get_query_run_results",
                {
                    "run_id": run_id,
                    "poll_timeout_seconds": poll_seconds,
                    "row_limit": row_limit,
                },
            )
            status = ""
            if isinstance(result, dict):
                status = str(result.get("status", "")).lower()
            if status in ("failed", "error", "canceled", "cancelled"):
                error = result.get("error") or result.get("message") or json.dumps(result)
                raise RuntimeError(f"SQL query failed: {error}")
            if status and status not in ("success", "succeeded", "completed", "complete"):
                if time.time() >= deadline:
                    raise TimeoutError(f"SQL query run {run_id} timed out after {timeout}s")
                continue

            rows = _extract_result_list(result, ("data", "rows", "results"))
            if rows and isinstance(rows[0], list):
                columns = result.get("columns") if isinstance(result, dict) else None
                if isinstance(columns, list):
                    names = [
                        c.get("name", str(i)) if isinstance(c, dict) else str(c)
                        for i, c in enumerate(columns)
                    ]
                    rows = [dict(zip(names, row, strict=False)) for row in rows]
            return rows[:row_limit]

    @staticmethod
    def _extract_run_id(result: Any) -> str:
        """Extract a run_id from a run_sql_query response."""
        if isinstance(result, dict):
            for key in ("run_id", "query_run_id", "id"):
                value = result.get(key)
                if isinstance(value, str) and value:
                    return value
            text = result.get("text")
            if isinstance(text, str):
                match = re.search(r"run[_ ]?id[\"'\s:=]+([\w-]+)", text, re.IGNORECASE)
                if match:
                    return match.group(1)
        raise RuntimeError(f"Could not extract run_id from run_sql_query response: {result!r}")

    def search_schemas(self, query: str) -> list[str]:
        """Search Allium schemas using semantic search.

        Args:
            query: Search query (e.g., "erc20 token transfers")

        Returns:
            List of matching table IDs
        """
        result = self._mcp_call("search_schemas", {"query": query})

        items = _extract_result_list(result, ("hits", "tables", "results", "data", "matches"))
        if items:
            out: list[str] = []
            for r in items:
                if isinstance(r, dict):
                    out.append(r.get("id") or r.get("table") or r.get("name") or str(r))
                else:
                    out.append(str(r))
            return out

        if isinstance(result, dict):
            text = result.get("text")
            if isinstance(text, str):
                return [line.strip(" -`") for line in text.splitlines() if line.strip()]

        return []

    def fetch_schema(self, table_id: str) -> dict:
        """Fetch schema metadata for a table.

        The dedicated explorer_fetch_schema MCP tool was removed; search_schemas
        with an `id` argument returns the single matching entry with its full
        markdown content populated.

        Args:
            table_id: Full table name (e.g., "ethereum.raw.token_transfers")

        Returns:
            Schema metadata dict (includes markdown `content` when available)
        """
        result = self._mcp_call("search_schemas", {"id": table_id})
        hits = _extract_result_list(result, ("hits", "results", "data", "matches"))
        if hits and isinstance(hits[0], dict):
            return hits[0]
        if isinstance(result, dict):
            return result
        return {"id": table_id, "content": str(result)}

    def run_query(
        self,
        query_id: str,
        parameters: dict | None = None,
        row_limit: int = 10000,
    ) -> str:
        """Start an async query run.

        Args:
            query_id: ID of the saved query in Allium Explorer
            parameters: Optional parameters to pass to the query
            row_limit: Maximum rows to return (default 10000)

        Returns:
            run_id: The ID of the query run to poll for results
        """
        payload: dict[str, Any] = {"row_limit": row_limit}
        if parameters:
            payload["parameters"] = parameters

        response = self.raw_request(
            "POST",
            f"/api/v1/explorer/queries/{query_id}/run-async",
            json_data=payload,
        )
        return response["run_id"]

    def get_query_status(self, run_id: str) -> dict:
        """Check the status of a query run.

        Args:
            run_id: The ID returned from run_query

        Returns:
            Status dict with 'status' key (pending, running, success, failed)
        """
        return self.raw_request("GET", f"/api/v1/explorer/query-runs/{run_id}/status")

    def get_query_results(self, run_id: str, format: str = "json") -> list[dict]:
        """Get results of a completed query run.

        Args:
            run_id: The ID returned from run_query
            format: Output format (json, csv)

        Returns:
            List of result rows as dicts
        """
        response = self.raw_request(
            "GET", f"/api/v1/explorer/query-runs/{run_id}/results?f={format}"
        )
        return response.get("data", [])

    def execute_query(
        self,
        query_id: str,
        parameters: dict | None = None,
        timeout: int = 300,
        poll_interval: float = 2.0,
    ) -> list[dict]:
        """Execute a query and wait for results (blocking).

        Args:
            query_id: ID of the saved query in Allium Explorer
            parameters: Optional parameters to pass to the query
            timeout: Maximum seconds to wait for results
            poll_interval: Seconds between status checks

        Returns:
            List of result rows as dicts

        Raises:
            TimeoutError: If query doesn't complete within timeout
            RuntimeError: If query fails
        """
        run_id = self.run_query(query_id, parameters)
        start_time = time.time()

        while True:
            if time.time() - start_time > timeout:
                raise TimeoutError(f"Query {query_id} timed out after {timeout}s")

            status = self.get_query_status(run_id)
            state = status.get("status", "unknown")

            if state == "success":
                return self.get_query_results(run_id)
            elif state == "failed":
                error = status.get("error", "Unknown error")
                raise RuntimeError(f"Query failed: {error}")
            elif state in ("pending", "running"):
                time.sleep(poll_interval)
            else:
                raise RuntimeError(f"Unknown query status: {state}")


# Pre-built SQL queries for stablecoin analysis
# These need to be saved in Allium Explorer and referenced by query_id

STABLECOIN_QUERIES = {
    "volume_by_chain": """
-- Stablecoin transfer volume by chain (last N days)
-- Parameters: days (int)
SELECT
    chain,
    stablecoin_symbol,
    SUM(amount_usd) as total_volume_usd,
    COUNT(*) as transfer_count,
    COUNT(DISTINCT from_address) as unique_senders,
    COUNT(DISTINCT to_address) as unique_receivers
FROM crosschain.stablecoin.transfers
WHERE block_timestamp >= CURRENT_DATE - INTERVAL '{{days}} days'
GROUP BY chain, stablecoin_symbol
ORDER BY total_volume_usd DESC
""",
    "top_contracts": """
-- Top contracts by stablecoin volume on a chain
-- Parameters: chain (string), days (int)
SELECT
    to_address as contract_address,
    COUNT(*) as transfer_count,
    SUM(amount_usd) as total_volume_usd,
    COUNT(DISTINCT from_address) as unique_senders,
    COUNT(DISTINCT stablecoin_symbol) as stablecoins_used
FROM crosschain.stablecoin.transfers
WHERE chain = '{{chain}}'
  AND block_timestamp >= CURRENT_DATE - INTERVAL '{{days}} days'
  AND to_address IS NOT NULL
GROUP BY to_address
ORDER BY total_volume_usd DESC
LIMIT 100
""",
    "stablecoin_flows": """
-- Net stablecoin flows (inflows vs outflows) by address on a chain
-- Parameters: chain (string), days (int)
WITH inflows AS (
    SELECT
        to_address as address,
        SUM(amount_usd) as inflow_usd
    FROM crosschain.stablecoin.transfers
    WHERE chain = '{{chain}}'
      AND block_timestamp >= CURRENT_DATE - INTERVAL '{{days}} days'
    GROUP BY to_address
),
outflows AS (
    SELECT
        from_address as address,
        SUM(amount_usd) as outflow_usd
    FROM crosschain.stablecoin.transfers
    WHERE chain = '{{chain}}'
      AND block_timestamp >= CURRENT_DATE - INTERVAL '{{days}} days'
    GROUP BY from_address
)
SELECT
    COALESCE(i.address, o.address) as address,
    COALESCE(i.inflow_usd, 0) as inflow_usd,
    COALESCE(o.outflow_usd, 0) as outflow_usd,
    COALESCE(i.inflow_usd, 0) - COALESCE(o.outflow_usd, 0) as net_flow_usd
FROM inflows i
FULL OUTER JOIN outflows o ON i.address = o.address
ORDER BY ABS(COALESCE(i.inflow_usd, 0) - COALESCE(o.outflow_usd, 0)) DESC
LIMIT 100
""",
    "transfers": """
-- Recent stablecoin transfers on a chain
-- Parameters: chain (string), limit (int), stablecoin (optional string)
SELECT
    block_timestamp,
    tx_hash,
    from_address,
    to_address,
    stablecoin_symbol,
    amount,
    amount_usd
FROM crosschain.stablecoin.transfers
WHERE chain = '{{chain}}'
  {% if stablecoin %}AND stablecoin_symbol = '{{stablecoin}}'{% endif %}
ORDER BY block_timestamp DESC
LIMIT {{limit}}
""",
    "daily_metrics": """
-- Daily stablecoin metrics from pre-aggregated table
-- Parameters: chain (optional string), days (int)
SELECT
    date,
    chain,
    stablecoin_symbol,
    total_volume_usd,
    transfer_count,
    unique_addresses
FROM crosschain.metrics.stablecoin_volume
WHERE date >= CURRENT_DATE - INTERVAL '{{days}} days'
  {% if chain %}AND chain = '{{chain}}'{% endif %}
ORDER BY date DESC, total_volume_usd DESC
""",
    "dex_trades": """
-- DEX trades involving stablecoins
-- Parameters: chain (string), days (int)
SELECT
    block_timestamp,
    tx_hash,
    dex_name,
    token_in_symbol,
    token_out_symbol,
    amount_in_usd,
    amount_out_usd,
    trader_address
FROM crosschain.dex.trades
WHERE chain = '{{chain}}'
  AND block_timestamp >= CURRENT_DATE - INTERVAL '{{days}} days'
  AND (
    token_in_symbol IN ('USDC', 'USDT', 'DAI', 'BUSD', 'FRAX', 'TUSD')
    OR token_out_symbol IN ('USDC', 'USDT', 'DAI', 'BUSD', 'FRAX', 'TUSD')
  )
ORDER BY block_timestamp DESC
LIMIT 1000
""",
    "cex_identification": """
-- Identify potential CEX wallets by transfer patterns
-- High volume, many unique counterparties, regular intervals
-- Parameters: chain (string), days (int)
SELECT
    address,
    total_volume_usd,
    unique_counterparties,
    transfer_count,
    avg_transfer_usd,
    CASE
        WHEN unique_counterparties > 1000 AND total_volume_usd > 10000000 THEN 'likely_cex'
        WHEN unique_counterparties > 100 AND total_volume_usd > 1000000 THEN 'possible_cex'
        ELSE 'unknown'
    END as classification
FROM (
    SELECT
        from_address as address,
        SUM(amount_usd) as total_volume_usd,
        COUNT(DISTINCT to_address) as unique_counterparties,
        COUNT(*) as transfer_count,
        AVG(amount_usd) as avg_transfer_usd
    FROM crosschain.stablecoin.transfers
    WHERE chain = '{{chain}}'
      AND block_timestamp >= CURRENT_DATE - INTERVAL '{{days}} days'
    GROUP BY from_address

    UNION ALL

    SELECT
        to_address as address,
        SUM(amount_usd) as total_volume_usd,
        COUNT(DISTINCT from_address) as unique_counterparties,
        COUNT(*) as transfer_count,
        AVG(amount_usd) as avg_transfer_usd
    FROM crosschain.stablecoin.transfers
    WHERE chain = '{{chain}}'
      AND block_timestamp >= CURRENT_DATE - INTERVAL '{{days}} days'
    GROUP BY to_address
) combined
GROUP BY address
HAVING SUM(total_volume_usd) > 1000000
ORDER BY total_volume_usd DESC
LIMIT 100
""",
}


def get_example_queries() -> dict[str, str]:
    """Get example SQL queries for stablecoin analysis.

    Returns:
        Dict mapping query name to SQL string
    """
    return STABLECOIN_QUERIES.copy()


def _client() -> AlliumClient:
    api_key = secret("ALLIUM_API_KEY", "")
    if not api_key:
        raise RuntimeError("ALLIUM_API_KEY not set.")
    return AlliumClient(api_key=api_key)
