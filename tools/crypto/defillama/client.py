"""DefiLlama API client."""


import httpx

from centaur_sdk import secret


_PERP_VENUE_HINTS = frozenset({
    "dydx",
    "dydx-v4",
    "dydx-chain",
    "dydx-v3",
    "hyperliquid",
    "hyperliquid-perps",
    "gmx",
    "gmx-v1",
    "gmx-v2",
    "aster",
    "lighter",
    "vertex",
    "vertex-perps",
    "synfutures",
    "kwenta",
    "drift",
    "drift-v2",
    "paradex",
    "mango-markets",
    "apex",
    "apex-pro",
    "level-finance",
})


def _looks_like_perps(slug: str) -> bool:
    """Heuristic: is this slug a known perps / derivatives venue?"""
    return (slug or "").lower().strip() in _PERP_VENUE_HINTS


class DefiLlamaClient:
    """Client for DefiLlama API.

    Supports both free and pro API endpoints. Pro endpoints require an API key.
    """

    def __init__(self, api_key: str | None = None, timeout: float = 30.0):
        """Initialize the DefiLlama client.

        Args:
            api_key: Optional API key for pro endpoints
            timeout: Request timeout in seconds
        """
        self._api_key = api_key
        self.base_url = "https://api.llama.fi"
        self.stablecoins_url = "https://stablecoins.llama.fi"
        self.bridges_url = "https://bridges.llama.fi"
        self.pro_url = "https://pro-api.llama.fi"
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    def _get_api_key(self) -> str | None:
        """Get API key from instance or env var."""
        if self._api_key:
            return self._api_key
        return secret("DEFILLAMA_API_KEY", "")

    def _request(
        self,
        endpoint: str,
        params: dict | None = None,
        pro: bool = False,
        base: str | None = None,
    ) -> dict | list:
        """Make an API request.

        Args:
            endpoint: API endpoint path (e.g., "/stablecoins")
            params: Optional query parameters
            pro: Whether this is a pro endpoint requiring API key
            base: Override base URL (e.g., self.stablecoins_url)

        Returns:
            JSON response data

        Raises:
            RuntimeError: If the request fails
        """
        if pro:
            api_key = self._get_api_key()
            if not api_key:
                raise RuntimeError("DEFILLAMA_API_KEY not set (required for pro endpoints).")
            if base:
                url = f"{base}{endpoint}"
                if params is None:
                    params = {}
                params["apikey"] = api_key
            else:
                url = f"{self.pro_url}/{api_key}{endpoint}"
        elif base:
            url = f"{base}{endpoint}"
        else:
            url = f"{self.base_url}{endpoint}"

        try:
            response = self.client.get(url, params=params)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise RuntimeError(f"API error: {e.response.status_code} - {e.response.text}")
        except httpx.RequestError as e:
            raise RuntimeError(f"Request failed: {e}")

    def raw_request(
        self, endpoint: str, params: dict | None = None, pro: bool = False
    ) -> dict | list:
        """Make a raw API call to any endpoint.

        Args:
            endpoint: Full API endpoint path (e.g., "/stablecoins")
            params: Optional query parameters
            pro: Whether to use pro API base URL

        Returns:
            JSON response data
        """
        return self._request(endpoint, params, pro=pro)

    # === Stablecoins ===

    def list_stablecoins(self) -> list[dict]:
        """List all stablecoins with their market caps.

        Returns:
            List of stablecoin data including id, name, symbol, circulating amounts
        """
        data = self._request("/stablecoins", base=self.stablecoins_url)
        return data.get("peggedAssets", []) if isinstance(data, dict) else data

    def get_stablecoin(self, asset_id: str) -> dict:
        """Get details for a specific stablecoin including chain breakdown.

        Args:
            asset_id: The stablecoin ID (e.g., "1" for USDT)

        Returns:
            Stablecoin details with chain-by-chain breakdown
        """
        return self._request(f"/stablecoin/{asset_id}", base=self.stablecoins_url)

    def get_stablecoin_charts(self, chain: str | None = None) -> list[dict]:
        """Get historical stablecoin market cap data.

        Args:
            chain: Optional chain name to filter (e.g., "ethereum", "arbitrum")

        Returns:
            List of historical data points with dates and market caps
        """
        if chain:
            return self._request(f"/stablecoincharts/{chain}", base=self.stablecoins_url)
        return self._request("/stablecoincharts/all", base=self.stablecoins_url)

    def get_stablecoin_chains(self) -> list[dict]:
        """Get stablecoin market cap by chain.

        Returns:
            List of chains with their total stablecoin market caps
        """
        return self._request("/stablecoinchains", base=self.stablecoins_url)

    def get_stablecoin_prices(self) -> dict:
        """Get historical stablecoin prices.

        Returns:
            Historical price data for stablecoins
        """
        return self._request("/stablecoinprices", base=self.stablecoins_url)

    # === TVL & Protocols ===

    def list_protocols(self) -> list[dict]:
        """List all DeFi protocols with their TVL.

        Returns:
            List of protocols with TVL, category, chain info
        """
        return self._request("/protocols")

    def get_protocol(self, slug: str) -> dict:
        """Get detailed protocol information including historical TVL.

        Args:
            slug: Protocol slug (e.g., "aave", "uniswap")

        Returns:
            Protocol details with historical TVL data
        """
        return self._request(f"/protocol/{slug}")

    def get_tvl(self, protocol: str) -> float:
        """Get current TVL for a protocol.

        Args:
            protocol: Protocol slug

        Returns:
            Current TVL value
        """
        return self._request(f"/tvl/{protocol}")

    def list_chains(self) -> list[dict]:
        """List all chains with their TVL.

        Returns:
            List of chains with TVL data
        """
        return self._request("/v2/chains")

    def get_chain_tvl(self, chain: str | None = None) -> list[dict]:
        """Get historical TVL data for chains.

        Args:
            chain: Optional chain name to filter

        Returns:
            Historical TVL data points
        """
        if chain:
            return self._request(f"/v2/historicalChainTvl/{chain}")
        return self._request("/v2/historicalChainTvl")

    def get_protocol_inflows(self, protocol: str, timestamp: int) -> dict:
        """Get protocol inflows/outflows (Pro endpoint).

        Args:
            protocol: Protocol slug
            timestamp: Unix timestamp

        Returns:
            Inflows/outflows data
        """
        return self._request(f"/api/inflows/{protocol}/{timestamp}", pro=True)

    # === DEX Volumes ===

    def get_dex_volumes(self, chain: str | None = None) -> dict:
        """Get DEX trading volumes.

        Args:
            chain: Optional chain name to filter

        Returns:
            DEX volume data
        """
        if chain:
            return self._request(f"/overview/dexs/{chain}")
        return self._request("/overview/dexs")

    def get_dex_summary(self, protocol: str) -> dict:
        """Get volume details for a specific DEX protocol.

        Note: perpetuals venues (dYdX, Hyperliquid, GMX, Aster, Lighter, etc.) are
        NOT listed under `/dexs`; use `get_derivatives_summary` instead. If DefiLlama
        returns a 400 "not found", the error is surfaced with a suggestion.

        Args:
            protocol: Protocol slug

        Returns:
            Protocol volume details
        """
        try:
            return self._request(f"/summary/dexs/{protocol}")
        except RuntimeError as exc:
            msg = str(exc)
            if "not found" in msg.lower() and _looks_like_perps(protocol):
                raise RuntimeError(
                    f"{msg}\n"
                    f"Hint: `{protocol}` looks like a perpetuals venue — try "
                    f"`get_derivatives_summary(protocol=...)` instead. DefiLlama "
                    f"splits spot DEX volumes (`/dexs`) from perp volumes (`/derivatives`)."
                ) from exc
            raise

    # === Derivatives / Perpetuals ===

    def get_derivatives_volumes(self, chain: str | None = None) -> dict:
        """Get perpetual-futures venue volumes across chains.

        Covers dYdX, Hyperliquid, GMX, Aster, Lighter, and other perps venues
        that are NOT listed under `/dexs`.

        Args:
            chain: Optional chain name to filter

        Returns:
            Derivatives volume data
        """
        if chain:
            return self._request(f"/overview/derivatives/{chain}")
        return self._request("/overview/derivatives")

    def get_derivatives_summary(self, protocol: str) -> dict:
        """Get volume details for a specific perpetuals venue.

        Args:
            protocol: Protocol slug (e.g., "dydx-v4", "hyperliquid", "gmx-v2")

        Returns:
            Protocol derivatives volume details
        """
        return self._request(f"/summary/derivatives/{protocol}")

    def get_open_interest_overview(self, chain: str | None = None) -> dict:
        """Get perpetual-futures open interest overview.

        Args:
            chain: Optional chain name to filter

        Returns:
            Open interest overview data
        """
        if chain:
            return self._request(f"/overview/open-interest/{chain}")
        return self._request("/overview/open-interest")

    def get_open_interest_summary(self, protocol: str) -> dict:
        """Get open interest details for a specific perpetuals venue.

        Args:
            protocol: Protocol slug (e.g., "hyperliquid", "lighter", "dydx-v4")

        Returns:
            Protocol open interest details
        """
        return self._request(f"/summary/open-interest/{protocol}")

    # === Bridges ===

    def list_bridges(self) -> list[dict]:
        """List all bridges.

        Returns:
            List of bridge data
        """
        data = self._request("/bridges", base=self.bridges_url)
        return data.get("bridges", []) if isinstance(data, dict) else data

    def get_bridge(self, bridge_id: str) -> dict:
        """Get details for a specific bridge.

        Args:
            bridge_id: Bridge ID

        Returns:
            Bridge details
        """
        return self._request(f"/bridge/{bridge_id}", base=self.bridges_url)

    def get_bridge_volumes(self, chain: str | None = None) -> list[dict]:
        """Get bridge volumes.

        Args:
            chain: Optional chain name to filter

        Returns:
            Bridge volume data
        """
        if chain:
            return self._request(f"/bridgevolume/{chain}", base=self.bridges_url)
        return self._request("/bridges", base=self.bridges_url)

    def get_bridge_day_stats(self, timestamp: int, chain: str) -> dict:
        """Get daily bridge statistics.

        Args:
            timestamp: Unix timestamp for the day
            chain: Chain name

        Returns:
            Daily bridge stats
        """
        return self._request(f"/bridgedaystats/{timestamp}/{chain}", base=self.bridges_url)

    def get_bridge_transactions(self, bridge_id: str) -> dict:
        """Get bridge transactions (Pro endpoint).

        Args:
            bridge_id: Bridge ID

        Returns:
            Transaction data
        """
        return self._request(f"/transactions/{bridge_id}", base=self.bridges_url, pro=True)

    # === Fees & Revenue ===

    def get_fees(self, chain: str | None = None) -> dict:
        """Get protocol fees overview.

        Args:
            chain: Optional chain name to filter

        Returns:
            Fees data
        """
        if chain:
            return self._request(f"/overview/fees/{chain}")
        return self._request("/overview/fees")

    def get_protocol_fees(self, protocol: str) -> dict:
        """Get fees for a specific protocol.

        Args:
            protocol: Protocol slug

        Returns:
            Protocol fees data
        """
        return self._request(f"/summary/fees/{protocol}")

    def close(self):
        """Close the HTTP client."""
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def _client() -> DefiLlamaClient:
    api_key = secret("DEFILLAMA_API_KEY", "")
    return DefiLlamaClient(api_key=api_key)
