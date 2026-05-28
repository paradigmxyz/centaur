"""Workflow: poll watched GitHub repos for new releases and announce them.

Posts to ``#system-updates`` whenever a tracked repo publishes a release
that has not already been announced. The destination Slack channel is the
cursor journal — each tick reads recent channel history, extracts markers
embedded in prior posts, and skips releases whose marker already appears.

This keeps state self-contained: no external KV store, no separate cursor
table, and the journal is human-auditable in the channel itself.

To extend coverage, add ``owner/repo`` entries to ``WATCHED_REPOS`` or pass
``{"repos": ["owner/repo", ...]}`` as workflow input.
"""

from __future__ import annotations

import re
from typing import TYPE_CHECKING, Any

import httpx

if TYPE_CHECKING:
    from api.workflow_engine import WorkflowContext

WORKFLOW_NAME = "release_watcher"
INTERVAL = 900
SLACK_CHANNEL = "system-updates"
WATCHED_REPOS: tuple[str, ...] = (
    "paradigmxyz/centaur",
)

_GITHUB_API = "https://api.github.com"
_HISTORY_LOOKBACK = 200
_HTTP_TIMEOUT = 10.0
_MARKER_RE = re.compile(r"`(release-watcher:[^`]+)`")


def _release_marker(repo: str, tag: str) -> str:
    """Stable marker embedded in every post so future ticks can dedup."""
    return f"release-watcher:{repo}@{tag}"


def _format_post(repo: str, release: dict[str, Any]) -> str:
    tag = release.get("tag_name") or "?"
    name = release.get("name") or tag
    html_url = release.get("html_url") or ""
    author = ""
    author_obj = release.get("author")
    if isinstance(author_obj, dict):
        author = str(author_obj.get("login") or "")
    suffix = f" by `{author}`" if author else ""
    marker = _release_marker(repo, tag)
    return (
        f"*New release*: <{html_url}|{repo} {name}>{suffix}\n"
        f"_marker: `{marker}`_"
    )


async def _fetch_latest_release(repo: str) -> dict[str, Any] | None:
    url = f"{_GITHUB_API}/repos/{repo}/releases/latest"
    async with httpx.AsyncClient(timeout=_HTTP_TIMEOUT) as client:
        resp = await client.get(
            url, headers={"Accept": "application/vnd.github+json"},
        )
    if resp.status_code == 404:
        return None
    resp.raise_for_status()
    data = resp.json()
    return data if isinstance(data, dict) else None


def _already_posted_markers(messages: list[dict[str, Any]]) -> set[str]:
    seen: set[str] = set()
    for msg in messages:
        text = msg.get("text") if isinstance(msg, dict) else None
        if not isinstance(text, str):
            continue
        seen.update(_MARKER_RE.findall(text))
    return seen


async def handler(inp: dict[str, Any], ctx: WorkflowContext) -> dict[str, Any]:
    inp_dict = inp if isinstance(inp, dict) else {}
    channel = inp_dict.get("slack_channel") or SLACK_CHANNEL
    repos_in = inp_dict.get("repos")
    watched = (
        tuple(str(r) for r in repos_in)
        if isinstance(repos_in, list) and repos_in
        else WATCHED_REPOS
    )

    history = await ctx.call_tool(
        "slack",
        "get_channel_history",
        {"channel": channel, "limit": _HISTORY_LOOKBACK},
    )
    messages: list[dict[str, Any]] = history if isinstance(history, list) else []
    seen_markers = _already_posted_markers(messages)

    announced: list[dict[str, str]] = []
    skipped: list[dict[str, str]] = []
    errors: list[dict[str, str]] = []

    for repo in watched:
        try:
            release = await ctx.step(
                f"fetch_{repo}",
                lambda r=repo: _fetch_latest_release(r),
                step_kind="github_release",
            )
        except Exception as err:
            errors.append({"repo": repo, "error": str(err)})
            continue

        if not isinstance(release, dict):
            skipped.append({"repo": repo, "reason": "no_releases"})
            continue

        tag = str(release.get("tag_name") or "")
        if not tag:
            skipped.append({"repo": repo, "reason": "missing_tag"})
            continue

        marker = _release_marker(repo, tag)
        if marker in seen_markers:
            skipped.append({"repo": repo, "reason": "already_announced", "tag": tag})
            continue

        await ctx.post_to_slack(channel, _format_post(repo, release))
        announced.append({"repo": repo, "tag": tag})
        seen_markers.add(marker)

    return {
        "channel": channel,
        "watched": list(watched),
        "announced": announced,
        "skipped": skipped,
        "errors": errors,
    }
