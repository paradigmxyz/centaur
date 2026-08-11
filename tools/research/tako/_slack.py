"""Render a Tako search card as a Slack message.

Presentation only. Every card in a search response already carries the rendered
chart (`image_url`), so turning one into a Slack message needs no extra API call
and no auth beyond `chat:write` on the posting side.

Always use the card's own `image_url`. Tako has already chosen the axes, units,
currency, and time spacing for that series, so reusing its render is both free and
the only way to be certain the units are right. Nothing in this module reads,
scales, rounds, or reformats a value: the headline is the card's own
`description`, passed through with whitespace collapsed and nothing else. Keep it
that way. If a caller needs different framing, ask Tako for a different card
rather than re-deriving numbers here.

Creating a visual (`POST /api/v1/thin_viz/create/`) is deliberately absent. It
belongs here only for genuinely novel data that Centaur holds itself and no Tako
card covers. If that path is ever added: pass raw values with their units
declared rather than pre-scaling them, and never resample a series to make it fit
a chart. Both shortcuts silently change what the chart claims.

Why `image` blocks rather than an iframe: Slack scales an image block to the
message width and preserves its aspect ratio without cropping, so there is no
geometry to fit. The one Slack surface that renders live HTML (the `video` block)
fixes the frame to a small box with no size controls, crops taller cards, loads
only on click, is dropped from ephemeral messages, and needs `links.embed:write`
on both tokens plus a registered unfurl domain. Image blocks avoid all of it.

The headline numbers and the "Open in Tako" button sit outside the image, so they
cost no image height, and interactivity is delegated to the button: the live card
opens in a browser with working range tabs, tooltips, and refetch.

Slack answers every post containing an image block with the warning
`ignored_extra_attributes_for_image_block`. It is benign and unavoidable: the
minimal legal block (`image_url` + `alt_text`) produces it, and dropping
`alt_text` to silence it fails outright with `missing required field: alt_text`.
Both measured against the live API on 2026-08-11. Do not chase it.

Verified against production on 2026-08-11:
- `/api/v1/image/<pub_id>/` answers anonymously (200 image/png), which is what
  lets Slack's image fetcher retrieve it.
- Images are 2400px wide, so they stay sharp on retina displays.
- The image already renders dark. `?dark_mode=true` returns byte-identical
  output, so this module passes the card's own `image_url` through untouched
  rather than appending a parameter that does nothing.
"""

from __future__ import annotations

import os
import re
from dataclasses import dataclass
from typing import Any, Literal

TAKO_HOST = "tako.com"

#: Slack rejects a `container` holding more than this many children.
MAX_CONTAINER_CHILDREN = 10

#: Slack truncates hard past this; clip so the text object stays valid.
MAX_TITLE_CHARS = 150
MAX_ALT_TEXT_CHARS = 1000
MAX_CONTEXT_CHARS = 2000
MAX_FALLBACK_TEXT_CHARS = 300

#: A card's public id, as it appears in every card/embed/image URL. Card ids can
#: reach this module from model output, so they are validated before being
#: interpolated into any URL.
PUB_ID = re.compile(r"^[A-Za-z0-9_-]{4,64}$")

#: `CENTAUR_THREAD_KEY`, e.g. `slack:T0123ABC:C0456DEF:1754870000.001200`. The
#: team segment is optional; non-Slack sources (Discord, Teams, direct API) do
#: not match and yield no target.
THREAD_KEY = re.compile(
    r"^(?P<source>[A-Za-z][A-Za-z0-9_.-]*):"
    r"(?:(?P<team>T[A-Z0-9]+):)?"
    r"(?P<channel>[CDG][A-Z0-9]+):"
    r"(?P<thread_ts>\d{10}\.\d{1,6})$"
)

Layout = Literal["container", "flat"]


@dataclass(frozen=True)
class ThreadTarget:
    """Where in Slack a card should be posted."""

    channel: str
    thread_ts: str
    team: str | None = None


def thread_target(thread_key: str | None = None) -> ThreadTarget | None:
    """Resolve the Slack thread the current sandbox turn belongs to.

    Reads `CENTAUR_THREAD_KEY` when no key is given. Returns None when the key is
    absent, malformed, or belongs to a non-Slack source, so callers can degrade
    to printing the payload instead of failing.
    """
    # CENTAUR_THREAD_KEY is thread identity, not a secret: the session runtime
    # sets it on the sandbox spec, so it is a plain env read like the proxy and
    # CA-bundle lookups in client.py.
    if thread_key is None:
        thread_key = os.environ.get("CENTAUR_THREAD_KEY", "")  # noqa: TID251
    raw = thread_key.strip()
    if not raw:
        return None
    match = THREAD_KEY.match(raw)
    if not match or match.group("source").lower() != "slack":
        return None
    return ThreadTarget(
        channel=match.group("channel"),
        thread_ts=match.group("thread_ts"),
        team=match.group("team"),
    )


