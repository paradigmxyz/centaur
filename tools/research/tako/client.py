"""Tako API client.

Wraps the official `tako-sdk` (https://pypi.org/project/tako-sdk/). Tako returns
structured, cited data cards backed by licensed sources, plus a knowledge graph
that resolves entities and metrics to node ids you can pin into a search.

`available_data` is the free discovery step (ported from the tako-mcp
`tako_available_data` tool): it resolves a name to graph nodes and reports the
data Tako holds for each, so agents confirm coverage — and learn the exact
metric/entity names — before spending a priced `search` or `answer` call.

API docs: https://docs.tako.com
"""

from __future__ import annotations

import os
from collections.abc import Callable
from dataclasses import asdict
from typing import Any

from centaur_sdk import secret
from tako import (
    Configuration,
    ContentsRequest,
    DataSourceSettings,
    SearchRequest,
    Sources,
    WebSourceSettings,
)
from tako.lib import Tako

from ._coverage import (
    EXPAND_TOP_N,
    PREVIEW,
    OtherMatch,
    build_match,
    build_summary,
    coverage_kind_for,
    enum_value,
    has_live_coverage,
    match_to_dict,
    unavailable_match,
)

# The SDK talks to https://tako.com/api by default (tako/configuration.py).
API_HOST = "tako.com"


def _proxy_url() -> str | None:
    """Return the sandbox egress proxy, if one is configured.

    The SDK's transport is urllib3, which — unlike httpx or requests — does not
    read proxy settings from the environment: with `configuration.proxy` unset
    it builds a plain `PoolManager` and dials the upstream directly
    (tako/rest.py). In a Centaur sandbox that means the request never reaches
    iron-proxy, so the credential placeholder is never swapped and the
    default-deny NetworkPolicy drops the connection. Wire the proxy explicitly.

    Same shape as `tools/productivity/gsuite/client.py`, which works around the
    identical limitation in google-api-python-client.
    """
    return os.environ.get("HTTPS_PROXY") or os.environ.get("https_proxy")  # noqa: TID251


def _ca_bundle() -> str | None:
    """Return the CA bundle to trust, if the sandbox pins one.

    iron-proxy terminates TLS, so its CA has to be trusted or every request
    fails certificate verification.
    """
    return os.environ.get("SSL_CERT_FILE") or os.environ.get("REQUESTS_CA_BUNDLE")  # noqa: TID251


def _dump(model: Any) -> Any:
    """Convert an SDK pydantic model into plain JSON-safe data."""
    if model is None:
        return None
    if hasattr(model, "model_dump"):
        return model.model_dump(mode="json", exclude_none=True)
    return model


def _sources(
    data_count: int | None,
    web_count: int | None,
    node_ids: list[str] | None,
    strict: bool,
) -> Sources | None:
    """Build a Sources object, or None to accept the API default (data + web).

    A source is searched only if its key is present, so passing `data_count=0`
    is how you get a data-only or web-only search.
    """
    if data_count is None and web_count is None and not node_ids and not strict:
        return None

    data = None
    if data_count is None or data_count > 0:
        data = DataSourceSettings(
            count=data_count,
            node_ids=node_ids or None,
            strict=strict or None,
        )

    web = None
    if web_count is None or web_count > 0:
        web = WebSourceSettings(count=web_count)

    return Sources(data=data, web=web)


