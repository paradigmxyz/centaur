"""Tako API client.

Wraps the official `tako-sdk` (https://pypi.org/project/tako-sdk/). Tako returns
structured, cited data cards backed by licensed sources, plus a knowledge graph
that resolves entities and metrics to node ids you can pin into a search.

`available_data` is the free discovery step, ported from the tako-mcp
`tako_available_data` tool. It resolves a name to graph nodes and reports the
data Tako holds for each, so agents confirm coverage, and learn the exact
metric and entity names, before spending a priced `search` or `answer` call.

Two backends, selected by whether a TAKO_API_KEY is configured (never by the
key's value, since inside a sandbox the tool only holds placeholders):

- key configured -> the SDK against tako.com/api, full features.
- no key -> Tako's free rate-limited hosted MCP (`_mcp.py`) for `search`,
  `answer`, and `available_data`; `contents` stays key-only. Same fallback
  shape as the websearch tool's keyless Parallel MCP path.

Responses carry `meta.backend` ("tako:sdk" | "tako:mcp") and
`meta.partial_failures` for knobs a backend cannot honor.

Card renderings leave here pinned to light mode regardless of backend; see
`_theme.py` for why, and for the server-side gap that makes it a no-op today.

API docs: https://docs.tako.com
"""

from __future__ import annotations

import logging
import os
from collections.abc import Callable
from dataclasses import asdict
from typing import Any

from centaur_sdk import get_tool_context, secret
from tako import (
    Configuration,
    ContentsRequest,
    DataSourceSettings,
    SearchRequest,
    Sources,
    WebSourceSettings,
)
from tako.exceptions import ApiException
from tako.lib import Tako
from tako.models.contents_delivery_mode import ContentsDeliveryMode
from tako.models.contents_format import ContentsFormat
from tako.models.search_effort_level import SearchEffortLevel

