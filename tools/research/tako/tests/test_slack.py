"""Unit tests for rendering a Tako card as a Slack message. No network."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from tools.research.tako._slack import (
    MAX_CONTAINER_CHILDREN,
    MAX_TITLE_CHARS,
    card_blocks,
    card_child_blocks,
    card_message,
    card_source,
    card_title,
    card_urls,
    cards_from_payload,
    pub_id_of,
)

PUB = "m-p-JX5qxsBStJgywrqI"

CARD = {
    "card_id": PUB,
    "title": "NVIDIA Corporation Total Revenues (Normalized) (Annual)",
    "description": "latest value was $215.9B on Jan 25, 2026, up 10,643.0% since Jan 30, 2005.",
    "webpage_url": f"https://tako.com/card/{PUB}/",
    "image_url": f"https://tako.com/api/v1/image/{PUB}/",
    "embed_url": f"https://tako.com/embed/{PUB}/",
    "sources": [{"source_name": "Fiscal.ai", "source_index": "data"}],
}


class TestCardsFromPayload:
    def test_reads_a_whole_search_response(self):
        payload = {"cards": [CARD], "web_results": [{"url": "https://example.com"}]}
        assert cards_from_payload(payload) == [CARD]

    def test_reads_a_bare_list_of_cards(self):
        assert cards_from_payload([CARD, CARD]) == [CARD, CARD]

    def test_reads_a_single_card_object(self):
        assert cards_from_payload(CARD) == [CARD]

    def test_preserves_order(self):
        second = {**CARD, "card_id": "m-p-SECONDcard0000000"}
        assert [card["card_id"] for card in cards_from_payload({"cards": [CARD, second]})] == [
            CARD["card_id"],
            second["card_id"],
        ]

    def test_drops_cards_without_a_usable_id(self):
        payload = {"cards": [{"title": "no id"}, CARD, "not a dict", None]}
        assert cards_from_payload(payload) == [CARD]

    def test_returns_nothing_for_empty_or_unusable_payloads(self):
        for payload in ({"cards": []}, [], {}, None, "text", 7):
            assert cards_from_payload(payload) == []


# Paid: raw /v3/search body, cards plus the top-level pointer.
SDK_RESPONSE = {
    "cards": [CARD],
    "web_results": [],
    "pub_id": PUB,
    "image_url": f"https://tako.com/api/v1/image/{PUB}/?dark_mode=true",
    "embed_url": f"https://tako.com/embed/{PUB}/?dark_mode=auto",
    "meta": {"backend": "tako:sdk"},
}

# Free, post tako-mcp#187: rebuilt dict, cards present.
MCP_RESPONSE = {
    "query": "nvidia revenue",
    "answer_markdown": "## Tako Data (1 card)",
    "cards": [CARD],
    "web_results": [],
    "request_id": "r",
    "pub_id": PUB,
    "image_url": f"https://tako.com/api/v1/image/{PUB}/?dark_mode=true",
    "embed_url": f"https://tako.com/embed/{PUB}/",
    "meta": {"backend": "tako:mcp", "partial_failures": []},
}

# Free, pre tako-mcp#187: no cards, only the top-level pointer.
MCP_PRE_187 = {
    "query": "nvidia revenue",
    "answer_markdown": "## Tako Data (1 card)\n\n### 1. NVIDIA",
    "cards": [],
    "web_results": [],
    "request_id": "r",
    "pub_id": PUB,
    "image_url": f"https://tako.com/api/v1/image/{PUB}/",
    "meta": {"backend": "tako:mcp", "partial_failures": []},
}


class TestBackendConsistency:
    """The paid SDK path and the free MCP path must render identically.

    The SDK path returns the whole /v3/search body; the MCP path rebuilds an
    equivalent one. Either can arrive with `cards` populated, or with `cards`
    empty and only the top-level lead-card pointer set.
    """

    def test_both_backends_yield_the_same_card(self):
        assert cards_from_payload(SDK_RESPONSE) == cards_from_payload(MCP_RESPONSE)

    def test_both_backends_render_identical_blocks(self):
        sdk = card_message(cards_from_payload(SDK_RESPONSE)[0], channel="C1")
        mcp = card_message(cards_from_payload(MCP_RESPONSE)[0], channel="C1")
        assert sdk == mcp

    def test_empty_cards_falls_back_to_the_top_level_pointer(self):
        # Without this the free tier renders nothing for a chart it did return.
        cards = cards_from_payload(MCP_PRE_187)
        assert len(cards) == 1
        assert pub_id_of(cards[0]) == PUB

    def test_the_fallback_still_produces_a_postable_message(self):
        card = cards_from_payload(MCP_PRE_187)[0]
        body = card_message(card, channel="C1", thread_ts="1754870000.001200")
        assert body is not None
        assert body["blocks"][0]["type"] == "container"
        image = body["blocks"][0]["child_blocks"][0]
        assert image["image_url"] == f"{MCP_PRE_187['image_url']}?dark_mode=false"

    def test_a_response_with_neither_cards_nor_pointer_renders_nothing(self):
        assert cards_from_payload({"cards": [], "web_results": [], "meta": {}}) == []

    def test_web_only_results_are_never_mistaken_for_a_card(self):
        payload = {"cards": [], "web_results": [{"url": "https://example.com", "title": "x"}]}
        assert cards_from_payload(payload) == []


class TestPubId:
    def test_reads_the_id_field(self):
        assert pub_id_of({"card_id": PUB}) == PUB
        assert pub_id_of({"pub_id": PUB}) == PUB

    def test_recovers_the_id_from_any_url(self):
        for key in ("webpage_url", "embed_url", "image_url"):
            path = {"webpage_url": "card", "embed_url": "embed", "image_url": "api/v1/image"}[key]
            assert pub_id_of({key: f"https://tako.com/{path}/{PUB}/"}) == PUB

    def test_rejects_ids_that_would_not_survive_url_interpolation(self):
        for bad in ("../../evil", "a b", "x", "ok/../..", "", None):
            assert pub_id_of({"card_id": bad}) is None

    def test_ignores_lookalike_hosts(self):
        assert pub_id_of({"webpage_url": f"https://tako.com.evil.example/card/{PUB}/"}) is None
        assert pub_id_of({"webpage_url": f"https://nottako.com/card/{PUB}/"}) is None


class TestCardUrls:
    # Every image URL below is pinned to `dark_mode=false`: a Slack message
    # renders on the reader's own theme, so the chart is fixed to light rather
    # than left to Tako's dark default. See `_theme.py`.
    def test_prefers_the_urls_the_api_returned(self):
        card = {**CARD, "image_url": f"https://tako.com/api/v1/image/{PUB}/?dark_mode=true"}
        image, webpage = card_urls(card)
        assert image == f"https://tako.com/api/v1/image/{PUB}/?dark_mode=false"
        assert webpage == f"https://tako.com/card/{PUB}/"

    def test_builds_urls_when_the_card_omits_them(self):
        image, webpage = card_urls({"card_id": PUB})
        assert image == f"https://tako.com/api/v1/image/{PUB}/?dark_mode=false"
        assert webpage == f"https://tako.com/card/{PUB}/"

    def test_replaces_a_non_tako_url(self):
        image, webpage = card_urls({"card_id": PUB, "image_url": "https://evil.example/x.png"})
        assert image == f"https://tako.com/api/v1/image/{PUB}/?dark_mode=false"
        assert webpage == f"https://tako.com/card/{PUB}/"

    def test_returns_none_without_a_usable_id(self):
        assert card_urls({"title": "no id"}) is None


class TestChildBlocks:
    def test_orders_image_then_headline_then_link(self):
        assert [block["type"] for block in card_child_blocks(CARD)] == [
            "image",
            "context",
            "actions",
        ]

    def test_image_carries_alt_text(self):
        image = card_child_blocks(CARD)[0]
        assert image["image_url"] == f"{CARD['image_url']}?dark_mode=false"
        assert image["alt_text"] == CARD["title"]

    def test_headline_reuses_the_card_description_verbatim(self):
        # Tako has already chosen the units, currency, and scaling. Nothing here
        # may rescale, round, or reformat a value, so this pins exact equality:
        # any future reformatting of the headline fails this test.
        context = card_child_blocks(CARD)[1]
        assert context["elements"][0]["type"] == "mrkdwn"
        assert context["elements"][0]["text"] == CARD["description"]

    def test_uses_takos_own_rendered_image_never_a_rebuilt_one(self):
        # The card's image_url is the authoritative render: this module never
        # points at a different chart, and never rescales or re-renders one. The
        # only thing it may rewrite is the `dark_mode` theme parameter, so the
        # host, path, and card id must survive untouched.
        card = {**CARD, "image_url": f"https://tako.com/api/v1/image/{PUB}/?dark_mode=true"}
        rendered = card_child_blocks(card)[0]["image_url"]
        assert rendered.split("?")[0] == card["image_url"].split("?")[0]
        assert rendered == f"https://tako.com/api/v1/image/{PUB}/?dark_mode=false"

    def test_omits_the_headline_when_there_is_no_description(self):
        card = {key: value for key, value in CARD.items() if key != "description"}
        assert [block["type"] for block in card_child_blocks(card)] == ["image", "actions"]

    def test_button_links_to_the_live_card(self):
        actions = card_child_blocks(CARD)[-1]
        button = actions["elements"][0]
        assert button["url"] == f"https://tako.com/card/{PUB}/"
        assert button["text"] == {"type": "plain_text", "text": "Open in Tako"}

    def test_returns_nothing_for_an_unrenderable_card(self):
        assert card_child_blocks({"title": "no id"}) == []


class TestLayouts:
    def test_container_is_full_width_with_text_object_title(self):
        [container] = card_blocks(CARD, "container")
        assert container["type"] == "container"
        assert container["width"] == "full"
        # A bare string here is rejected by Slack; it must be a text object.
        assert container["title"] == {"type": "plain_text", "text": CARD["title"]}
        assert container["subtitle"] == {"type": "plain_text", "text": "Fiscal.ai via Tako"}

    def test_container_omits_subtitle_without_sources(self):
        card = {key: value for key, value in CARD.items() if key != "sources"}
        [container] = card_blocks(card, "container")
        assert "subtitle" not in container

    def test_container_caps_its_children(self):
        [container] = card_blocks(CARD, "container")
        assert len(container["child_blocks"]) <= MAX_CONTAINER_CHILDREN

    def test_flat_returns_the_same_children_at_top_level(self):
        assert card_blocks(CARD, "flat") == card_child_blocks(CARD)

    def test_both_layouts_are_empty_for_an_unrenderable_card(self):
        assert card_blocks({"title": "no id"}, "container") == []
        assert card_blocks({"title": "no id"}, "flat") == []


class TestCardMessage:
    def test_always_sets_the_notification_fallback_text(self):
        body = card_message(CARD)
        assert body["text"] == CARD["title"]

    def test_includes_channel_and_thread_when_given(self):
        body = card_message(CARD, channel="C0456DEF", thread_ts="1754870000.001200")
        assert body["channel"] == "C0456DEF"
        assert body["thread_ts"] == "1754870000.001200"

    def test_omits_channel_and_thread_when_absent(self):
        body = card_message(CARD)
        assert "channel" not in body
        assert "thread_ts" not in body

    def test_honors_the_flat_layout(self):
        body = card_message(CARD, layout="flat")
        assert [block["type"] for block in body["blocks"]] == ["image", "context", "actions"]

    def test_returns_none_for_an_unrenderable_card(self):
        assert card_message({"title": "no id"}) is None


class TestClipping:
    def test_title_is_clipped(self):
        title = card_title({"title": "x" * 400})
        assert len(title) == MAX_TITLE_CHARS
        assert title.endswith("…")

    def test_title_whitespace_is_collapsed(self):
        assert card_title({"title": "NVIDIA   Total\nRevenues"}) == "NVIDIA Total Revenues"

    def test_source_deduplicates_names(self):
        card = {
            "card_id": PUB,
            "sources": [{"source_name": "Fiscal.ai"}, {"source_name": "Fiscal.ai"}],
        }
        assert card_source(card) == "Fiscal.ai via Tako"

    def test_source_accepts_plain_strings(self):
        assert card_source({"card_id": PUB, "sources": ["FRED", "OECD"]}) == "FRED, OECD via Tako"

    def test_source_is_none_without_sources(self):
        assert card_source({"card_id": PUB}) is None
        assert card_source({"card_id": PUB, "sources": []}) is None