class TakoClient:
    """Client for the Tako API."""

    def __init__(self, retries: int = 2):
        config = Configuration(retries=retries)
        config.api_key["apiKey"] = secret("TAKO_API_KEY")

        proxy = _proxy_url()
        if proxy:
            config.proxy = proxy
        ca_bundle = _ca_bundle()
        if ca_bundle:
            config.ssl_ca_cert = ca_bundle

        self._client = Tako(config)

    # -- search and answer ------------------------------------------------

    def search(
        self,
        query: str,
        effort: str | None = None,
        data_count: int | None = None,
        web_count: int | None = None,
        node_ids: list[str] | None = None,
        strict: bool = False,
        country_code: str | None = None,
        locale: str | None = None,
    ) -> dict:
        """Search Tako for structured data cards and web results.

        Args:
            query: Natural language query.
            effort: 'instant', 'fast' (default), or 'deep'.
            data_count: Max Tako data cards, 1-20. Pass 0 to skip the data index.
            web_count: Max web results, 1-20. Pass 0 to skip the web index.
            node_ids: Graph node ids to pin as retrieval candidates (max 20).
                Resolve them with `graph_search` first. Ids are not durable
                across knowledge-graph rebuilds.
            strict: Return only cards matching `node_ids`. Requires node_ids.
            country_code: ISO 3166-1 alpha-2 code for localization.
            locale: BCP-47 locale tag.

        Returns:
            A dict with `cards`, `web_results`, `request_id`, and `usage`.
        """
        request = SearchRequest(
            query=query,
            effort=effort,
            sources=_sources(data_count, web_count, node_ids, strict),
            country_code=country_code,
            locale=locale,
        )
        return _dump(self._client.search(request))

    def answer(
        self,
        query: str,
        effort: str | None = None,
        data_count: int | None = None,
        web_count: int | None = None,
        node_ids: list[str] | None = None,
        strict: bool = False,
        country_code: str | None = None,
        locale: str | None = None,
    ) -> dict:
        """Get a synthesized written answer with the cards that support it.

        Same arguments as `search`. Returns a dict with `answer` and `cards`,
        where `cards[0]` is the lead card — the one to show alongside the text.
        """
        request = SearchRequest(
            query=query,
            effort=effort,
            sources=_sources(data_count, web_count, node_ids, strict),
            country_code=country_code,
            locale=locale,
        )
        return _dump(self._client.answer(request))

    # -- contents ---------------------------------------------------------

    def contents(
        self,
        url: str,
        mode: str | None = None,
        content_format: str | None = None,
        max_rows: int | None = None,
        max_chars: int | None = None,
        quote_only: bool = False,
    ) -> dict:
        """Fetch the underlying data behind a result URL.

        A Tako card URL resolves to the card's data; any other URL resolves to
        the page's extracted text. Not every card is exportable — protected
        sources return 403.

        Args:
            url: A card's `webpage_url` or a web result's `url`.
            mode: 'url' (default) for a presigned link, or 'inline'.
            content_format: 'csv' (default), 'json_records', or 'json_compact'.
                Ignored for web URLs.
            max_rows: Row cap for card exports. Defaults to the 20-row free
                allowance; rows beyond it are billed. Ceiling is 2,000.
            max_chars: Character cap on extracted web text. Default 10,000.
            quote_only: Price the export without fetching or being charged.
        """
        request = ContentsRequest(
            url=url,
            mode=mode,
            content_format=content_format,
            max_rows=max_rows,
            max_chars=max_chars,
            quote_only=quote_only or None,
        )
        return _dump(self._client.contents(request))

    # -- data-coverage discovery -------------------------------------------

    def available_data(
        self,
        q: str,
        types: str | None = None,
        label: str | None = None,
    ) -> dict:
        """Find what data Tako has on an entity or metric — free, one call.

        Resolves `q` against the knowledge graph, then drills coverage for the
        top hits (entity → its metrics; metric → the entities it is tracked
        across) and returns a deterministic summary. Run this before `search`
        or `answer`: it confirms the data exists and returns the exact names
        (and node ids) that make the priced follow-up land precisely.

        Args:
            q: Entity or metric name to look up (min 2 chars).
            types: Narrow resolution to 'entity' (a thing) or 'metric'
                (a measure). Omit to search both.
            label: NER label to prefer — a boost, not a filter (PERSON, ORG,
                GPE, LOC, PRODUCT, EVENT, LANGUAGE, MONEY, METRIC,
                STOCK_TICKER, WEBSITE).

        Returns:
            {found, query, summary, matches, other_matches}. `found` is true
            only when a match has live coverage — node resolution alone never
            counts. Each match carries coverage.names (reuse verbatim in a
            follow-up query) and node_id (pin via search(node_ids=...)).
        """
        return _run_available_data(
            q,
            graph_search=self._graph_search,
            graph_related=self._graph_related,
            types=types,
            label=label,
        )

    # -- knowledge graph plumbing ------------------------------------------
    # Private: `available_data` is the supported discovery surface. Kept as
    # methods (not deleted) so re-exposing pagination/cohort walking later is
    # a CLI change, not a rebuild.

    def _graph_search(
        self,
        q: str,
        types: str | None = None,
        limit: int | None = None,
        label: str | None = None,
        infer_label: bool | None = None,
    ):
        """Resolve a name into knowledge-graph nodes. Returns the SDK model."""
        return self._client.graph_search(
            q, types=types, limit=limit, label=label, infer_label=infer_label
        )

    def _graph_related(
        self,
        node_id: str,
        relation: str | None = None,
        limit: int | None = None,
    ):
        """Walk one relation from a graph node. Returns the SDK model."""
        return self._client.graph_related(node_id, relation=relation, limit=limit)


def _run_available_data(
    q: str,
    *,
    graph_search: Callable[..., Any],
    graph_related: Callable[..., Any],
    types: str | None = None,
    label: str | None = None,
) -> dict:
    """The available-data pipeline, decoupled from TakoClient for testing.

    One graph search (limit 10), then a type-aware coverage drill for the top
    EXPAND_TOP_N hits. Per-node error isolation: a failed drill yields an
    `unavailable` match rather than sinking the whole call (auth and
    connectivity are already proven by the search). A search failure raises —
    there is nothing to salvage.
    """
    response = graph_search(q, types=types, limit=10, label=label)
    results = list(response.results or [])
    if not results:
        return {
            "found": False,
            "query": q,
            "summary": build_summary(q, [], []),
            "matches": [],
            "other_matches": [],
        }

    top = results[:EXPAND_TOP_N]
    others = [
        OtherMatch(name=node.name, type=enum_value(node.type))
        for node in results[EXPAND_TOP_N:]
    ]

    matches = []
    for node in top:
        relation = coverage_kind_for(node.type)
        try:
            related = graph_related(node.id, relation=relation, limit=PREVIEW)
            matches.append(build_match(node, related.relation))
        except Exception:
            matches.append(unavailable_match(node))

    return {
        "found": any(has_live_coverage(m) for m in matches),
        "query": q,
        "summary": build_summary(q, matches, others),
        "matches": [match_to_dict(m) for m in matches],
        "other_matches": [asdict(o) for o in others],
    }


def _client() -> TakoClient:
    """Factory: create a TakoClient using the TAKO_API_KEY secret."""
    return TakoClient()