def cards_from_payload(payload: Any) -> list[dict[str, Any]]:
    """Pull the renderable cards out of whatever a caller piped in.

    Accepts a whole `search`/`answer` response (`{"cards": [...]}`), a bare list
    of cards, or a single card object, so a caller can pipe the tool's own output
    straight in without reshaping it. Cards that carry no usable id are dropped.

    Both backends render the same way. The paid SDK path returns the whole
    /v3/search body and the free MCP path rebuilds an equivalent one, and either
    can come back with `cards` empty while still naming its lead chart in the
    top-level `pub_id`/`image_url`/`embed_url` pointer (the pre-187 MCP shape does
    exactly that). So an empty `cards` falls back to that pointer rather than
    rendering nothing on the free tier for a chart the paid tier would show.
    """
    if isinstance(payload, dict):
        candidates = payload.get("cards")
        if not isinstance(candidates, list):
            candidates = [payload]
    elif isinstance(payload, list):
        candidates = payload
    else:
        return []
    cards = [card for card in candidates if isinstance(card, dict) and pub_id_of(card)]
    if not cards and isinstance(payload, dict) and pub_id_of(payload):
        cards = [payload]
    return cards


def pub_id_of(card: dict[str, Any]) -> str | None:
    """The card's public id, from the id field or recovered from any of its URLs."""
    direct = str(card.get("card_id") or card.get("pub_id") or "").strip()
    if PUB_ID.match(direct):
        return direct
    for key in ("webpage_url", "embed_url", "image_url"):
        found = _pub_id_from_url(str(card.get(key) or ""))
        if found:
            return found
    return None


def _pub_id_from_url(url: str) -> str | None:
    match = re.search(
        rf"https?://(?:www\.)?{re.escape(TAKO_HOST)}/(?:card|embed|api/v1/image)/([A-Za-z0-9_-]{{4,64}})/?",
        url,
    )
    return match.group(1) if match else None


def card_urls(card: dict[str, Any]) -> tuple[str, str] | None:
    """The (image, webpage) URLs for a card.

    Prefers the URLs the API returned, since they already carry whatever query
    parameters the search was run with, and falls back to building them from the
    card's public id.
    """
    pub_id = pub_id_of(card)
    if not pub_id:
        return None
    image = str(card.get("image_url") or "").strip()
    webpage = str(card.get("webpage_url") or "").strip()
    if not _is_tako_https(image):
        image = f"https://{TAKO_HOST}/api/v1/image/{pub_id}/"
    if not _is_tako_https(webpage):
        webpage = f"https://{TAKO_HOST}/card/{pub_id}/"
    return image, webpage


def _is_tako_https(url: str) -> bool:
    return url.startswith(f"https://{TAKO_HOST}/") or url.startswith(f"https://www.{TAKO_HOST}/")


def card_title(card: dict[str, Any]) -> str:
    return _clip(str(card.get("title") or "Tako data card"), MAX_TITLE_CHARS)


def card_source(card: dict[str, Any]) -> str | None:
    """ "S&P Global via Tako"-style attribution built from the card's sources."""
    sources = card.get("sources")
    names: list[str] = []
    if isinstance(sources, list):
        for source in sources:
            name = (
                str(source.get("source_name") or "").strip()
                if isinstance(source, dict)
                else str(source or "").strip()
            )
            if name and name not in names:
                names.append(name)
    if not names:
        return None
    return _clip(f"{', '.join(names)} via Tako", MAX_TITLE_CHARS)


def card_child_blocks(card: dict[str, Any]) -> list[dict[str, Any]]:
    """The image, the headline numbers, and the link out, in that order.

    The context line reuses the card's own `description`, which search responses
    already populate with a written headline (latest value, change, market cap),
    so nothing extra is fetched to fill it.
    """
    urls = card_urls(card)
    if not urls:
        return []
    image_url, webpage_url = urls
    title = card_title(card)

    blocks: list[dict[str, Any]] = [
        {
            "type": "image",
            "image_url": image_url,
            "alt_text": _clip(str(card.get("alt_text") or title), MAX_ALT_TEXT_CHARS),
        }
    ]

    description = _one_line(str(card.get("description") or ""))
    if description:
        blocks.append(
            {
                "type": "context",
                "elements": [{"type": "mrkdwn", "text": _clip(description, MAX_CONTEXT_CHARS)}],
            }
        )

    blocks.append(
        {
            "type": "actions",
            "elements": [
                {
                    "type": "button",
                    "text": {"type": "plain_text", "text": "Open in Tako"},
                    "url": webpage_url,
                }
            ],
        }
    )
    return blocks


