"""Nansen API client."""


import httpx

from centaur_sdk import secret

SUPPORTED_CHAINS = [
    "all",
    "ethereum",
    "solana",
    "base",
    "bnb",
    "arbitrum",
    "polygon",
    "optimism",
    "avalanche",
    "linea",
    "scroll",
    "zksync",
    "mantle",
    "ronin",
    "sei",
    "plasma",
    "sonic",
    "unichain",
    "monad",
    "hyperevm",
    "iotaevm",
    "bitcoin",
    "ton",
    "tron",
    "near",
    "starknet",
    "sui",
    "aptos",
    "algorand",
    "stacks",
    "stellar",
    "hyperliquid",
]


class NansenClient:
    """Client for Nansen API."""

    def __init__(self, api_key: str | None = None, timeout: float = 60.0):
        self._api_key = api_key
        self.base_url = "https://api.nansen.ai"
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
        return secret("NANSEN_API_KEY", "")

    def _request(
        self,
        endpoint: str,
        data: dict | None = None,
        method: str = "POST",
    ) -> dict | list:
        """Make an API request."""
        api_key = self._get_api_key()
        if not api_key:
            raise RuntimeError("NANSEN_API_KEY not set.")

        url = f"{self.base_url}{endpoint}"
        headers = {
            "Content-Type": "application/json",
            "apiKey": api_key,
        }

        try:
            if method == "POST":
                response = self.client.post(url, json=data, headers=headers)
            else:
                response = self.client.get(url, params=data, headers=headers)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise RuntimeError(f"API error: {e.response.status_code} - {e.response.text}")
        except httpx.RequestError as e:
            raise RuntimeError(f"Request failed: {e}")

    def get_address_labels(
        self,
        address: str,
        chain: str = "ethereum",
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get labels for a wallet address."""
        data = {
            "parameters": {
                "chain": chain,
                "address": address,
            },
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        return self._request("/api/beta/profiler/address/labels", data=data)

    def get_address_balance(
        self,
        address: str | None = None,
        entity_name: str | None = None,
        chain: str = "ethereum",
        hide_spam: bool = True,
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get current token balances for an address or entity."""
        if not address and not entity_name:
            raise ValueError("Either address or entity_name must be provided")

        data: dict = {
            "chain": chain,
            "hide_spam_token": hide_spam,
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        if address:
            data["address"] = address
        if entity_name:
            data["entity_name"] = entity_name

        return self._request("/api/v1/profiler/address/current-balance", data=data)

    def get_address_transactions(
        self,
        address: str,
        chain: str = "ethereum",
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get transactions for an address."""
        data = {
            "address": address,
            "chain": chain,
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        return self._request("/api/v1/profiler/address/transactions", data=data)

    def get_address_pnl(
        self,
        address: str,
        chain: str = "ethereum",
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get PnL and trade performance for an address."""
        data = {
            "address": address,
            "chain": chain,
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        return self._request("/api/v1/profiler/address/pnl", data=data)

    def get_address_related_wallets(
        self,
        address: str,
        chain: str = "ethereum",
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get related wallets for an address."""
        data = {
            "address": address,
            "chain": chain,
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        return self._request("/api/v1/profiler/address/related-wallets", data=data)

    def get_smart_money_holdings(
        self,
        chains: list[str] | None = None,
        labels: list[str] | None = None,
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get Smart Money token holdings."""
        data: dict = {
            "chains": chains or ["ethereum"],
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        if labels:
            data["labels"] = labels

        return self._request("/api/v1/smart-money/holdings", data=data)

    def get_smart_money_netflows(
        self,
        chains: list[str] | None = None,
        labels: list[str] | None = None,
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get Smart Money net flows."""
        data: dict = {
            "chains": chains or ["ethereum"],
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        if labels:
            data["labels"] = labels

        return self._request("/api/v1/smart-money/netflows", data=data)

    def get_smart_money_dex_trades(
        self,
        chains: list[str] | None = None,
        labels: list[str] | None = None,
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get Smart Money DEX trades in last 24h."""
        data: dict = {
            "chains": chains or ["ethereum"],
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        if labels:
            data["labels"] = labels

        return self._request("/api/v1/smart-money/dex-trades", data=data)

    def get_token_screener(
        self,
        chain: str = "ethereum",
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get token screening data."""
        data = {
            "chain": chain,
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        return self._request("/api/v1/tgm/token-screener", data=data)

    def get_token_holders(
        self,
        token_address: str,
        chain: str = "ethereum",
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get top holders for a token."""
        data = {
            "token_address": token_address,
            "chain": chain,
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        return self._request("/api/v1/tgm/holders", data=data)

    def get_token_flows(
        self,
        token_address: str,
        chain: str = "ethereum",
    ) -> dict:
        """Get token inflows/outflows by entity type."""
        data = {
            "token_address": token_address,
            "chain": chain,
        }
        return self._request("/api/v1/tgm/flows", data=data)

    def get_token_who_bought_sold(
        self,
        token_address: str,
        chain: str = "ethereum",
    ) -> dict:
        """Get recent buyers and sellers of a token."""
        data = {
            "token_address": token_address,
            "chain": chain,
        }
        return self._request("/api/v1/tgm/who-bought-sold", data=data)

    def get_token_dex_trades(
        self,
        token_address: str,
        chain: str = "ethereum",
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get DEX trades for a token."""
        data = {
            "token_address": token_address,
            "chain": chain,
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        return self._request("/api/v1/tgm/dex-trades", data=data)

    def get_token_pnl_leaderboard(
        self,
        token_address: str,
        chain: str = "ethereum",
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Get PnL leaderboard for a token."""
        data = {
            "token_address": token_address,
            "chain": chain,
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        return self._request("/api/v1/tgm/pnl-leaderboard", data=data)

    def search_entity(
        self,
        query: str,
        page: int = 1,
        per_page: int = 100,
    ) -> dict:
        """Search for an entity by name."""
        data = {
            "query": query,
            "pagination": {
                "page": page,
                "recordsPerPage": per_page,
            },
        }
        return self._request("/api/beta/profiler/entity/search", data=data)

    def close(self):
        """Close the HTTP client."""
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()



def _client() -> NansenClient:
    return NansenClient()
