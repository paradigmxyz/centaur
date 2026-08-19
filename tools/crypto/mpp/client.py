"""MPP (Machine Payments Protocol) client for Centaur.

Wraps Tempo-paid data services: Parallel (web search), CoinGecko (prices/charts),
Codex (DEX data), Allium (wallets), and Dune (SQL). Pays per-query with Tempo stablecoins.

Ported from gtmskill's mpp-client.ts, mpp-search.ts, and mpp-onchain.ts.
"""

from __future__ import annotations

import time
from datetime import UTC, datetime
from typing import Any

import httpx

from centaur_sdk.tool_sdk import secret

MPP_SERVICE_CATALOG_URL = "https://mpp.dev/api/services"
MPP_SERVICE_CATALOG_TIMEOUT_SECONDS = 15
DEFAULT_SERVICE_LIMIT = 20
MAX_SERVICE_LIMIT = 100


class MppCatalogError(RuntimeError):
    """Raised when the public MPP service catalog cannot be used safely."""

# --- Token name normalization ---

TOKEN_NAME_MAP: dict[str, str] = {
    "SOLANA": "SOL", "BITCOIN": "BTC", "ETHEREUM": "ETH", "CARDANO": "ADA",
    "POLKADOT": "DOT", "AVALANCHE": "AVAX", "CHAINLINK": "LINK", "POLYGON": "MATIC",
    "LITECOIN": "LTC", "DOGECOIN": "DOGE", "RIPPLE": "XRP", "TONCOIN": "TON",
    "UNISWAP": "UNI", "AAVE": "AAVE", "CELESTIA": "TIA", "ARBITRUM": "ARB",
    "OPTIMISM": "OP", "APTOS": "APT", "SUI": "SUI", "SEI": "SEI",
    "HYPERLIQUID": "HYPE", "JUPITER": "JUP", "JITO": "JTO", "RAYDIUM": "RAY",
    "PENDLE": "PENDLE", "EIGENLAYER": "EIGEN", "STARKNET": "STRK",
    "MONAD": "MON", "NOBLE": "NOBLE",
}

COINGECKO_ID_MAP: dict[str, str] = {
    "BTC": "bitcoin", "ETH": "ethereum", "SOL": "solana", "ADA": "cardano",
    "DOT": "polkadot", "AVAX": "avalanche-2", "LINK": "chainlink", "MATIC": "matic-network",
    "LTC": "litecoin", "DOGE": "dogecoin", "XRP": "ripple", "TON": "the-open-network",
    "UNI": "uniswap", "AAVE": "aave", "TIA": "celestia", "ARB": "arbitrum",
    "OP": "optimism", "APT": "aptos", "SUI": "sui", "SEI": "sei-network",
    "HYPE": "hyperliquid", "JUP": "jupiter-exchange-solana", "JTO": "jito-governance-token",
    "RAY": "raydium", "PENDLE": "pendle", "EIGEN": "eigenlayer", "STRK": "starknet",
    "NEAR": "near", "ATOM": "cosmos", "FIL": "filecoin", "INJ": "injective-protocol",
    "TRX": "tron", "PEPE": "pepe", "SHIB": "shiba-inu", "BONK": "bonk",
}


def _normalize_token(name: str) -> str:
    return TOKEN_NAME_MAP.get(name.upper(), name)


def _fmt_price(n: float) -> str:
    if n >= 1e12:
        return f"{n / 1e12:.2f}T"
    if n >= 1e9:
        return f"{n / 1e9:.1f}B"
    if n >= 1e6:
        return f"{n / 1e6:.1f}M"
    if n >= 1000:
        return f"{n:,.2f}"
    if n >= 1:
        return f"{n:.2f}"
    return f"{n:.4f}"