def card_blocks(card: dict[str, Any], layout: Layout = "container") -> list[dict[str, Any]]:
    """Blocks for one card.

    `container` groups the image with its title, subtitle, and controls in a
    single full-width unit. `flat` returns the same children as top-level blocks,
    using only blocks that have been generally available for years; it is the
    fallback when `container` rendering is a problem on any client.
    """
    children = card_child_blocks(card)
    if not children:
        return []
    if layout == "flat":
        return children

    container: dict[str, Any] = {
        "type": "container",
        "width": "full",
        # Slack's docs describe these as strings, but the API rejects a bare
        # string ("failed to match all allowed schemas"). They are text objects.
        "title": {"type": "plain_text", "text": card_title(card)},
        "child_blocks": children[:MAX_CONTAINER_CHILDREN],
    }
    subtitle = card_source(card)
    if subtitle:
        container["subtitle"] = {"type": "plain_text", "text": subtitle}
    return [container]


def card_message(
    card: dict[str, Any],
    *,
    channel: str | None = None,
    thread_ts: str | None = None,
    layout: Layout = "container",
) -> dict[str, Any] | None:
    """A complete `chat.postMessage` body for one card, or None if unrenderable.

    The top-level `text` is required, not decorative: it is what Slack shows in
    notifications and on clients that cannot render the blocks.
    """
    blocks = card_blocks(card, layout)
    if not blocks:
        return None
    body: dict[str, Any] = {
        "text": _clip(card_title(card), MAX_FALLBACK_TEXT_CHARS),
        "blocks": blocks,
    }
    if channel:
        body["channel"] = channel
    if thread_ts:
        body["thread_ts"] = thread_ts
    return body


class SlackPostError(RuntimeError):
    """Slack refused the post, or could not be reached."""


def post_message(
    body: dict[str, Any],
    *,
    token: str | None = None,
    transport: Any = None,
    timeout_seconds: float = 20.0,
) -> dict[str, Any]:
    """Post a prepared message with `chat.postMessage`.

    Posts directly rather than delegating to the `slack` tool, because tools run
    under `uvx --from <project_dir>`: each gets an isolated environment holding
    only its own project and declared dependencies, so a sibling tool's package
    is not importable. httpx is already a dependency here, and it honors
    HTTPS_PROXY and SSL_CERT_FILE from the environment, so this needs no explicit
    iron-proxy wiring (same reasoning as the MCP backend).

    The token is read through `secret()`, which inside a sandbox hands back a
    placeholder that iron-proxy swaps for the real value on the Authorization
    header. That swap only happens because this tool declares SLACK_BOT_TOKEN
    against slack.com in its `[tool.centaur]` manifest.

    `transport` is a test seam for httpx.MockTransport.

    Raises SlackPostError on a transport failure, a non-2xx, or `ok: false`.
    """
    import httpx

    if token is None:
        from centaur_sdk import secret

        token = secret("SLACK_BOT_TOKEN", default="")
    token = (token or "").strip()
    if not token:
        raise SlackPostError(
            "SLACK_BOT_TOKEN is not configured, so the card cannot be posted. "
            "Drop --post to print the payload instead."
        )

    try:
        with httpx.Client(timeout=timeout_seconds, transport=transport) as client:
            response = client.post(
                "https://slack.com/api/chat.postMessage",
                json=body,
                headers={
                    "Authorization": f"Bearer {token}",
                    "Content-Type": "application/json; charset=utf-8",
                },
            )
    except httpx.HTTPError as exc:
        raise SlackPostError(f"could not reach Slack: {exc}") from exc

    if response.status_code == 429:
        # Not retried here: one card per answer is well inside Slack's limits,
        # and a silent retry would hide a real problem behind a delay.
        retry_after = response.headers.get("Retry-After", "unknown")
        raise SlackPostError(f"Slack rate-limited the post; retry after {retry_after}s")
    if response.status_code >= 400:
        raise SlackPostError(f"Slack returned HTTP {response.status_code}")

    try:
        payload = response.json()
    except ValueError as exc:
        raise SlackPostError("Slack returned a non-JSON response") from exc

    if not payload.get("ok"):
        raise SlackPostError(_post_error_message(payload))
    return payload


def _post_error_message(payload: dict[str, Any]) -> str:
    error = str(payload.get("error") or "unknown_error")
    hints = {
        "not_in_channel": "invite the bot to the channel first (/invite)",
        "channel_not_found": (
            "the channel is private or the bot is not a member; invite it, or "
            "grant chat:write.public for public channels"
        ),
        "invalid_blocks": "Slack rejected the blocks; try --flat",
        "missing_scope": "the bot token needs chat:write",
        "invalid_auth": "the bot token is invalid or revoked",
    }
    detail = hints.get(error)
    # Slack names the offending block here, which is the only way to find it.
    messages = (payload.get("response_metadata") or {}).get("messages") or []
    parts = [f"Slack rejected the post: {error}"]
    if detail:
        parts.append(f"({detail})")
    if messages:
        parts.append("; ".join(str(message) for message in messages))
    return " ".join(parts)


def _one_line(value: str) -> str:
    return re.sub(r"\s+", " ", value).strip()


def _clip(value: str, limit: int) -> str:
    collapsed = _one_line(value)
    return collapsed if len(collapsed) <= limit else f"{collapsed[: limit - 1]}…"
