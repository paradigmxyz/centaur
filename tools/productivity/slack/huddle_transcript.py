"""The verbatim transcript of a recorded Slack huddle.

Slack records every huddle and saves the full, speaker-attributed transcript as a
``huddle_transcript`` file. That file is never shared into a channel, so **a bot token cannot
download it, and there is no public API for it** — Slack support confirms this is deliberate.
``files.info`` on the id returns metadata and no words, whatever scopes the bot holds. So the richest
record a company produces about its own decisions — the standup where the priority was set, the
incident call where the cause was found — is the one thing an agent in that workspace cannot read.

The Slack *web client* reads it perfectly well. It calls ``files.info`` on the **workspace host**
(``<workspace>.slack.com``, never ``slack.com``) with ``include_transcription=true``, authenticated by
a user web session: an ``xoxc`` token paired with the ``d`` cookie. This module replays exactly that
call. Nothing here is a scope trick or a permission bypass — a user session sees precisely what that
user could already read by scrolling the huddle in their own Slack client.

Two halves, deliberately separate, because they fail in completely different ways:

* **Discovery** — which huddles exist, and which transcript file belongs to each — runs on the
  ordinary bot token and is stable. It never needs the session.
* **Fetch** — the words themselves — is the session-bound half. When a session lapses it fails
  **loudly** (``needs_reauth``), never by returning an empty transcript. That distinction is the whole
  ballgame: a silent empty read looks exactly like "nobody said anything", and an agent that quietly
  believes a meeting was silent is worse than one that admits it cannot see.

The parsing below is pure and unit-tested against real Slack payloads; the network call is a thin
shell around it.
"""

from __future__ import annotations

import json
import re
import urllib.parse
import urllib.request

# Slack marks each spoken turn with a bold ` [mm:ss]: ` between the speaker and their words — how far
# into the call the turn happened. Long huddles use `[h:mm:ss]`.
_STAMP = re.compile(r"\[(\d{1,2}:\d{2}(?::\d{2})?)\]")

_PAGE = 1000  # turns per page; long huddles paginate
_MAX_PAGES = 60  # ~60k turns — a backstop against a pathological response, never hit by a real huddle

# The failures that mean "the human must sign in again", as opposed to "this code is wrong". Only
# these set ``needs_reauth``; everything else is a bug and should read like one.
_REAUTH_ERRORS = frozenset(
    {"not_authed", "invalid_auth", "token_revoked", "token_expired", "account_inactive"}
)


class HuddleTranscriptError(RuntimeError):
    """A structured failure that survives stringification through the tool boundary.

    ``needs_reauth=True`` marks the one failure that is not a defect: the user web session expired,
    and only a human signing in again can restore it. Callers should surface that verbatim rather than
    treating it as an empty result.
    """

    def __init__(self, message: str, **detail: object) -> None:
        self.payload = {"error": "huddle_transcript_failed", "message": message, **detail}
        super().__init__(str(self.payload))


# ── parsing (pure) ──────────────────────────────────────────────────────────────────────────────


def parse_segments(files_info: dict) -> list[dict]:
    """The transcript as ordered turns: ``{"user_id", "at", "text"}``.

    Slack returns it as one ``rich_text`` block whose elements are a ``rich_text_section`` per spoken
    turn. Each section carries a ``user`` element (the speaker's id), a **bold** ``text`` element
    holding the ` [mm:ss]: ` mark, and one or more plain ``text`` elements with the words.

    Speakers stay as **Slack ids, never names**. An id round-trips to a real mention and cannot drift;
    a name resolved at parse time silently rots when someone changes their display name.
    """
    block = ((files_info.get("file") or {}).get("huddle_transcription") or {}).get("blocks") or {}
    out: list[dict] = []
    for section in block.get("elements", []):
        if section.get("type") != "rich_text_section":
            continue
        user_id: str | None = None
        at: str | None = None
        words: list[str] = []
        for element in section.get("elements", []):
            if element.get("type") == "user":
                user_id = element.get("user_id")
            elif element.get("type") == "text":
                text = element.get("text", "")
                stamp = _STAMP.search(text)
                # The bold element that *is* a timestamp is the marker, not speech. A bold word inside
                # the speech itself carries no [mm:ss] and must survive.
                if stamp and (element.get("style") or {}).get("bold"):
                    at = stamp.group(1)
                else:
                    words.append(text)
        said = "".join(words).strip()
        if said:
            out.append({"user_id": user_id, "at": at, "text": said})
    return out