class MppClient:
    """Paid-per-query market data via Tempo MPP."""

    def __init__(self) -> None:
        # Lazy-load the secret — do NOT call secret() at import/init time
        # because the secret may not exist in 1Password yet, which would crash
        # the tool registry and take down other tools.
        self._private_key: str | None = None
        self._daily_spend = 0.0
        self._daily_cap = 10.0
        self._last_reset = datetime.now(UTC).strftime("%Y-%m-%d")
        self._coingecko_id_cache: dict[str, str] = {}
        self._catalog_http: httpx.Client | None = None

    def _fetch_service_catalog(self) -> list[dict[str, Any]]:
        """Fetch and validate the public MPP service catalog without using a wallet."""
        client = self._catalog_http
        owns_client = client is None
        if client is None:
            client = httpx.Client(timeout=MPP_SERVICE_CATALOG_TIMEOUT_SECONDS)

        try:
            try:
                response = client.get(MPP_SERVICE_CATALOG_URL)
                response.raise_for_status()
            except httpx.HTTPStatusError as exc:
                raise MppCatalogError(
                    f"MPP service catalog returned HTTP {exc.response.status_code}"
                ) from exc
            except httpx.HTTPError as exc:
                raise MppCatalogError("could not fetch MPP service catalog") from exc

            try:
                payload = response.json()
            except ValueError as exc:
                raise MppCatalogError("MPP service catalog returned invalid JSON") from exc

            services = payload.get("services") if isinstance(payload, dict) else None
            if not isinstance(services, list) or not all(
                isinstance(service, dict) for service in services
            ):
                raise MppCatalogError("MPP service catalog has an invalid services list")
            return services
        finally:
            if owns_client:
                client.close()

    @staticmethod
    def _validate_service_limit(limit: int) -> None:
        if not 1 <= limit <= MAX_SERVICE_LIMIT:
            raise ValueError(f"limit must be between 1 and {MAX_SERVICE_LIMIT}")

    @staticmethod
    def _matches_service(
        service: dict[str, Any], query: str | None, category: str | None, tag: str | None
    ) -> bool:
        if category is not None:
            categories = service.get("categories") or []
            if not any(str(value).casefold() == category.casefold() for value in categories):
                return False

        if tag is not None:
            tags = service.get("tags") or []
            if not any(str(value).casefold() == tag.casefold() for value in tags):
                return False

        if query is None:
            return True

        haystack = [
            service.get("id"),
            service.get("name"),
            service.get("description"),
            *(service.get("categories") or []),
            *(service.get("tags") or []),
        ]
        normalized_query = query.casefold()
        return any(
            normalized_query in str(value).casefold() for value in haystack if value is not None
        )

    @staticmethod
    def _service_summary(service: dict[str, Any]) -> dict[str, Any]:
        endpoints = service.get("endpoints") or []
        paid_endpoints = service.get("paidEndpoints")
        if not isinstance(paid_endpoints, int):
            paid_endpoints = sum(
                isinstance(endpoint, dict) and endpoint.get("payment") is not None
                for endpoint in endpoints
            )
        return {
            "id": service.get("id", ""),
            "name": service.get("name", ""),
            "description": service.get("description", ""),
            "service_url": service.get("serviceUrl") or service.get("url") or "",
            "categories": service.get("categories") or [],
            "tags": service.get("tags") or [],
            "status": service.get("status", ""),
            "paid_endpoints": paid_endpoints,
        }

    def list_services(
        self,
        query: str | None = None,
        category: str | None = None,
        tag: str | None = None,
        limit: int = DEFAULT_SERVICE_LIMIT,
    ) -> list[dict[str, Any]]:
        """List public MPP services, optionally filtered by catalog metadata."""
        self._validate_service_limit(limit)
        normalized_query = query.strip() if query is not None else None
        normalized_category = category.strip() if category is not None else None
        normalized_tag = tag.strip() if tag is not None else None
        if normalized_query == "":
            normalized_query = None
        if normalized_category == "":
            normalized_category = None
        if normalized_tag == "":
            normalized_tag = None

        return [
            self._service_summary(service)
            for service in self._fetch_service_catalog()
            if self._matches_service(service, normalized_query, normalized_category, normalized_tag)
        ][:limit]

    def search_services(
        self,
        query: str,
        category: str | None = None,
        tag: str | None = None,
        limit: int = DEFAULT_SERVICE_LIMIT,
    ) -> list[dict[str, Any]]:
        """Search public MPP services by text and optional exact catalog filters."""
        return self.list_services(query=query, category=category, tag=tag, limit=limit)

    def get_service(self, service: str) -> dict[str, Any]:
        """Return one complete public MPP service record by id or unambiguous name."""
        identifier = service.strip()
        if not identifier:
            raise ValueError("service id or name is required")

        services = self._fetch_service_catalog()
        exact_ids = [item for item in services if item.get("id") == identifier]
        if exact_ids:
            return exact_ids[0]

        name_matches = [
            item
            for item in services
            if isinstance(item.get("name"), str)
            and item["name"].casefold() == identifier.casefold()
        ]
        if len(name_matches) == 1:
            return name_matches[0]
        if len(name_matches) > 1:
            raise ValueError(f"MPP service name {identifier!r} is ambiguous; use its id")
        raise ValueError(f"MPP service {identifier!r} was not found")

    def _get_private_key(self) -> str:
        """Lazy-load MPP_PRIVATE_KEY from secrets on first use."""
        if self._private_key is None:
            try:
                self._private_key = secret("MPP_PRIVATE_KEY")
            except Exception as e:
                raise RuntimeError(
                    f"MPP_PRIVATE_KEY not configured. Add it to 1Password to enable paid data queries. ({e})"
                ) from e
        return self._private_key

    def _check_budget(self, needed: float = 0) -> bool:
        today = datetime.now(UTC).strftime("%Y-%m-%d")
        if today != self._last_reset:
            self._daily_spend = 0.0
            self._last_reset = today
        return (self._daily_spend + needed) < self._daily_cap

    def _track(self, amount: float) -> None:
        self._daily_spend += amount

    def _tempo_fetch(self, url: str, method: str = "GET", body: dict | None = None, timeout: float = 30) -> Any:
        """Make a paid HTTP request via Tempo MPP with the private key."""
        # Lazy-load key on first actual API call, not on import
        key = self._get_private_key()
        headers = {
            "Content-Type": "application/json",
            "X-MPP-Key": key,
        }
        with httpx.Client(timeout=timeout) as client:
            if method == "POST":
                resp = client.post(url, json=body or {}, headers=headers)
            else:
                resp = client.get(url, headers=headers)
            resp.raise_for_status()
            return resp.json()

    # --- Web Search (Parallel) ---

    def search_web(self, query: str, num_results: int = 5) -> list[dict]:
        """Search the web via Parallel ($0.01/query). Returns title, url, text, date for each result."""
        if not self._check_budget(0.01):
            return []
        try:
            data = self._tempo_fetch(
                "https://parallelmpp.dev/api/search",
                method="POST",
                body={"query": query, "mode": "fast"},
            )
            self._track(0.01)
            results = data.get("results") or data.get("data") or []
            return [
                {
                    "title": r.get("title") or r.get("name") or "",
                    "url": r.get("url") or r.get("link") or "",
                    "text": (r.get("text") or r.get("content") or r.get("snippet") or "")[:2000],
                    "date": r.get("publishedDate") or r.get("published_date"),
                }
                for r in results[:num_results]
            ]
        except Exception as e:
            return [{"error": str(e)}]

    # --- Token Price (CoinGecko primary, Codex fallback) ---

    def get_token_price(self, token_name: str) -> dict:
        """Get current price, 24h change, volume, and market cap for a token.

        Uses CoinGecko ($0.06-0.12) with Codex DEX fallback ($0.02).
        Accepts full names (Solana) or symbols (SOL).
        """
        normalized = _normalize_token(token_name)
        empty = {"found": False, "name": token_name, "symbol": "", "price": 0}

        # Try CoinGecko first
        result = self._coingecko_price(normalized)
        if result.get("found"):
            return result

        # Fallback: Codex (DEX-only data)
        codex = self._codex_token(normalized)
        if codex and codex.get("price", 0) > 0:
            return {
                "found": True,
                "name": token_name,
                "symbol": codex.get("symbol", normalized.upper()),
                "price": codex["price"],
                "volume_24h": codex.get("volume24"),
                "liquidity": codex.get("liquidity"),
                "market_cap": codex.get("marketCap"),
                "source": "codex",
            }

        return empty

    def _coingecko_price(self, token_name: str) -> dict:
        empty = {"found": False, "name": token_name}
        known_id = COINGECKO_ID_MAP.get(token_name.upper())

        if known_id:
            coin_id = known_id
        else:
            if not self._check_budget(0.12):
                return empty
            try:
                search = self._tempo_fetch(
                    "https://coingecko.mpp.paywithlocus.com/coingecko/search",
                    method="POST",
                    body={"query": token_name},
                )
                self._track(0.06)
                coins = search.get("data", {}).get("coins") or search.get("coins") or []
                if not coins:
                    return empty
                best = min(coins, key=lambda c: c.get("market_cap_rank") or 9999)
                coin_id = best["id"]
                self._coingecko_id_cache[token_name.lower()] = coin_id
            except Exception:
                return empty

        if not self._check_budget(0.06):
            return empty

        try:
            price_data = self._tempo_fetch(
                "https://coingecko.mpp.paywithlocus.com/coingecko/simple-price",
                method="POST",
                body={
                    "ids": coin_id,
                    "vs_currencies": "usd",
                    "include_24hr_change": True,
                    "include_24hr_vol": True,
                    "include_market_cap": True,
                },
            )
            self._track(0.06)
            d = price_data.get("data", {}).get(coin_id) or price_data.get(coin_id) or {}
            if not d:
                return empty
            return {
                "found": True,
                "name": token_name,
                "symbol": token_name.upper(),
                "price": d.get("usd", 0),
                "change_24h": d.get("usd_24h_change"),
                "volume_24h": d.get("usd_24h_vol"),
                "market_cap": d.get("usd_market_cap"),
                "source": "coingecko",
            }
        except Exception:
            return empty

    def _codex_token(self, name: str) -> dict | None:
        if not self._check_budget(0.02):
            return None
        try:
            data = self._tempo_fetch(
                "https://graph.codex.io/graphql",
                method="POST",
                body={
                    "query": f'{{ filterTokens(phrase: "{name}", limit: 3) {{ results {{ token {{ name symbol address networkId }} priceUSD volume24 liquidity marketCap }} }} }}'
                },
            )
            self._track(0.02)
            results = data.get("data", {}).get("filterTokens", {}).get("results") or []
            if not results:
                return None
            best = max(results, key=lambda r: float(r.get("liquidity") or 0))
            return {
                "symbol": best.get("token", {}).get("symbol"),
                "address": best.get("token", {}).get("address"),
                "networkId": best.get("token", {}).get("networkId"),
                "price": float(best["priceUSD"]) if best.get("priceUSD") else None,
                "volume24": float(best["volume24"]) if best.get("volume24") else None,
                "liquidity": float(best["liquidity"]) if best.get("liquidity") else None,
                "marketCap": float(best["marketCap"]) if best.get("marketCap") else None,
            }
        except Exception:
            return None

    # --- Price History (for charts) ---

    def get_price_history(self, token_name: str, days: int = 30) -> list[dict]:
        """Get price history for charting. Returns [{date, price}, ...].

        Uses CoinGecko market-chart ($0.06). Accepts symbols (SOL) or names (Solana).
        Supports 1d, 7d, 30d, 90d, 365d timeframes.
        """
        normalized = _normalize_token(token_name)
        coin_id = COINGECKO_ID_MAP.get(normalized.upper()) or self._coingecko_id_cache.get(normalized.lower())

        if not coin_id:
            # Resolve via search
            self.get_token_price(token_name)
            coin_id = COINGECKO_ID_MAP.get(normalized.upper()) or self._coingecko_id_cache.get(normalized.lower())
            if not coin_id:
                return []

        if not self._check_budget(0.06):
            return []

        try:
            data = self._tempo_fetch(
                "https://coingecko.mpp.paywithlocus.com/coingecko/market-chart",
                method="POST",
                body={"id": coin_id, "vs_currency": "usd", "days": str(days)},
            )
            self._track(0.06)
            prices = data.get("data", {}).get("prices") or data.get("prices") or []

            target_points = min(max(days * 2, 48), 100)
            step = max(len(prices) // target_points, 1)
            return [
                {
                    "date": (
                        datetime.fromtimestamp(p[0] / 1000, tz=UTC).strftime("%Y-%m-%d %H:%M")
                        if days <= 7
                        else datetime.fromtimestamp(p[0] / 1000, tz=UTC).strftime("%Y-%m-%d")
                    ),
                    "price": p[1],
                }
                for i, p in enumerate(prices)
                if i % step == 0 or i == len(prices) - 1
            ]
        except Exception:
            return []

    # --- OHLC Data (for candlestick charts) ---

    def get_ohlc(self, token_name: str, days: int = 30) -> list[dict]:
        """Get OHLC candlestick data. Returns [{date, open, high, low, close}, ...].

        Uses CoinGecko OHLC ($0.06). For candle charts.
        """
        normalized = _normalize_token(token_name)
        coin_id = COINGECKO_ID_MAP.get(normalized.upper()) or self._coingecko_id_cache.get(normalized.lower())

        if not coin_id:
            self.get_token_price(token_name)
            coin_id = COINGECKO_ID_MAP.get(normalized.upper()) or self._coingecko_id_cache.get(normalized.lower())
            if not coin_id:
                return []

        if not self._check_budget(0.06):
            return []

        try:
            data = self._tempo_fetch(
                "https://coingecko.mpp.paywithlocus.com/coingecko/ohlc",
                method="POST",
                body={"id": coin_id, "vs_currency": "usd", "days": str(days)},
            )
            self._track(0.06)
            candles = data.get("data") or data or []
            if not isinstance(candles, list):
                return []
            return [
                {
                    "date": datetime.fromtimestamp(c[0] / 1000, tz=UTC).strftime("%Y-%m-%d"),
                    "open": c[1],
                    "high": c[2],
                    "low": c[3],
                    "close": c[4],
                }
                for c in candles
                if isinstance(c, list) and len(c) >= 5
            ]
        except Exception:
            return []

    # --- Trending Tokens ---

    def get_trending(self) -> list[dict]:
        """Get top trending tokens from CoinGecko ($0.06). Returns name, symbol, rank, price, change_24h."""
        if not self._check_budget(0.06):
            return []
        try:
            data = self._tempo_fetch(
                "https://coingecko.mpp.paywithlocus.com/coingecko/trending",
                method="POST",
                body={},
            )
            self._track(0.06)
            coins = data.get("data", {}).get("coins") or data.get("coins") or []
            return [
                {
                    "name": (c.get("item") or c).get("name", ""),
                    "symbol": (c.get("item") or c).get("symbol", ""),
                    "rank": i + 1,
                    "price": (c.get("item") or c).get("data", {}).get("price"),
                    "change_24h": (c.get("item") or c).get("data", {}).get("price_change_percentage_24h", {}).get("usd"),
                }
                for i, c in enumerate(coins[:10])
            ]
        except Exception as e:
            return [{"error": str(e)}]

    # --- Market Snapshot (BTC, ETH, SOL, HYPE + dominance) ---

    def get_market_snapshot(self) -> dict:
        """Get BTC, ETH, SOL, HYPE prices + BTC dominance in a single summary.

        Returns tokens list with price/change/mcap, plus btc_dominance and total_market_cap.
        Costs ~$0.12 (two CoinGecko calls).
        """
        if not self._check_budget(0.12):
            return {"tokens": []}

        symbol_map = {"bitcoin": "BTC", "ethereum": "ETH", "solana": "SOL", "hyperliquid": "HYPE"}
        try:
            price_data = self._tempo_fetch(
                "https://coingecko.mpp.paywithlocus.com/coingecko/simple-price",
                method="POST",
                body={
                    "ids": "bitcoin,ethereum,solana,hyperliquid",
                    "vs_currencies": "usd",
                    "include_24hr_change": True,
                    "include_market_cap": True,
                },
            )
            self._track(0.06)
            prices = price_data.get("data") or price_data or {}
            tokens = []
            for cg_id, symbol in symbol_map.items():
                d = prices.get(cg_id, {})
                if d.get("usd"):
                    tokens.append({
                        "symbol": symbol,
                        "price": d["usd"],
                        "change_24h": d.get("usd_24h_change", 0),
                        "market_cap": d.get("usd_market_cap", 0),
                    })

            result: dict[str, Any] = {"tokens": tokens}
            try:
                global_data = self._tempo_fetch(
                    "https://coingecko.mpp.paywithlocus.com/coingecko/global",
                    method="POST",
                    body={},
                )
                self._track(0.06)
                gd = global_data.get("data") or global_data or {}
                result["btc_dominance"] = gd.get("market_cap_percentage", {}).get("btc")
                result["total_market_cap"] = gd.get("total_market_cap", {}).get("usd")
            except Exception:
                pass

            return result
        except Exception as e:
            return {"tokens": [], "error": str(e)}

    # --- Wallet Data (Allium) ---

    def get_wallet(self, address: str, chain: str = "ethereum") -> dict:
        """Get wallet balances and total value via Allium ($0.03).

        Returns address, chain, balances list, and total_value_usd.
        """
        if not self._check_budget(0.03):
            return {"address": address, "chain": chain, "balances": [], "total_value_usd": 0}
        try:
            data = self._tempo_fetch(
                "https://agents.allium.so/api/v1/developer/wallet/balances",
                method="POST",
                body={"address": address, "chains": [chain]},
            )
            self._track(0.03)
            raw = data if isinstance(data, list) else data.get("data", [])
            balances = [
                {
                    "token": b.get("token_name") or b.get("name", ""),
                    "symbol": b.get("token_symbol") or b.get("symbol", ""),
                    "amount": b.get("balance") or b.get("amount", 0),
                    "value_usd": b.get("value_usd") or b.get("usd_value", 0),
                }
                for b in raw
            ]
            return {
                "address": address,
                "chain": chain,
                "balances": balances,
                "total_value_usd": sum(b["value_usd"] for b in balances),
            }
        except Exception as e:
            return {"address": address, "chain": chain, "balances": [], "total_value_usd": 0, "error": str(e)}

    # --- Dune SQL ---

    def run_dune_query(self, sql: str) -> dict:
        """Run a SQL query on Dune Analytics ($0.05-4.00). Async with polling.

        Only use for explicit data requests. Returns rows, columns, row_count.
        """
        if not self._check_budget(0.10):
            return {"rows": [], "columns": [], "row_count": 0, "error": "budget_exceeded"}
        try:
            submit = self._tempo_fetch(
                "https://api.dune.com/api/v1/sql/execute",
                method="POST",
                body={"sql": sql},
                timeout=60,
            )
            execution_id = submit.get("execution_id")
            if not execution_id:
                return {"rows": [], "columns": [], "row_count": 0, "error": "no_execution_id"}

            for attempt in range(12):
                time.sleep(min(5 * (attempt + 1), 15))
                poll = self._tempo_fetch(
                    f"https://api.dune.com/api/v1/execution/{execution_id}/results",
                    method="GET",
                    timeout=30,
                )
                if poll.get("is_execution_finished"):
                    self._track(0.05)
                    result = poll.get("result", {})
                    return {
                        "execution_id": execution_id,
                        "rows": result.get("rows", []),
                        "columns": result.get("metadata", {}).get("column_names", []),
                        "row_count": result.get("metadata", {}).get("row_count", 0),
                    }
                if poll.get("state") == "QUERY_STATE_FAILED":
                    return {"rows": [], "columns": [], "row_count": 0, "error": poll.get("error", "query_failed")}

            return {"rows": [], "columns": [], "row_count": 0, "error": "timeout"}
        except Exception as e:
            return {"rows": [], "columns": [], "row_count": 0, "error": str(e)}

    # --- Budget Info ---

    def get_spend(self) -> dict:
        """Get current daily MPP spend and budget remaining."""
        return {
            "daily_spend": round(self._daily_spend, 4),
            "daily_cap": self._daily_cap,
            "remaining": round(self._daily_cap - self._daily_spend, 4),
        }


def _client() -> MppClient:
    return MppClient()
