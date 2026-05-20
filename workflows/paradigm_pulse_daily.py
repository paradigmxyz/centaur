"""Workflow: daily Paradigm Pulse digest.

Posts to #paradigm-pulse every morning at 7:45am PT.
"""

from __future__ import annotations

import re
from typing import TYPE_CHECKING, Any
from urllib.parse import urlparse

if TYPE_CHECKING:
    from api.workflow_engine import WorkflowContext

WORKFLOW_NAME = "paradigm_pulse_daily"
CRON = "45 7 * * *"
SLACK_CHANNEL = "paradigm-pulse"

_MAX_BLOCK_TEXT = 2900
_MARKDOWN_LINK_RE = re.compile(r"\[([^\]]+)\]\((https?://[^\s]+)\)")
_SLACK_LINK_RE = re.compile(r"<(https?://[^>|]+)\|([^>]+)>")
_ANGLE_LINK_RE = re.compile(r"<(https?://[^>|]+)>")
_BARE_URL_RE = re.compile(r"(?<!<)(https?://[^\s<>\]]+)")

TRACKED_PORTFOLIO_SIGNALS = """
These tracking seeds are additive. They must not replace the existing crypto,
market, Paradigm team, holdings, and influential-circle search scope.

The daily digest should be a mix of everything we track, ranked by what is
most prominent and actually trending that day. Do not mechanically fill from
one list, and do not let the additional seeds crowd out higher-signal crypto,
market, team, portfolio, or influential-circle items.

When searching, preserve coverage for core crypto-native signals, including
Hyperliquid/HYPE, Monad/MON, Trade.xyz, MPP/Tempo, Ethereum/ETH, Bitcoin/BTC,
Solana/SOL, Uniswap/UNI, Optimism/OP, Zcash/ZEC, market structure/policy,
DeFi/security incidents, and major crypto company or protocol news. Include
these when they are among the day's most prominent signals.

Also include these additional tracking seeds when searching for
portfolio/company momentum and social signal:
- SendCutSend, Jim Belosic, https://x.com/sendcutsend, https://x.com/jimbelosic
- True Anomaly, Even Rogers, https://x.com/The_TrueAnomaly
- Zipline, Keller Cliffton, https://x.com/zipline, https://x.com/Keller
- Standard Economics, Evan Jones, https://x.com/standardecon, https://x.com/evanjones
- Draftea, Alán Jaime Misrahi, https://x.com/Draftea_Mexico
- Revolut, Nik Storonsky, https://x.com/Revolut
- Talarion, Nick Hope, https://x.com/TalarionTech
- USDM1 / M1X Global, Mark Lurie, https://x.com/M1XGlobal, https://x.com/MarkLurie
- Kalshi, Tarek Mansour, Luana Lopes Lara, https://x.com/Kalshi,
  https://x.com/mansourtarek_, https://x.com/luanalopeslara
- Tempo, https://tempo.xyz/
- Additional supplied X seed: https://x.com/jollyrogersta

Dan Robinson is Paradigm & Team, not Influential Circles.
""".strip()

FUNDING_NEWS_SWEEP = """
Mandatory funding/news sweep:
- Run a broad daily search for Paradigm-linked financing news before ranking
  the digest: `Paradigm funding`, `Paradigm raised`, `Paradigm co-led`,
  `site:techmeme.com Paradigm`, and `site:prnewswire.com Paradigm`.
- For tracked non-crypto portfolio companies, search each company name with
  financing terms: `raised`, `funding`, `financing`, `valued`, `valuation`,
  `co-led`, `led by`, `Series`, and `round`.
- This sweep must include at least SendCutSend, True Anomaly, Zipline,
  Standard Economics, Draftea, Revolut, Talarion, USDM1/M1X Global, Kalshi,
  and Tempo.
- If a current-date or recent article says Paradigm invested, co-led, led, or
  participated in a financing round, promote that item to News ahead of lower
  signal X/social/trending items.
- Do not rely on X-only searches for funding coverage; use web/news sources
  such as Techmeme, company press releases, PRNewswire, Fortune, TechCrunch,
  The Information, Bloomberg, Reuters, or other primary/reputable coverage.
""".strip()