def render(segments: list[dict]) -> str:
    """Turns → a readable transcript, one line each: ``<@U…> [mm:ss]: words``.

    The ``<@U…>`` form is what Slack renders as a real mention, so a verbatim quote names the person
    instead of printing id soup.
    """
    lines = []
    for segment in segments:
        who = f"<@{segment['user_id']}>" if segment.get("user_id") else "someone"
        at = f" [{segment['at']}]" if segment.get("at") else ""
        lines.append(f"{who}{at}: {segment['text']}")
    return "\n".join(lines)


def speakers(segments: list[dict]) -> list[str]:
    """Distinct speaker ids in first-spoke order — an attendee list drawn from who actually *talked*,
    which is not the same as who Slack listed as present."""
    seen: list[str] = []
    for segment in segments:
        uid = segment.get("user_id")
        if uid and uid not in seen:
            seen.append(uid)
    return seen


# ── the session-bound fetch (thin shell) ────────────────────────────────────────────────────────


def _call(host: str, token: str, cookie: str, **params: str) -> dict:
    """One ``files.info`` call against the workspace host.

    The token rides in the ``Authorization`` header and the session in ``Cookie``. Both are places
    iron-proxy is allowed to look (``match_headers``), so in a sandbox the tool holds only
    placeholders and the real session never leaves the proxy. That is the reason this does not simply
    post the token as a form field the way the browser does: a request body is the one place the
    firewall cannot reach, and a user session token sitting in a sandbox is exactly the exposure the
    injection model exists to prevent.
    """
    url = f"https://{host}/api/files.info?" + urllib.parse.urlencode(params)
    request = urllib.request.Request(
        url,
        data=b"",  # Slack wants a POST; the parameters ride in the query string
        headers={"Authorization": f"Bearer {token}", "Cookie": cookie},
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            body = json.load(response)
    except OSError as exc:  # network/TLS, not Slack saying no
        raise HuddleTranscriptError(f"could not reach {host}", cause=str(exc)) from exc

    if not body.get("ok"):
        error = body.get("error")
        if error in _REAUTH_ERRORS:
            raise HuddleTranscriptError(
                "the Slack web session expired — a human must sign in again and refresh the "
                "SLACK_WEB_TOKEN / SLACK_WEB_COOKIE pair; huddle transcripts stay unreadable until "
                "they do",
                slack_error=error,
                needs_reauth=True,
            )
        raise HuddleTranscriptError("Slack returned not-ok", slack_error=error)
    return body


def fetch(file_id: str, *, host: str, token: str, cookie: str) -> dict:
    """The full verbatim transcript for one ``huddle_transcript`` file id.

    Returns ``{"file_id", "speakers", "turns", "text"}``. Raises :class:`HuddleTranscriptError` with
    ``needs_reauth=True`` when the session has lapsed — the loud signal that keeps a stale cookie from
    ever reading as "this huddle had no transcript".

    Only the rendered ``text`` comes back, not the parsed segments as well. They are the same words
    twice, and this is the largest single payload the tool can produce: returning both writes the
    whole huddle into the model's context a second time, and it is then re-read on every request of
    the turn. Callers that need the structure can call :func:`parse_segments` themselves.
    """
    segments: list[dict] = []
    for page in range(1, _MAX_PAGES + 1):
        body = _call(
            host,
            token,
            cookie,
            file=file_id,
            include_transcription="true",
            page=str(page),
            count=str(_PAGE),
        )
        chunk = parse_segments(body)
        segments += chunk
        if len(chunk) < _PAGE:  # a short page is the last page — also the no-pagination case
            break
    return {
        "file_id": file_id,
        "speakers": speakers(segments),
        "turns": len(segments),
        "text": render(segments),
    }
