"""Pin Tako card renderings to light mode.

Tako hands back card URLs pre-stamped for a dark canvas: `image_url` carries
`?dark_mode=true` and `embed_url` carries `?dark_mode=auto`. Both are wrong for
this tool's consumers, which render a card into a surface whose background they
do not control. Slack is the case that forced this: a message renders on each
viewer's own theme, so a dark PNG sits in a light thread as a black box, and
`auto` resolves against the *fetcher's* environment rather than the reader's,
which is to say arbitrarily. One fixed appearance beats a coin flip, and light
is the safe one -- it reads on a light background and on a dark one.

Applied at the response boundary rather than in any single renderer so every
consumer inherits it: a card pointer that leaves this tool is already light, and
a new surface needs no theme code of its own.

Scope: presentation only. This rewrites the `dark_mode` query parameter on Tako
image/embed URLs and changes nothing else. `webpage_url` is deliberately left
alone -- that link opens the live card on tako.com, which is Tako's own property
and should follow the visitor's own theme preference.

Verified against the live renderer on 2026-08-11 (card vjvljW2ZZ3ImEsq1P-nI):
`/api/v1/image/<id>/` with no parameter and with `dark_mode=true` returns
byte-identical dark PNGs, while `dark_mode=false` returns a genuinely light one;
`/embed/<id>/` likewise distinguishes `true` from `false`, and serves `auto` as
`false`. So the parameter is honored today -- note that a *missing* parameter
means dark, which is why this adds the parameter rather than only overwriting an
existing one.

If a rendering ever comes back dark anyway, that is a renderer regression. Do
not "fix" it by reverting to `dark_mode=true`; re-check the endpoint first.
"""

from __future__ import annotations

from typing import Any
from urllib.parse import parse_qsl, urlencode, urlparse, urlunparse

#: The query parameter Tako's renderer reads, and the value that means light.
DARK_MODE_PARAM = "dark_mode"
LIGHT_MODE_VALUE = "false"

#: Hosts whose URLs this module is allowed to rewrite. Deliberately a local
#: constant rather than an import from the Slack renderer: that module is
#: presentation-layer and moves around, while this normalization belongs to the
#: response payload and must not follow it.
TAKO_HOSTS = ("tako.com", "www.tako.com")

#: Card keys holding a rendering this tool embeds, and so pins to light.
#: `webpage_url` is excluded on purpose (see the module docstring).
RENDERING_KEYS = ("image_url", "embed_url")


def light_mode_url(url: str | None) -> str | None:
    """Return `url` with `dark_mode=false`, or unchanged if it is not a Tako URL.

    Idempotent, and safe on any input: a blank value, a non-Tako host, or an
    unparseable string comes back exactly as it went in. Other query parameters
    and their order are preserved; a repeated `dark_mode` collapses to one.
    """
    if not url or not isinstance(url, str):
        return url
    try:
        parts = urlparse(url)
    except ValueError:
        return url
    if parts.hostname is None or parts.hostname.lower() not in TAKO_HOSTS:
        return url

    query = [(k, v) for k, v in parse_qsl(parts.query, keep_blank_values=True)]
    seen = any(k == DARK_MODE_PARAM for k, _ in query)
    updated = [
        (k, LIGHT_MODE_VALUE if k == DARK_MODE_PARAM else v)
        for k, v in _drop_repeats(query, DARK_MODE_PARAM)
    ]
    if not seen:
        updated.append((DARK_MODE_PARAM, LIGHT_MODE_VALUE))
    return urlunparse(parts._replace(query=urlencode(updated)))


def _drop_repeats(pairs: list[tuple[str, str]], key: str) -> list[tuple[str, str]]:
    """Keep only the first occurrence of `key`, preserving order otherwise."""
    out: list[tuple[str, str]] = []
    kept = False
    for k, v in pairs:
        if k == key:
            if kept:
                continue
            kept = True
        out.append((k, v))
    return out


def apply_light_mode(payload: Any) -> Any:
    """Return a copy of a search/answer payload with every rendering set to light.

    Rewrites `image_url`/`embed_url` on each card and on the MCP-shaped
    top-level lead-card pointer, and clears the `dark_mode` flag the MCP reports
    alongside them so the payload does not contradict its own URLs.

    Never mutates its argument, and passes anything that is not a dict straight
    through, so it is safe to wrap around a backend result of any shape.
    """
    if not isinstance(payload, dict):
        return payload

    out = dict(payload)
    for key in RENDERING_KEYS:
        if key in out:
            out[key] = light_mode_url(out[key])
    if DARK_MODE_PARAM in out:
        out[DARK_MODE_PARAM] = False

    cards = out.get("cards")
    if isinstance(cards, list):
        out["cards"] = [_light_mode_card(card) for card in cards]
    return out


def _light_mode_card(card: Any) -> Any:
    """Return a copy of one card with its embedded renderings set to light."""
    if not isinstance(card, dict):
        return card
    out = dict(card)
    for key in RENDERING_KEYS:
        if key in out:
            out[key] = light_mode_url(out[key])
    return out