PROMPT = (
    "Generate today's Paradigm Pulse digest for Paradigm I&R and "
    "Marketing. Use Centaur tools to gather fresh signals across "
    "Paradigm mentions, Paradigm team activity, portfolio company "
    "momentum, relevant market/news signals, and notable "
    "influential-circle content.\n\n"
    "Output concise Slack-ready markdown with these sections when "
    "there is signal:\n"
    "- News\n"
    "- Trending\n"
    "- Paradigm & Team\n"
    "- Holdings\n"
    "- Influential Circles\n\n"
    "Avoid low-signal filler. Reuse the existing thread context to "
    "avoid repeating items that were already posted recently unless "
    "they changed materially. Write in clean prose, not a link dump. "
    "Use embedded hyperlinks with source names or short descriptors, "
    "never full raw URLs in the body, and do not end with a separate "
    "list of links.\n\n"
    f"{TRACKED_PORTFOLIO_SIGNALS}"
    "\n\n"
    f"{FUNDING_NEWS_SWEEP}"
)


def _format_slack_link(url: str, label: str) -> str:
    return f"<{url}|{label}>"


def _label_for_url(url: str) -> str:
    parsed = urlparse(url)
    host = parsed.netloc.lower().removeprefix("www.")
    path_parts = [part for part in parsed.path.split("/") if part]

    if host in {"x.com", "twitter.com"} and path_parts:
        return f"@{path_parts[0]}"
    if path_parts:
        return f"{host}/{path_parts[-1]}"
    return host


def _replace_bare_url(match: re.Match[str]) -> str:
    original = match.group(1)
    url, suffix = _split_trailing_url_suffix(original)
    return f"{_format_slack_link(url, _label_for_url(url))}{suffix}"


def _split_trailing_url_suffix(text: str) -> tuple[str, str]:
    url = text
    suffix = ""

    while url and url[-1] in ".,;:!?":
        suffix = url[-1] + suffix
        url = url[:-1]

    while url.endswith(")") and url.count("(") < url.count(")"):
        suffix = ")" + suffix
        url = url[:-1]

    return url, suffix


def _normalize_label(url: str, label: str) -> str:
    cleaned = label.strip().strip("<>")
    if cleaned.startswith(("http://", "https://")):
        return _label_for_url(url)
    return cleaned


def _slackify_links(text: str) -> str:
    """Convert markdown and bare URLs into Slack mrkdwn hyperlinks."""

    converted = _MARKDOWN_LINK_RE.sub(
        lambda match: _format_slack_link(
            *_split_markdown_link(match.group(1), match.group(2))
        ),
        text,
    )
    converted = _SLACK_LINK_RE.sub(
        lambda match: _format_slack_link(match.group(1), _normalize_label(match.group(1), match.group(2))),
        converted,
    )
    converted = _ANGLE_LINK_RE.sub(
        lambda match: _format_slack_link(match.group(1), _label_for_url(match.group(1))),
        converted,
    )
    return _BARE_URL_RE.sub(_replace_bare_url, converted)


def _split_markdown_link(label: str, url: str) -> tuple[str, str]:
    trimmed_url, _suffix = _split_trailing_url_suffix(url)
    return trimmed_url, _normalize_label(trimmed_url, label)


def _section_block(text: str) -> dict[str, Any]:
    return {
        "type": "section",
        "text": {"type": "mrkdwn", "text": text.strip(), "verbatim": True},
    }


def _build_blocks(text: str) -> list[dict[str, Any]]:
    """Render the digest as Block Kit so Slack won't unfurl article links."""

    blocks: list[dict[str, Any]] = []
    chunk = ""

    for line in text.splitlines():
        candidate = line if not chunk else f"{chunk}\n{line}"
        if len(candidate) <= _MAX_BLOCK_TEXT:
            chunk = candidate
            continue

        if chunk.strip():
            blocks.append(_section_block(chunk))

        if len(line) <= _MAX_BLOCK_TEXT:
            chunk = line
            continue

        for start in range(0, len(line), _MAX_BLOCK_TEXT):
            piece = line[start : start + _MAX_BLOCK_TEXT].strip()
            if piece:
                blocks.append(_section_block(piece))
        chunk = ""

    if chunk.strip():
        blocks.append(_section_block(chunk))

    return blocks


async def handler(inp: dict[str, Any], ctx: WorkflowContext) -> dict[str, Any]:
    channel = inp.get("slack_channel") or SLACK_CHANNEL

    result = await ctx.agent_turn(PROMPT)
    text = str(result.get("result_text") or "").strip()
    if not text:
        return result

    slack_text = _slackify_links(text)
    args: dict[str, Any] = {
        "channel": channel,
        "text": slack_text,
        "no_attribution": True,
        "blocks": _build_blocks(slack_text),
        "unfurl_links": False,
        "unfurl_media": False,
    }
    await ctx.call_tool("slack", "send_message", args)
    result["slack_text"] = slack_text
    return result
