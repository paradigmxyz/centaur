"""Unit tests for pinning Tako card renderings to light mode. No network."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from tools.research.tako._theme import apply_light_mode, light_mode_markdown, light_mode_url

PUB = "m-p-JX5qxsBStJgywrqI"


class TestLightModeUrl:
    def test_adds_the_parameter_when_absent(self):
        assert (
            light_mode_url(f"https://tako.com/api/v1/image/{PUB}/")
            == f"https://tako.com/api/v1/image/{PUB}/?dark_mode=false"
        )

    def test_overrides_dark_mode_true(self):
        assert (
            light_mode_url(f"https://tako.com/api/v1/image/{PUB}/?dark_mode=true")
            == f"https://tako.com/api/v1/image/{PUB}/?dark_mode=false"
        )

    def test_overrides_dark_mode_auto(self):
        assert (
            light_mode_url(f"https://tako.com/embed/{PUB}/?dark_mode=auto")
            == f"https://tako.com/embed/{PUB}/?dark_mode=false"
        )

    def test_is_idempotent(self):
        once = light_mode_url(f"https://tako.com/embed/{PUB}/?dark_mode=auto")
        assert light_mode_url(once) == once

    def test_keeps_other_query_parameters(self):
        out = light_mode_url(f"https://tako.com/embed/{PUB}/?width=800&dark_mode=auto&height=400")
        assert "width=800" in out
        assert "height=400" in out
        assert "dark_mode=false" in out
        assert "dark_mode=auto" not in out

    def test_collapses_a_repeated_dark_mode_parameter(self):
        out = light_mode_url(f"https://tako.com/embed/{PUB}/?dark_mode=true&dark_mode=auto")
        assert out.count("dark_mode=") == 1
        assert "dark_mode=false" in out

    def test_accepts_the_www_host(self):
        assert (
            light_mode_url(f"https://www.tako.com/embed/{PUB}/")
            == f"https://www.tako.com/embed/{PUB}/?dark_mode=false"
        )

    def test_leaves_a_non_tako_url_untouched(self):
        url = "https://example.com/chart.png"
        assert light_mode_url(url) == url

    def test_does_not_match_a_lookalike_host(self):
        url = "https://tako.com.evil.test/api/v1/image/x/"
        assert light_mode_url(url) == url

    def test_leaves_a_blank_value_untouched(self):
        assert light_mode_url("") == ""
        assert light_mode_url(None) is None


CARD = {
    "card_id": PUB,
    "title": "NVIDIA Corporation Total Revenues (Normalized) (Annual)",
    "webpage_url": f"https://tako.com/card/{PUB}/",
    "image_url": f"https://tako.com/api/v1/image/{PUB}/?dark_mode=true",
    "embed_url": f"https://tako.com/embed/{PUB}/?dark_mode=auto",
}

PAYLOAD = {
    "query": "nvidia revenue",
    "cards": [CARD],
    "web_results": [{"title": "NVIDIA", "url": "https://example.com/nvidia?dark_mode=true"}],
    # The MCP shape also names the lead card at the top level.
    "pub_id": PUB,
    "image_url": f"https://tako.com/api/v1/image/{PUB}/?dark_mode=true",
    "embed_url": f"https://tako.com/embed/{PUB}/?dark_mode=auto",
    "dark_mode": True,
}


class TestApplyLightMode:
    def test_rewrites_card_renderings(self):
        out = apply_light_mode(PAYLOAD)
        assert out["cards"][0]["image_url"].endswith("?dark_mode=false")
        assert out["cards"][0]["embed_url"].endswith("?dark_mode=false")

    def test_rewrites_the_top_level_card_pointer(self):
        out = apply_light_mode(PAYLOAD)
        assert out["image_url"].endswith("?dark_mode=false")
        assert out["embed_url"].endswith("?dark_mode=false")

    def test_clears_the_dark_mode_flag_the_mcp_reports(self):
        assert apply_light_mode(PAYLOAD)["dark_mode"] is False

    def test_leaves_webpage_url_alone(self):
        # The live card on tako.com follows the viewer's own theme; only the
        # renderings this tool embeds are pinned.
        assert apply_light_mode(PAYLOAD)["cards"][0]["webpage_url"] == CARD["webpage_url"]

    def test_leaves_web_results_alone(self):
        out = apply_light_mode(PAYLOAD)
        assert out["web_results"] == PAYLOAD["web_results"]

    def test_does_not_mutate_the_input(self):
        before = f"https://tako.com/api/v1/image/{PUB}/?dark_mode=true"
        apply_light_mode(PAYLOAD)
        assert PAYLOAD["image_url"] == before
        assert PAYLOAD["cards"][0]["image_url"] == before
        assert PAYLOAD["dark_mode"] is True

    def test_preserves_untouched_keys(self):
        out = apply_light_mode(PAYLOAD)
        assert out["query"] == "nvidia revenue"
        assert out["cards"][0]["title"] == CARD["title"]
        assert out["pub_id"] == PUB

    def test_tolerates_a_missing_cards_key(self):
        assert apply_light_mode({"answer": "42"}) == {"answer": "42"}

    def test_tolerates_a_non_dict_card_entry(self):
        out = apply_light_mode({"cards": [None, "junk", CARD]})
        assert out["cards"][:2] == [None, "junk"]
        assert out["cards"][2]["image_url"].endswith("?dark_mode=false")

    def test_tolerates_a_non_list_cards_value(self):
        assert apply_light_mode({"cards": "junk"}) == {"cards": "junk"}

    def test_passes_a_non_dict_payload_through(self):
        assert apply_light_mode(None) is None
        assert apply_light_mode([1, 2]) == [1, 2]


class TestLightModeMarkdown:
    """`answer_markdown` embeds chart URLs as text, so it needs the same pass.

    The SDK path renders `chart: <image_url>` itself; the keyless path passes
    through the hosted tool's own document, which names each card's image URL
    with no `dark_mode` at all. Either way a reader following that URL would get
    a dark chart while `cards[]` said light.
    """

    def test_pins_a_bare_image_url(self):
        md = f"chart: https://tako.com/api/v1/image/{PUB}/"
        assert (
            light_mode_markdown(md)
            == f"chart: https://tako.com/api/v1/image/{PUB}/?dark_mode=false"
        )

    def test_pins_an_already_dark_image_url(self):
        md = f"chart: https://tako.com/api/v1/image/{PUB}/?dark_mode=true"
        assert "dark_mode=false" in light_mode_markdown(md)
        assert "dark_mode=true" not in light_mode_markdown(md)

    def test_pins_an_embed_url(self):
        md = f"embed: https://tako.com/embed/{PUB}/?dark_mode=auto"
        assert light_mode_markdown(md) == f"embed: https://tako.com/embed/{PUB}/?dark_mode=false"

    def test_leaves_the_live_card_link_alone(self):
        md = f"see https://tako.com/card/{PUB}/ for the live chart"
        assert light_mode_markdown(md) == md

    def test_leaves_web_result_links_alone(self):
        md = "1. [Apple](https://example.com/apple?dark_mode=true)"
        assert light_mode_markdown(md) == md

    def test_survives_a_markdown_link_without_eating_the_paren(self):
        md = f"[chart](https://tako.com/api/v1/image/{PUB}/)"
        assert (
            light_mode_markdown(md)
            == f"[chart](https://tako.com/api/v1/image/{PUB}/?dark_mode=false)"
        )

    def test_rewrites_every_occurrence(self):
        md = (
            f"a https://tako.com/api/v1/image/{PUB}/ b https://tako.com/embed/{PUB}/?dark_mode=true"
        )
        assert light_mode_markdown(md).count("dark_mode=false") == 2

    def test_passes_through_blank_and_non_string(self):
        assert light_mode_markdown("") == ""
        assert light_mode_markdown(None) is None

    def test_apply_light_mode_pins_the_markdown_field(self):
        payload = {
            "answer_markdown": f"chart: https://tako.com/api/v1/image/{PUB}/?dark_mode=true",
            "cards": [CARD],
        }
        out = apply_light_mode(payload)
        assert "dark_mode=false" in out["answer_markdown"]
        assert "dark_mode=true" not in out["answer_markdown"]

    def test_apply_light_mode_pins_the_answer_field(self):
        # `_mcp.answer` falls back to the whole text channel on servers predating
        # tako-mcp#187, and that document names image URLs.
        payload = {"answer": f"see https://tako.com/api/v1/image/{PUB}/", "cards": []}
        assert apply_light_mode(payload)["answer"].endswith("?dark_mode=false")

    def test_a_plain_answer_is_left_alone(self):
        payload = {"answer": "Apple's fiscal 2025 revenue was $416.2 billion."}
        assert apply_light_mode(payload)["answer"] == payload["answer"]

    def test_markdown_and_cards_agree(self):
        # The failure this guards: cards[] light, answer_markdown dark.
        payload = {
            "answer_markdown": f"chart: https://tako.com/api/v1/image/{PUB}/",
            "cards": [{"card_id": PUB, "image_url": f"https://tako.com/api/v1/image/{PUB}/"}],
        }
        out = apply_light_mode(payload)
        assert out["cards"][0]["image_url"] in out["answer_markdown"]


class TestRenderedCardIsLight:
    """The renderer is the last gate: no dark image may reach a Slack message.

    `apply_light_mode` covers cards that came back from a search, but a card can
    also be built from a bare pub id (`datasearch slack-card <id>`), which has no
    `image_url` for the payload pass to rewrite. That URL is reconstructed inside
    the renderer, so it needs pinning there too.
    """

    def test_a_card_built_from_a_bare_pub_id_renders_light(self):
        from tools.research.tako._slack import card_urls

        image, _ = card_urls({"card_id": PUB})
        assert image == f"https://tako.com/api/v1/image/{PUB}/?dark_mode=false"

    def test_a_dark_card_url_is_pinned_light_by_the_renderer(self):
        from tools.research.tako._slack import card_urls

        image, _ = card_urls(
            {"card_id": PUB, "image_url": f"https://tako.com/api/v1/image/{PUB}/?dark_mode=true"}
        )
        assert image.endswith("?dark_mode=false")

    def test_the_open_in_tako_link_is_left_alone(self):
        from tools.research.tako._slack import card_urls

        _, webpage = card_urls(CARD)
        assert webpage == CARD["webpage_url"]
        assert "dark_mode" not in webpage

    def test_the_slack_image_block_carries_the_light_url(self):
        from tools.research.tako._slack import card_message

        body = card_message({"card_id": PUB}, layout="flat")
        images = [b for b in body["blocks"] if b.get("type") == "image"]
        assert images, "expected an image block"
        assert images[0]["image_url"].endswith("?dark_mode=false")