from ._coverage import (
    EXPAND_TOP_N,
    NER_LABELS,
    NODE_TYPES,
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
from ._mcp import MCP_URL, TakoMcpBackend
from ._theme import apply_light_mode

logger = logging.getLogger(__name__)

# The maximum node ids the API accepts as retrieval candidates per search.
MAX_NODE_IDS = 20
# Per-source result cap (DataSourceSettings.count is 1-20 in the SDK; 0 is
# this tool's "skip the source" sentinel and is never sent).
MAX_SOURCE_COUNT = 20

# Valid option values, derived from the SDK enums so an upstream addition is
# accepted the moment the dependency updates (the pin is open: >=2.2.6).
EFFORT_LEVELS = tuple(e.value for e in SearchEffortLevel)
CONTENT_MODES = tuple(m.value for m in ContentsDeliveryMode)
CONTENT_FORMATS = tuple(f.value for f in ContentsFormat)

# Request timeouts, in seconds. urllib3's PoolManager defaults to NO timeout
# and the retry config excludes POST, so without these a hung search/answer/
# contents read would block until the sandbox kills the process.
DEFAULT_TIMEOUT_SECONDS = 120.0
GRAPH_TIMEOUT_SECONDS = 30.0


def _validate_choice(name: str, value: str | None, allowed: tuple[str, ...]) -> None:
    """Reject an invalid enum option with a one-line error, pre-network."""
    if value is not None and value not in allowed:
        raise ValueError(f"{name} must be one of: {', '.join(allowed)}")


def _is_configured(key: str) -> bool:
    """Authoritative check for whether a secret was explicitly configured.

    `secret(key)` is unsafe for routing decisions: under centaur's default
    StubBackend it returns the literal key name as a placeholder for
    un-configured secrets. Both signals are needed (same helper as the
    websearch tool):

    - Server / tool-runtime: ToolManager populates ``ctx.secrets[key]`` only
      for secrets it actually resolved, so dict membership is authoritative.
    - CLI / direct-invoke: no ToolContext is bound; fall through to
      ``secret(key)`` and treat the value-equals-key stub case as "not
      configured".
    """
    try:
        ctx = get_tool_context()
        return bool(ctx.secrets.get(key))
    except LookupError:
        try:
            val = secret(key)
        except KeyError:
            return False
        return bool(val) and val != key


def _proxy_url() -> str | None:
    """Return the sandbox egress proxy, if one is configured.

    The SDK's transport is urllib3, which, unlike httpx or requests, does not
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


def _validate_source_args(
    data_count: int | None,
    web_count: int | None,
    node_ids: list[str] | None,
    strict: bool,
) -> None:
    """Source-selection contract checks, shared by the SDK and MCP backends.

    Rejects: counts outside 0-20, `strict` without `node_ids`, more than
    MAX_NODE_IDS ids, `data_count=0` combined with node pinning, and skipping
    both sources.
    """
    for name, count in (("data_count", data_count), ("web_count", web_count)):
        if count is not None and not 0 <= count <= MAX_SOURCE_COUNT:
            raise ValueError(
                f"{name} must be between 0 (skip this source) and {MAX_SOURCE_COUNT}"
            )
    if strict and not node_ids:
        raise ValueError("strict=True requires node_ids")
    if node_ids and len(node_ids) > MAX_NODE_IDS:
        raise ValueError(f"node_ids accepts at most {MAX_NODE_IDS} ids")
    if data_count == 0 and (node_ids or strict):
        raise ValueError(
            "data_count=0 skips the data index, which contradicts node_ids/strict "
            "(pinned nodes apply only to the data source)"
        )
    if data_count == 0 and web_count == 0:
        raise ValueError("cannot skip both sources; drop one of the zero counts")


def _sources(
    data_count: int | None,
    web_count: int | None,
    node_ids: list[str] | None,
    strict: bool,
) -> Sources | None:
    """Build a Sources object, or None to accept the API default (data + web).

    A source is searched only if its key is present, so passing `data_count=0`
    is how you get a data-only or web-only search. A skipped source must be
    genuinely ABSENT from the request body: passing `data=None` explicitly
    would land `data` in pydantic's model_fields_set and the generated
    `Sources.to_dict` re-emits it as `"data": null`: key present, promise
    broken. Hence the conditional kwargs at the bottom.

    Raises ValueError on contract violations the API would otherwise reject
    with a raw 400 (or worse, silently misread).
    """
    _validate_source_args(data_count, web_count, node_ids, strict)
    if data_count is None and web_count is None and not node_ids and not strict:
        return None

    kwargs: dict[str, Any] = {}
    if data_count is None or data_count > 0:
        kwargs["data"] = DataSourceSettings(
            count=data_count,
            node_ids=node_ids or None,
            strict=strict or None,
        )
    if web_count is None or web_count > 0:
        kwargs["web"] = WebSourceSettings(count=web_count)
    return Sources(**kwargs)


class TakoClient:
    """Client for the Tako API (SDK with a key, free hosted MCP without)."""

    def __init__(
        self,
        retries: int = 2,
        timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
        graph_timeout_seconds: float = GRAPH_TIMEOUT_SECONDS,
        api_key: str | None = None,
        mcp_url: str | None = None,
    ):
        self._timeout = timeout_seconds
        self._graph_timeout = graph_timeout_seconds
        self._mcp_url = mcp_url or MCP_URL
        self._has_key = api_key is not None or _is_configured("TAKO_API_KEY")
        # Inside a sandbox, tool secrets have NO env signal: `secret()` hands
        # back the placeholder name and iron-proxy swaps it on the wire, so
        # _is_configured() cannot see whether the deployment vault holds the
        # key. When the firewall is active, take the SDK path optimistically
        # (if the vault has the key, the swap makes it work) and fall back to
        # the free MCP tier once on an auth rejection (vault doesn't have it).
        firewall_active = bool(_proxy_url())
        self._fallback_on_auth = not self._has_key and firewall_active

        if not self._has_key and not firewall_active:
            # Keyless outside a sandbox: everything except `contents` runs on
            # the free hosted MCP tier directly.
            self._client = None
            self._mcp = TakoMcpBackend(
                mcp_url=self._mcp_url, timeout_seconds=timeout_seconds
            )
            return

        self._mcp = None
        config = Configuration(retries=retries)
        # Read via secret() so the firewall (StubBackend -> iron-proxy) can
        # swap the placeholder on outbound headers; _is_configured() above is
        # the routing signal, never this value.
        config.api_key["apiKey"] = api_key or secret("TAKO_API_KEY")

        proxy = _proxy_url()
        if proxy:
            config.proxy = proxy
        ca_bundle = _ca_bundle()
        if ca_bundle:
            config.ssl_ca_cert = ca_bundle

        self._client = Tako(config)
        # The beta graph endpoints are missing security declarations in the
        # SDK's OpenAPI spec, so the generated client attaches no X-API-Key to
        # graph_search/graph_related (their auth_settings list is empty) and
        # the API answers 401. Setting the key as a default header covers
        # every operation; on endpoints that do declare auth, the generated
        # client overwrites it with the same value. Worth an upstream spec
        # fix, at which point this line can go.
        self._client._api.api_client.set_default_header(
            "X-API-Key", config.api_key["apiKey"]
        )

    @property
    def backend(self) -> str:
        """Which backend serves this client: 'tako:sdk' or 'tako:mcp'."""
        return "tako:mcp" if self._mcp is not None else "tako:sdk"

    def _make_mcp(self) -> TakoMcpBackend:
        """Fallback factory; an instance attribute so tests can substitute it."""
        return TakoMcpBackend(mcp_url=self._mcp_url, timeout_seconds=self._timeout)

    def _with_fallback(self, sdk_call, mcp_call):
        """Run the SDK path, degrading to the free MCP once on sandbox 401s.

        Only when no key was explicitly configured AND the sandbox firewall is
        active: an auth rejection there means the deployment vault has no
        TAKO_API_KEY, so the placeholder was forwarded un-swapped. The switch
        is remembered for the client's lifetime, so one probe pays the cost.
        """
        try:
            return sdk_call()
        except ApiException as exc:
            if self._fallback_on_auth and getattr(exc, "status", None) in (401, 403):
                logger.warning(
                    "tako: API rejected the placeholder credential (no "
                    "TAKO_API_KEY in the deployment vault); falling back to "
                    "the free MCP tier"
                )
                self._fallback_on_auth = False
                self._mcp = self._make_mcp()
                return mcp_call()
            raise

    # Calls go through the generated TakoApi (`_client._api`) rather than the
    # `Tako` facade: the facade does not forward `_request_timeout`, and
    # urllib3 defaults to NO timeout with POST excluded from retries, so a
    # hung read would otherwise block until the sandbox kills the process.
    # Same escape hatch as the default-header workaround above; drop both
    # when fixed upstream.

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
            effort: 'instant', 'fast' (default), or 'deep'. The keyless MCP
                backend serves fast and instant only; 'deep' is flagged in
                meta.partial_failures and the search runs at the default.
            data_count: Max Tako data cards, 1-20. Pass 0 to skip the data index.
            web_count: Max web results, 1-20. Pass 0 to skip the web index.
            node_ids: Graph node ids to pin as retrieval candidates (max 20).
                Take them from an `available_data` match. Ids are not durable
                across knowledge-graph rebuilds.
            strict: Return only cards matching `node_ids`. Requires node_ids.
            country_code: ISO 3166-1 alpha-2 code for localization.
            locale: BCP-47 locale tag.

        Returns:
            A dict with `cards`, `web_results`, `request_id`, `usage`, and
            `meta` ({backend, partial_failures}).
        """
        _validate_choice("effort", effort, EFFORT_LEVELS)
        _validate_source_args(data_count, web_count, node_ids, strict)

        def via_mcp() -> dict:
            # Wrapped here rather than at each call site so the keyless path and
            # the 401-fallback path are pinned to light by one edit.
            return apply_light_mode(
                self._mcp.search(
                    query,
                    effort=effort,
                    data_count=data_count,
                    web_count=web_count,
                    node_ids=node_ids,
                    strict=strict,
                    country_code=country_code,
                    locale=locale,
                )
            )

        if self._mcp is not None:
            return via_mcp()
        request = SearchRequest(
            query=query,
            effort=effort,
            sources=_sources(data_count, web_count, node_ids, strict),
            country_code=country_code,
            locale=locale,
        )
        return self._with_fallback(
            lambda: apply_light_mode(
                _with_meta(
                    _add_search_markdown(
                        _dump(self._client._api.search(request, _request_timeout=self._timeout))
                    )
                )
            ),
            via_mcp,
        )

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
        where `cards[0]` is the lead card, the one to show alongside the text.
        """
        _validate_choice("effort", effort, EFFORT_LEVELS)
        _validate_source_args(data_count, web_count, node_ids, strict)

        def via_mcp() -> dict:
            return apply_light_mode(
                self._mcp.answer(
                    query,
                    effort=effort,
                    data_count=data_count,
                    web_count=web_count,
                    node_ids=node_ids,
                    strict=strict,
                    country_code=country_code,
                    locale=locale,
                )
            )

        if self._mcp is not None:
            return via_mcp()
        request = SearchRequest(
            query=query,
            effort=effort,
            sources=_sources(data_count, web_count, node_ids, strict),
            country_code=country_code,
            locale=locale,
        )
        return self._with_fallback(
            lambda: apply_light_mode(
                _with_meta(_dump(self._client._api.answer(request, _request_timeout=self._timeout)))
            ),
            via_mcp,
        )

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
        the page's extracted text. Not every card is exportable; protected
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
        if self._mcp is not None:
            raise ValueError(
                "contents requires TAKO_API_KEY (row-level export billing is "
                "not part of the free tier); search and available_data work "
                "without one"
            )
        _validate_choice("mode", mode, CONTENT_MODES)
        _validate_choice("content_format", content_format, CONTENT_FORMATS)
        request = ContentsRequest(
            url=url,
            mode=mode,
            content_format=content_format,
            max_rows=max_rows,
            max_chars=max_chars,
            quote_only=quote_only or None,
        )
        return _with_meta(
            _dump(self._client._api.contents(request, _request_timeout=self._timeout))
        )

    # -- data-coverage discovery -------------------------------------------

    def available_data(
        self,
        q: str,
        types: str | None = None,
        label: str | None = None,
    ) -> dict:
        """Find what data Tako has on an entity or metric, in one free call.

        Resolves `q` against the knowledge graph, then drills coverage for the
        top hits (an entity reports its metrics; a metric reports the entities
        it is tracked across) and returns a deterministic summary. Run this
        before `search` or `answer`: it confirms the data exists and returns
        the exact names and node ids that make the priced follow-up land
        precisely.

        Args:
            q: Entity or metric name to look up (min 2 chars).
            types: Narrow resolution to 'entity' (a thing) or 'metric'
                (a measure). Omit to search both.
            label: NER label to prefer, a boost rather than a filter (PERSON,
                ORG, GPE, LOC, PRODUCT, EVENT, LANGUAGE, MONEY, METRIC,
                STOCK_TICKER, WEBSITE).

        Returns:
            {found, query, summary, matches, other_matches}. `found` is true
            only when a DRILLED match has live coverage; node resolution alone
            never counts, and hits beyond the top EXPAND_TOP_N land in
            `other_matches` unchecked, so `found=False` means "not confirmed
            in the drilled set", not "Tako has no data". Each match carries
            coverage.names (reuse verbatim in a follow-up query) and node_id
            (pin via search(node_ids=...)). `other_matches` include node_id on
            the SDK backend; the hosted MCP returns name/type only today.
        """
        _validate_discovery_args(q, types, label)
        if self._mcp is not None:
            return self._mcp.available_data(q, types=types, label=label)
        return self._with_fallback(
            lambda: _run_available_data(
                q,
                graph_search=self._graph_search,
                graph_related=self._graph_related,
                types=types,
                label=label,
            ),
            lambda: self._mcp.available_data(q, types=types, label=label),
        )

    def probe(self) -> dict:
        """Cheapest authenticated (or anonymous) read, for `health`."""
        if self._mcp is not None:
            return self._mcp.available_data("nvidia")
        return self._with_fallback(
            lambda: _dump(self._graph_search("nvidia", limit=1)),
            lambda: self._mcp.available_data("nvidia"),
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
        return self._client._api.graph_search(
            q,
            types=types,
            limit=limit,
            label=label,
            infer_label=infer_label,
            _request_timeout=self._graph_timeout,
        )

    def _graph_related(
        self,
        node_id: str,
        relation: str | None = None,
        limit: int | None = None,
    ):
        """Walk one relation from a graph node. Returns the SDK model."""
        return self._client._api.graph_related(
            node_id,
            relation=relation,
            limit=limit,
            _request_timeout=self._graph_timeout,
        )


def _validate_discovery_args(q: str, types: str | None, label: str | None) -> None:
    """available_data input contract, shared by the SDK pipeline and MCP path."""
    if len(q.strip()) < 2:
        raise ValueError("q must be at least 2 characters")
    if types is not None and types not in NODE_TYPES:
        raise ValueError(f"types must be one of: {', '.join(NODE_TYPES)}")
    if label is not None and label not in NER_LABELS:
        raise ValueError(f"label must be one of: {', '.join(NER_LABELS)}")


def _with_meta(payload: dict) -> dict:
    """Stamp SDK-path responses with the same meta shape the MCP path emits."""
    payload["meta"] = {"backend": "tako:sdk", "partial_failures": []}
    return payload


def _add_search_markdown(payload: dict) -> dict:
    """Add an `answer_markdown` rendering to an SDK-path search response.

    The MCP backend returns the hosted tool's readable `## Tako Data` document
    in `answer_markdown`; the SDK backend returns only structured cards. This
    renders an equivalent readable document from those cards so both backends
    return the same shape and downstream models get prose to synthesize from,
    while the full structured cards (with row `content`, `image_url`, etc.)
    stay in `cards`. Rows are pointed at, not inlined (matching tako-mcp#187).
    """
    payload["answer_markdown"] = _render_search_markdown(
        payload.get("cards") or [], payload.get("web_results") or []
    )
    return payload


def _render_search_markdown(cards: list[dict], web_results: list[dict]) -> str:
    blocks: list[str] = []
    if cards:
        blocks.append(f"## Tako Data ({len(cards)} {'card' if len(cards) == 1 else 'cards'})")
        for i, card in enumerate(cards, 1):
            lines = [f"### {i}. {card.get('title') or 'Untitled card'}"]
            if card.get("description"):
                lines.append(str(card["description"]))
            facts = []
            if card.get("data_freshness"):
                facts.append(f"freshness: {card['data_freshness']}")
            if card.get("exportable"):
                facts.append("exportable via `contents`")
            if facts:
                lines.append(" · ".join(facts))
            if card.get("image_url"):
                lines.append(f"chart: {card['image_url']}")
            if card.get("content"):
                lines.append("data: rows available in this card's `content`")
            blocks.append("\n".join(lines))
    if web_results:
        blocks.append(f"## Web Results ({len(web_results)})")
        for i, w in enumerate(web_results, 1):
            blocks.append(f"{i}. [{w.get('title') or w.get('url')}]({w.get('url')})")
    return "\n\n".join(blocks)


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
    `unavailable` match rather than sinking the whole call, since auth and
    connectivity are already proven by the search. A search failure raises,
    because there is nothing to salvage.

    Raises ValueError on invalid input, so library and workflow callers get
    the same guardrails as the CLI.
    """
    _validate_discovery_args(q, types, label)
    response = graph_search(q, types=types, limit=10, label=label)
    results = list(response.results or [])
    if not results:
        return _with_meta(
            {
                "found": False,
                "query": q,
                "summary": build_summary(q, [], []),
                "matches": [],
                "other_matches": [],
            }
        )

    top = results[:EXPAND_TOP_N]
    others = [
        OtherMatch(node_id=node.id, name=node.name, type=enum_value(node.type))
        for node in results[EXPAND_TOP_N:]
    ]

    matches = []
    for node in top:
        relation = coverage_kind_for(node.type)
        try:
            related = graph_related(node.id, relation=relation, limit=PREVIEW)
            matches.append(build_match(node, related.relation))
        except Exception as exc:
            # Isolated, not ignored: the match degrades to "unavailable" but
            # the failure stays visible to operators, so a systematic drill
            # regression doesn't masquerade as transient flakiness forever.
            logger.warning(
                "available-data coverage drill failed for node %s (relation=%s): %s",
                node.id,
                relation,
                exc,
            )
            matches.append(unavailable_match(node))

    return _with_meta(
        {
            "found": any(has_live_coverage(m) for m in matches),
            "query": q,
            "summary": build_summary(q, matches, others),
            "matches": [match_to_dict(m) for m in matches],
            "other_matches": [asdict(o) for o in others],
        }
    )


def _client() -> TakoClient:
    """Factory: create a TakoClient using the TAKO_API_KEY secret."""
    return TakoClient()
