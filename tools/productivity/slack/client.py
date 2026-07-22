"""Slack API client for bot-token Slack tool operations."""

import base64
import binascii
import json
import mimetypes
import os
import re
import time
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, ClassVar
from urllib.parse import urlparse

import structlog
from slack_sdk import WebClient
from slack_sdk.errors import SlackApiError

from centaur_sdk.tool_sdk import secret

# structlog so these lines render as JSON through the tool-server's
# configure_structlog() pipeline, like the rest of the service's logs.
logger = structlog.get_logger()

# Cache for channel list to avoid repeated API calls


class SlackAuthError(RuntimeError):
    """Structured Slack auth failure that survives tool-manager stringification."""

    def __init__(
        self,
        *,
        slack_method: str,
        access_path: str,
        error_code: str,
        status_code: int | None,
        requested_channel: str | None = None,
        resolved_channel: str | None = None,
    ) -> None:
        payload = {
            "error": "slack_auth_failed",
            "message": f"Slack authentication failed for {slack_method} via {access_path}",
            "slack_method": slack_method,
            "access_path": access_path,
            "error_code": error_code,
            "status_code": status_code,
            "requested_channel": requested_channel,
            "resolved_channel": resolved_channel,
        }
        self.payload = payload
        super().__init__(json.dumps(payload, sort_keys=True))


class SlackRateLimitError(RuntimeError):
    """Structured Slack rate-limit failure that does not sleep through tool timeouts."""

    def __init__(self, *, slack_method: str, retry_after: float, access_path: str) -> None:
        payload = {
            "error": "slack_rate_limited",
            "message": f"Slack rate limited {slack_method}; retry after {retry_after:.2f}s",
            "slack_method": slack_method,
            "access_path": access_path,
            "retry_after_seconds": retry_after,
        }
        self.payload = payload
        super().__init__(json.dumps(payload, sort_keys=True))


class SlackClient:
    """Slack API client.

    Most operations use the bot token. Native Slack search and Slack ETL can
    optionally use dedicated user tokens so workspace-wide reads stay separate
    from the interactive bot's access model.
    """

    # Cache settings
    _CACHE_DIR = Path.home() / ".cache" / "paradigm-slack"
    _CHANNEL_CACHE_FILE = _CACHE_DIR / "channels.json"
    _USER_CACHE_FILE = _CACHE_DIR / "users.json"
    _CHANNEL_CACHE_TTL = 300  # 5 minutes
    _USER_CACHE_TTL = 600  # 10 minutes
    _MAX_PAGE_SIZE = 200
    _MAX_SLACK_HISTORY_PROXY_PAGE_SIZE = 999
    _MAX_SLACK_FILES_LIST_PAGE_SIZE = 200
    _DEFAULT_THREAD_REPLY_LIMIT = 50
    _DEFAULT_DUMP_MESSAGE_LIMIT = 100
    _DEFAULT_DUMP_THREAD_LIMIT = 25
    _DEFAULT_API_TIMEOUT_SECONDS = 8
    _MAX_RATE_LIMIT_SLEEP_SECONDS = 0.0
    _DATE_ONLY_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
    _NUMERIC_TS_RE = re.compile(r"^\d+(?:\.\d+)?$")
    _CHANNEL_ID_RE = re.compile(r"^[CGD][A-Z0-9]+$")
    _FILE_ID_RE = re.compile(r"^F[A-Z0-9]+$")
    _USER_ID_RE = re.compile(r"^[UW][A-Z0-9]+$")
    _AUTH_ERROR_CODES: ClassVar[frozenset[str]] = frozenset(
        {
            "account_inactive",
            "invalid_auth",
            "missing_scope",
            "no_permission",
            "not_allowed_token_type",
            "not_authed",
            "token_revoked",
        }
    )

    def __init__(
        self,
        bot_token: str | None = None,
        search_token: str | None = None,
    ):
        token = (bot_token or secret("SLACK_BOT_TOKEN", default="")).strip()
        if not token:
            raise RuntimeError(
                "SLACK_BOT_TOKEN not set.\n"
                "Get one at https://api.slack.com/apps → OAuth & Permissions → Bot User OAuth Token"
            )
        self.token = token
        self.search_token = (search_token or secret("SLACK_SEARCH_TOKEN", default="")).strip()
        timeout = self._api_timeout_seconds()
        self._client = WebClient(token=token, timeout=timeout)
        self._search_client = (
            WebClient(token=self.search_token, timeout=timeout)
            if self.search_token
            else self._client
        )
        self._user_cache: dict[str, str] = {}
        self._ratelimit_deadlines: dict[str, float] = {}

    def __getattr__(self, name: str):
        """Proxy raw Slack SDK methods when the higher-level wrapper does not define them."""
        return getattr(self._client, name)

    def _is_ratelimit_error(self, error: SlackApiError) -> bool:
        """Detect Slack rate limit responses from either payload or status code."""
        status_code = getattr(error.response, "status_code", None)
        return status_code == 429 or error.response.get("error") == "ratelimited"

    def _slack_error_code(self, error: SlackApiError) -> str:
        """Return Slack's machine-readable error code when present."""
        return str(error.response.get("error") or "unknown_error")

    def _is_auth_error(self, error: SlackApiError) -> bool:
        """Classify auth and scope failures so callers can choose better fallbacks."""
        status_code = getattr(error.response, "status_code", None)
        return status_code in {401, 403} or self._slack_error_code(error) in self._AUTH_ERROR_CODES

    @classmethod
    def _api_timeout_seconds(cls) -> int:
        """Return Slack SDK request timeout in seconds."""
        raw = secret("SLACK_API_TIMEOUT_SECONDS", default="")
        if raw is None:
            return cls._DEFAULT_API_TIMEOUT_SECONDS
        raw = str(raw).strip()
        if not raw:
            return cls._DEFAULT_API_TIMEOUT_SECONDS
        try:
            return max(1, int(raw))
        except ValueError:
            return cls._DEFAULT_API_TIMEOUT_SECONDS

    def _api_server_proxy_enabled(self) -> bool:
        """Return whether the sandbox API-server proxy is enabled."""
        return secret("CENTAUR_SANDBOX_API_SERVER_ENABLED", "true").strip().lower() != "false"

    def _clean_channel_ref(self, channel: str) -> str:
        """Normalize #name, ID, and <#ID|name> Slack channel references."""
        raw = str(channel).strip()
        if raw.startswith("<#") and raw.endswith(">"):
            raw = raw[2:-1].split("|", 1)[0]
        return raw.lstrip("#").strip()

    def _looks_like_channel_id(self, channel: str) -> bool:
        """Return whether a channel reference is already a Slack conversation ID."""
        return bool(self._CHANNEL_ID_RE.fullmatch(self._clean_channel_ref(channel).upper()))

    def _clean_user_ref(self, user_id: str) -> str:
        """Normalize plain and mention-form Slack user IDs."""
        raw = str(user_id).strip()
        if raw.startswith("<@") and raw.endswith(">"):
            raw = raw[2:-1].split("|", 1)[0]
        return raw.strip()

    def _looks_like_user_id(self, user_id: str) -> bool:
        """Return whether a value is a Slack user ID."""
        return bool(self._USER_ID_RE.fullmatch(self._clean_user_ref(user_id).upper()))

    def _raise_slack_api_error(
        self,
        error: SlackApiError,
        *,
        slack_method: str,
        access_path: str,
        requested_channel: str | None = None,
        resolved_channel: str | None = None,
    ) -> None:
        """Raise auth failures as structured payloads; keep other errors unchanged."""
        error_code = self._slack_error_code(error)
        status_code = getattr(error.response, "status_code", None)
        if self._is_auth_error(error):
            raise SlackAuthError(
                slack_method=slack_method,
                access_path=access_path,
                error_code=error_code,
                status_code=status_code,
                requested_channel=requested_channel,
                resolved_channel=resolved_channel,
            ) from error
        raise RuntimeError(f"Slack API error: {error_code}") from error

    def _retry_on_ratelimit(
        self,
        func,
        *args,
        method_key: str | None = None,
        max_retries: int = 6,
        max_retry_sleep_s: float | None = None,
        **kwargs,
    ):
        """Retry short Slack rate limits; fail fast on long Retry-After windows."""
        key = method_key or getattr(func, "__name__", "slack_api_call")
        max_sleep = (
            self._MAX_RATE_LIMIT_SLEEP_SECONDS
            if max_retry_sleep_s is None
            else max(0.0, max_retry_sleep_s)
        )
        for attempt in range(max_retries):
            blocked_until = self._ratelimit_deadlines.get(key, 0.0)
            remaining = blocked_until - time.time()
            if remaining > 0:
                if remaining > max_sleep:
                    raise SlackRateLimitError(
                        slack_method=key,
                        retry_after=round(remaining, 3),
                        access_path="slack_api",
                    )
                time.sleep(remaining)

            try:
                return func(*args, **kwargs)
            except SlackApiError as e:
                if self._is_ratelimit_error(e):
                    retry_after = self._parse_retry_after(
                        getattr(e.response, "headers", {}).get("Retry-After"),
                        default=max(1, 2**attempt),
                    )
                    self._ratelimit_deadlines[key] = time.time() + retry_after
                    if attempt < max_retries - 1 and retry_after <= max_sleep:
                        time.sleep(retry_after)
                        continue
                    raise SlackRateLimitError(
                        slack_method=key,
                        retry_after=retry_after,
                        access_path="slack_api",
                    ) from e
                raise
        raise RuntimeError("Max retries exceeded")

    def _parse_retry_after(self, value: str | None, default: int = 5) -> float:
        """Return a Retry-After delay with a small safety buffer."""
        try:
            seconds = float(value) if value is not None else float(default)
        except (TypeError, ValueError):
            seconds = float(default)
        return max(seconds, 1.0) + 0.25

    def _format_ts(self, value: float) -> str:
        """Format epoch seconds in Slack timestamp format."""
        return f"{value:.6f}"

    def _normalize_ts(self, value: str | int | float | None) -> str | None:
        """Accept Slack timestamps, epoch seconds, or ISO/date strings."""
        if value in (None, ""):
            return None

        if isinstance(value, int | float):
            seconds = float(value)
            if seconds >= 1_000_000_000_000:
                seconds /= 1000.0
            return self._format_ts(seconds)

        raw = str(value).strip()
        if not raw:
            return None

        if self._NUMERIC_TS_RE.fullmatch(raw):
            seconds = float(raw)
            if "." not in raw and len(raw) >= 13:
                seconds /= 1000.0
            return self._format_ts(seconds)

        if self._DATE_ONLY_RE.fullmatch(raw):
            parsed = datetime.fromisoformat(f"{raw}T00:00:00+00:00")
            return self._format_ts(parsed.timestamp())

        try:
            parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
        except ValueError as exc:
            raise ValueError(
                f"Unsupported timestamp format '{value}'. Use Slack ts, epoch seconds, ISO datetime, or YYYY-MM-DD."
            ) from exc

        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=UTC)
        return self._format_ts(parsed.timestamp())

    def _centaur_api_url(self) -> str:
        """Return the Centaur API base URL available inside agent sandboxes."""
        return secret("CENTAUR_API_URL", "http://api:8000").rstrip("/")

    def _centaur_api_headers(self) -> dict[str, str]:
        """Return headers for API-server calls.

        In sandboxes, iron-proxy injects the principal-scoped Authorization
        header for the API host. A local bearer can be supplied for tests or
        manual CLI use.
        """
        headers = {"Accept": "application/json"}
        bearer = secret("CENTAUR_API_BEARER_TOKEN", "").strip()
        if bearer:
            headers["Authorization"] = f"Bearer {bearer}"
        return headers

    def _centaur_api_query_value(self, value: Any) -> str:
        """Format query values for axum/serde query extraction."""
        if isinstance(value, bool):
            return "true" if value else "false"
        return str(value)

    def _centaur_api_url_for(self, path: str, params: dict[str, Any]) -> str:
        """Build a Centaur API URL with query parameters."""
        query = urllib.parse.urlencode(
            {
                key: self._centaur_api_query_value(value)
                for key, value in params.items()
                if value is not None
            }
        )
        url = f"{self._centaur_api_url()}{path}"
        if query:
            url = f"{url}?{query}"
        return url

    def _centaur_api_get_json(self, path: str, params: dict[str, Any]) -> dict[str, Any]:
        """GET a Centaur API JSON endpoint."""
        url = self._centaur_api_url_for(path, params)
        request = urllib.request.Request(url, headers=self._centaur_api_headers(), method="GET")
        try:
            with urllib.request.urlopen(request, timeout=self._api_timeout_seconds()) as response:
                raw = response.read().decode("utf-8")
        except urllib.error.HTTPError as exc:
            raw = exc.read().decode("utf-8", errors="replace")
            detail = raw
            try:
                body = json.loads(raw)
                detail = body.get("message") or body.get("detail") or raw
            except json.JSONDecodeError:
                pass
            raise RuntimeError(f"Centaur API error {exc.code} on {path}: {detail}") from exc
        except urllib.error.URLError as exc:
            raise RuntimeError(f"Centaur API request failed on {path}: {exc.reason}") from exc
        return json.loads(raw) if raw else {}

    def _centaur_api_post_bytes_json(
        self,
        path: str,
        params: dict[str, Any],
        body: bytes,
        content_type: str = "application/octet-stream",
    ) -> dict[str, Any]:
        """POST bytes to a Centaur API endpoint and parse a JSON response."""
        url = self._centaur_api_url_for(path, params)
        headers = self._centaur_api_headers()
        headers["Content-Type"] = content_type
        request = urllib.request.Request(url, data=body, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(request, timeout=self._api_timeout_seconds()) as response:
                raw = response.read().decode("utf-8")
        except urllib.error.HTTPError as exc:
            raw = exc.read().decode("utf-8", errors="replace")
            detail = raw
            try:
                error_body = json.loads(raw)
                detail = error_body.get("message") or error_body.get("detail") or raw
            except json.JSONDecodeError:
                pass
            raise RuntimeError(f"Centaur API error {exc.code} on {path}: {detail}") from exc
        except urllib.error.URLError as exc:
            raise RuntimeError(f"Centaur API request failed on {path}: {exc.reason}") from exc
        return json.loads(raw) if raw else {}

    def _centaur_api_get_bytes(
        self,
        path: str,
        params: dict[str, Any],
        max_bytes: int,
    ) -> tuple[bytes, dict[str, str]]:
        """GET bytes from a Centaur API endpoint."""
        url = self._centaur_api_url_for(path, params)
        request = urllib.request.Request(url, headers=self._centaur_api_headers(), method="GET")
        try:
            with urllib.request.urlopen(request, timeout=self._api_timeout_seconds()) as response:
                body = response.read(max_bytes + 1)
                headers = {key.lower(): value for key, value in response.headers.items()}
        except urllib.error.HTTPError as exc:
            raw = exc.read().decode("utf-8", errors="replace")
            detail = raw
            try:
                error_body = json.loads(raw)
                detail = error_body.get("message") or error_body.get("detail") or raw
            except json.JSONDecodeError:
                pass
            raise RuntimeError(f"Centaur API error {exc.code} on {path}: {detail}") from exc
        except urllib.error.URLError as exc:
            raise RuntimeError(f"Centaur API request failed on {path}: {exc.reason}") from exc
        if len(body) > max_bytes:
            raise ValueError(f"Slack file exceeds the {max_bytes}-byte download limit")
        return body, headers

    def _message_permalink(self, channel_id: str, ts: str) -> str:
        """Build a Slack permalink from channel and timestamp."""
        return f"https://slack.com/archives/{channel_id}/p{ts.replace('.', '')}"

    def _resolve_channel_name(self, channel: str, channel_id: str) -> str:
        """Resolve a human-readable channel name when callers passed an ID."""
        normalized = self._clean_channel_ref(channel)
        if normalized != channel_id:
            return normalized
        if self._looks_like_channel_id(normalized):
            return channel_id

        for item in self.list_bot_channels(limit=1000):
            if item["id"] == channel_id:
                return item["name"]
        return channel_id

    def _normalize_explicit_channel_id(self, channel_id: str) -> str:
        """Normalize and validate an explicit Slack conversation ID."""
        normalized_channel_id = self._clean_channel_ref(channel_id).upper()
        if len(normalized_channel_id) < 9 or not self._looks_like_channel_id(
            normalized_channel_id
        ):
            raise ValueError("channel_id must be a Slack conversation ID like C123456789")
        return normalized_channel_id

    def _normalize_file_id(self, file_id: str) -> str:
        """Normalize and validate a Slack file ID."""
        normalized_file_id = str(file_id).strip().upper()
        if len(normalized_file_id) < 9 or not self._FILE_ID_RE.fullmatch(normalized_file_id):
            raise ValueError("file_id must be a Slack file ID like F123456789")
        return normalized_file_id

    def _content_disposition_filename(self, value: str | None) -> str | None:
        """Extract a simple filename value from Content-Disposition."""
        if not value:
            return None
        match = re.search(r'filename="([^"]+)"', value)
        if match:
            return match.group(1)
        match = re.search(r"filename=([^;]+)", value)
        if match:
            return match.group(1).strip()
        return None

    def _serialize_message(
        self,
        msg: dict[str, Any],
        channel_id: str,
        user_cache: dict[str, str],
        *,
        channel_name: str | None = None,
    ) -> dict[str, Any]:
        """Normalize Slack API message payloads into a stable shape."""
        user_id = msg.get("user") or msg.get("bot_id", "")
        username = user_cache.get(user_id, msg.get("username", user_id))
        if not username:
            username = msg.get("bot_profile", {}).get("name", "") or user_id

        ts = msg.get("ts", "")
        message = {
            "user": username,
            "user_id": user_id,
            "text": self._resolve_mentions(msg.get("text", ""), user_cache),
            "timestamp": ts,
            "permalink": self._message_permalink(channel_id, ts),
            "channel_id": channel_id,
            "thread_ts": msg.get("thread_ts"),
            "reply_count": msg.get("reply_count", 0),
            "reply_users": msg.get("reply_users", []),
            "latest_reply": msg.get("latest_reply"),
            "type": msg.get("type", "message"),
            "subtype": msg.get("subtype"),
            "parent_user_id": msg.get("parent_user_id"),
            "bot_id": msg.get("bot_id"),
        }
        if channel_name is not None:
            message["channel"] = channel_name
        return message

    def _collect_cursor_pages(
        self,
        fetch_page: Callable[[str | None, int], dict[str, Any]],
        *,
        result_key: str,
        limit: int,
        cursor: str | None = None,
    ) -> tuple[list[dict[str, Any]], str | None, bool]:
        """Collect paginated Slack responses up to a caller-provided limit."""
        remaining = max(limit, 0)
        next_cursor = cursor
        items: list[dict[str, Any]] = []

        while remaining > 0:
            batch_limit = min(remaining, self._MAX_PAGE_SIZE)
            response = fetch_page(next_cursor, batch_limit)
            batch = response.get(result_key, []) or []
            items.extend(batch)

            next_cursor = response.get("response_metadata", {}).get("next_cursor") or None
            has_more = bool(next_cursor or response.get("has_more"))
            if not has_more or not batch:
                return items, next_cursor, has_more

            remaining = limit - len(items)

        return items, next_cursor, bool(next_cursor)

    def _resolve_channel(self, channel: str) -> str:
        """Resolve a channel name, channel ID, user ID, or @user DM to a conversation ID.

        User references (``U123``, ``<@U123>``, or ``@username``) resolve to the
        bot's one-on-one DM channel with that user, opening it if needed.
        """
        raw = str(channel).strip()
        if self._looks_like_user_id(raw):
            return self._open_dm_channel(raw)
        if raw.startswith("@"):
            username = raw[1:].strip()
            user_cache = self._get_user_cache()
            for user_id, name in user_cache.items():
                if name == username:
                    return self._open_dm_channel(user_id)
            raise RuntimeError(f"User '{channel}' not found in workspace")
        normalized = self._clean_channel_ref(channel)
        if self._looks_like_channel_id(normalized):
            return normalized.upper()
        channels = self.list_bot_channels()
        name = normalized
        for ch in channels:
            if ch["name"] == name:
                return ch["id"]
        raise RuntimeError(f"Channel '{channel}' not found or bot not a member")

    def _open_dm_channel(self, user_id: str) -> str:
        """Open or reuse a one-on-one DM channel with a Slack user."""
        normalized = self._clean_user_ref(user_id).upper()
        if not self._looks_like_user_id(normalized):
            raise ValueError("user_id must be a Slack user ID like U123 or <@U123>")
        try:
            response = self._retry_on_ratelimit(
                self._client.conversations_open,
                users=normalized,
                method_key="conversations.open",
            )
        except SlackApiError as e:
            self._raise_slack_api_error(
                e,
                slack_method="conversations.open",
                access_path="bot_token",
                requested_channel=normalized,
            )
        channel_id = response.get("channel", {}).get("id")
        if not channel_id:
            raise RuntimeError("Slack API error: conversations.open returned no channel id")
        return str(channel_id)

    def _resolve_message_destination(self, channel: str) -> str:
        """Resolve a send_message destination from channel, channel ID, or user ID."""
        if self._looks_like_user_id(channel):
            return self._open_dm_channel(channel)
        return self._resolve_channel(channel)

    def _resolve_mentions(self, text: str, user_cache: dict[str, str]) -> str:
        """Replace <@USER_ID> mentions with @username using cached lookups only."""

        def replace_mention(match: re.Match) -> str:
            user_id = match.group(1)
            name = user_cache.get(user_id, user_id)
            return f"@{name}"

        return re.sub(r"<@([A-Z0-9]+)>", replace_mention, text)

    def _load_channel_cache(self) -> tuple[list[dict], float] | None:
        """Load cached channel list if valid."""
        try:
            if self._CHANNEL_CACHE_FILE.exists():
                data = json.loads(self._CHANNEL_CACHE_FILE.read_text())
                cached_at = data.get("cached_at", 0)
                if time.time() - cached_at < self._CHANNEL_CACHE_TTL:
                    return data.get("channels", []), cached_at
        except Exception:
            pass
        return None

    def _save_channel_cache(self, channels: list[dict]) -> None:
        """Save channel list to cache."""
        try:
            self._CACHE_DIR.mkdir(parents=True, exist_ok=True)
            self._CHANNEL_CACHE_FILE.write_text(
                json.dumps(
                    {
                        "cached_at": time.time(),
                        "channels": channels,
                    }
                )
            )
        except Exception:
            pass

    def _load_user_cache(self) -> dict[str, str] | None:
        """Load cached user list if valid."""
        try:
            if self._USER_CACHE_FILE.exists():
                data = json.loads(self._USER_CACHE_FILE.read_text())
                cached_at = data.get("cached_at", 0)
                if time.time() - cached_at < self._USER_CACHE_TTL:
                    return data.get("users", {})
        except Exception:
            pass
        return None

    def _save_user_cache(self, users: dict[str, str]) -> None:
        """Save user mapping to cache."""
        try:
            self._CACHE_DIR.mkdir(parents=True, exist_ok=True)
            self._USER_CACHE_FILE.write_text(
                json.dumps(
                    {
                        "cached_at": time.time(),
                        "users": users,
                    }
                )
            )
        except Exception:
            pass

    def _get_user_cache(self) -> dict[str, str]:
        """Get user ID -> name mapping, using cache when possible."""
        cached = self._load_user_cache()
        if cached:
            return cached

        user_cache: dict[str, str] = {}
        try:
            users_response = self._retry_on_ratelimit(self._client.users_list, limit=1000)
            for user in users_response.get("members", []):
                user_cache[user.get("id", "")] = user.get("name", "")
            self._save_user_cache(user_cache)
        except (SlackApiError, SlackRateLimitError):
            pass
        return user_cache

    def list_bot_channels(self, limit: int = 500, force_refresh: bool = False) -> list[dict]:
        """List channels (public AND private) the bot is a member of.

        Bot membership is the relevant scope here, not just public visibility
        — Centaur is intentionally added to private investing / sourcing /
        legal channels and search/history calls only work on channels the bot
        actually belongs to. Filtering to ``types="public_channel"`` silently
        drops every private channel from this list and from every caller
        that depends on it (e.g. ``_search_messages_local`` scans this list
        as the fallback when native search lacks ``search:read``, and
        ``gather_context``'s Slack grab walks the bot's member channels).

        Uses ``users.conversations``, which returns only the conversations the
        bot is a member of. ``conversations.list`` would instead page through
        every channel in the workspace and filter ``is_member`` client-side —
        an O(workspace) scan that, on large workspaces, never finishes inside
        the tool-call budget (so the cache below never warms and every call
        re-scans the whole workspace from scratch).

        Args:
            limit: Maximum channels to return
            force_refresh: Ignore cache and fetch fresh data

        Returns:
            List of channel dicts with id, name, is_private
        """
        # Check cache first
        if not force_refresh:
            cached = self._load_channel_cache()
            if cached:
                channels, _ = cached
                return channels[:limit]

        channels = []
        cursor = None

        while True:
            try:
                response = self._retry_on_ratelimit(
                    self._client.users_conversations,
                    types="public_channel,private_channel",
                    limit=min(limit - len(channels), 200),
                    cursor=cursor,
                    exclude_archived=True,
                )
            except SlackApiError as e:
                self._raise_slack_api_error(
                    e,
                    slack_method="users.conversations",
                    access_path="bot_token",
                )

            # users.conversations only returns conversations the bot belongs
            # to, so membership is implied — no client-side is_member filter
            # (and no whole-workspace pagination) needed.
            for channel in response.get("channels", []):
                channels.append(
                    {
                        "id": channel.get("id", ""),
                        "name": channel.get("name", ""),
                        "purpose": channel.get("purpose", {}).get("value", ""),
                        "topic": channel.get("topic", {}).get("value", ""),
                        "member_count": channel.get("num_members", 0),
                        "is_private": channel.get("is_private", False),
                    }
                )

            cursor = response.get("response_metadata", {}).get("next_cursor")
            if not cursor or len(channels) >= limit:
                break

        result = sorted(channels, key=lambda x: x["name"])
        self._save_channel_cache(result)
        return result

    def _fetch_channel_history_for_search(
        self,
        channel_id: str,
        channel_name: str,
        limit: int,
        user_cache: dict[str, str],
    ) -> list[dict]:
        """Fetch history for a single search fallback channel."""
        try:
            response = self.get_channel_history_proxy(channel_id, limit=limit)
        except (RuntimeError, ValueError):
            return self._fetch_direct_channel_history_for_search(
                channel_id,
                channel_name,
                limit,
                user_cache,
            )

        messages = []
        for msg in response.get("messages", []):
            messages.append(
                self._serialize_message(
                    msg,
                    channel_id,
                    user_cache,
                    channel_name=channel_name,
                )
            )

        return messages

    _MAX_SEARCH_DIRECT_THREADS = 10

    def _fetch_direct_channel_history_for_search(
        self,
        channel_id: str,
        channel_name: str,
        limit: int,
        user_cache: dict[str, str],
    ) -> list[dict]:
        """Fetch direct Slack history and expand a bounded number of threads."""
        try:
            page = self.get_channel_history_page(channel_id, limit=limit)
        except (RuntimeError, ValueError):
            return []

        messages = [{**msg, "channel": channel_name} for msg in page.get("messages", [])]
        seen_timestamps = {message.get("timestamp") for message in messages}
        expanded_threads = 0

        for message in list(messages):
            if expanded_threads >= self._MAX_SEARCH_DIRECT_THREADS:
                break
            if int(message.get("reply_count") or 0) <= 0:
                continue

            thread_ts = message.get("thread_ts") or message.get("timestamp")
            if not thread_ts:
                continue

            try:
                thread_page = self.get_thread_replies_page(
                    channel=channel_id,
                    thread_ts=thread_ts,
                    limit=min(limit, self._DEFAULT_THREAD_REPLY_LIMIT),
                )
            except (RuntimeError, ValueError):
                continue

            expanded_threads += 1
            for reply in thread_page.get("messages", []):
                timestamp = reply.get("timestamp")
                if not timestamp or timestamp in seen_timestamps:
                    continue
                seen_timestamps.add(timestamp)
                messages.append({**reply, "channel": channel_name})

        return messages

    _MAX_SEARCH_CHANNELS = 50  # Max channels to search when no filter specified

    def _rank_channels_for_query(self, channels: list[dict], query_terms: list[str]) -> list[dict]:
        """Rank channels by relevance to query terms. Most relevant first."""
        scored = []
        for ch in channels:
            score = 0.0
            name_lower = ch["name"].lower()
            searchable = f"{name_lower} {ch.get('purpose', '')} {ch.get('topic', '')}".lower()
            for term in query_terms:
                if term in name_lower:
                    score += 5.0
                elif term in searchable:
                    score += 2.0
            # Boost by member count (more members = more likely relevant)
            score += min(ch.get("member_count", 0) / 50, 3.0)
            scored.append((score, ch))
        scored.sort(key=lambda x: -x[0])
        return [ch for _, ch in scored]

    def _score_match(self, query_terms: list[str], text: str) -> float:
        """Score how well text matches query terms. Higher = better match."""
        text_lower = text.lower()
        score = 0.0

        # Exact phrase match (highest score)
        full_query = " ".join(query_terms)
        if full_query in text_lower:
            score += 10.0

        # Individual term matches
        for term in query_terms:
            if term in text_lower:
                score += 1.0
                # Bonus for word boundary matches
                if f" {term} " in f" {text_lower} ":
                    score += 0.5

        # Penalty for very long messages (likely less relevant)
        if len(text) > 500:
            score *= 0.8

        return score

    def search_messages(
        self,
        query: str,
        max_results: int = 20,
        channels: list[str] | None = None,
        from_user: str | None = None,
        messages_per_channel: int = 200,
    ) -> list[dict]:
        """Search messages using Slack's native search.messages API.

        Uses Slack's native search.messages API for fast, workspace-wide
        search. When ``SLACK_SEARCH_TOKEN`` is configured, the native call runs
        with that dedicated user token and its ``search:read`` scope. Falls
        back to proxy-backed channel history scanning if the native API fails.

        Supports Slack search modifiers in the query string:
            in:#channel, from:@user, before:YYYY-MM-DD, after:YYYY-MM-DD,
            has:link, has:reaction, is:thread, etc.

        Args:
            query: Search query (plain text or with Slack search modifiers)
            max_results: Maximum results to return
            channels: Optional list of channel names to filter by
            from_user: Optional username to filter by
            messages_per_channel: Messages per channel (only used in fallback)

        Returns:
            List of matching message dicts, sorted by relevance
        """
        local_query, local_channels, local_from_user = self._extract_local_search_filters(
            query, channels, from_user
        )
        if local_channels:
            return self._search_messages_local(
                local_query,
                max_results,
                local_channels,
                local_from_user,
                messages_per_channel,
            )

        # Build the search query with modifiers
        search_query = query
        if from_user:
            search_query += f" from:@{from_user.lstrip('@')}"

        try:
            return self._search_messages_native(search_query, max_results)
        except (SlackApiError, RuntimeError, SlackRateLimitError):
            # Fall back to proxy-backed channel history scanning if native search fails.
            return self._search_messages_local(
                local_query, max_results, local_channels, local_from_user, messages_per_channel
            )

    def _search_messages_native(
        self,
        query: str,
        max_results: int = 20,
    ) -> list[dict]:
        """Search using Slack's native search.messages API."""
        response = self._retry_on_ratelimit(
            self._search_client.api_call,
            "search.messages",
            method_key="search.messages",
            params={"query": query, "count": max_results, "sort": "timestamp"},
        )

        if not response.get("ok"):
            raise RuntimeError(response.get("error", "search.messages failed"))

        matches = response.get("messages", {}).get("matches", [])
        user_cache = self._get_user_cache()

        results = []
        for m in matches:
            user_id = m.get("user", "")
            username = user_cache.get(user_id, m.get("username", user_id))
            channel_info = m.get("channel", {})
            channel_id = channel_info.get("id", "") if isinstance(channel_info, dict) else ""
            channel_name = channel_info.get("name", "") if isinstance(channel_info, dict) else ""
            ts = m.get("ts", "")
            text = self._resolve_mentions(m.get("text", ""), user_cache)

            results.append(
                {
                    "channel": channel_name,
                    "channel_id": channel_id,
                    "user": username,
                    "user_id": user_id,
                    "text": text,
                    "timestamp": ts,
                    "permalink": m.get("permalink", ""),
                    "thread_ts": m.get("thread_ts"),
                    "reply_count": m.get("reply_count", 0),
                }
            )

        return results

    def _extract_local_search_filters(
        self,
        query: str,
        channels: list[str] | None,
        from_user: str | None,
    ) -> tuple[str, list[str] | None, str | None]:
        """Extract common Slack search modifiers for the local history scanner."""
        local_channels = list(channels or [])
        local_from_user = from_user

        def channel_repl(match: re.Match) -> str:
            local_channels.append(match.group("id") or match.group("name") or "")
            return " "

        query = re.sub(
            r"(?<!\S)in:(?:<#(?P<id>[CGD][A-Z0-9]+)(?:\|[^>]+)?>|#?(?P<name>[A-Za-z0-9_-]+))",
            channel_repl,
            query,
        )

        def from_repl(match: re.Match) -> str:
            nonlocal local_from_user
            local_from_user = match.group("uid") or match.group("uname") or local_from_user
            return " "

        query = re.sub(
            r"(?<!\S)from:(?:<@(?P<uid>[A-Z0-9]+)>|@?(?P<uname>[A-Za-z0-9._-]+))",
            from_repl,
            query,
        )

        deduped_channels = []
        seen = set()
        for channel in local_channels:
            normalized = self._clean_channel_ref(channel)
            if not normalized:
                continue
            key = normalized.lower()
            if key in seen:
                continue
            seen.add(key)
            deduped_channels.append(normalized)

        return " ".join(query.split()), deduped_channels or None, local_from_user

    def _channel_refs_for_search(self, channels: list[str]) -> list[dict]:
        """Resolve channel filters without listing channels when IDs are provided."""
        resolved: dict[str, dict] = {}
        unresolved_names: set[str] = set()

        for channel in channels:
            normalized = self._clean_channel_ref(channel)
            if not normalized:
                continue
            if self._looks_like_channel_id(normalized):
                channel_id = normalized.upper()
                resolved[channel_id] = {
                    "id": channel_id,
                    "name": channel_id,
                    "purpose": "",
                    "topic": "",
                    "member_count": 0,
                    "is_private": channel_id.startswith("G"),
                }
            else:
                unresolved_names.add(normalized.lower())

        if unresolved_names:
            for channel in self.list_channels_proxy(history_only=True):
                if channel["name"].lower() in unresolved_names:
                    resolved[channel["id"]] = channel

        return list(resolved.values())

    def _search_messages_local(
        self,
        query: str,
        max_results: int = 20,
        channels: list[str] | None = None,
        from_user: str | None = None,
        messages_per_channel: int = 200,
    ) -> list[dict]:
        """Search messages by scanning channel histories through the proxy."""
        query_terms = [t.strip().lower() for t in query.split() if t.strip()]

        if channels:
            bot_channels = self._channel_refs_for_search(channels)
        else:
            bot_channels = self.list_channels_proxy(history_only=True)
            bot_channels = self._rank_channels_for_query(bot_channels, query_terms)
            bot_channels = bot_channels[: self._MAX_SEARCH_CHANNELS]

        if not bot_channels:
            return []

        user_cache = self._get_user_cache()
        effective_limit = max(1, min(int(messages_per_channel), self._MAX_PAGE_SIZE))
        if len(bot_channels) > 30 and messages_per_channel > 100:
            effective_limit = 100

        all_messages = []
        with ThreadPoolExecutor(max_workers=min(6, len(bot_channels))) as executor:
            futures = {
                executor.submit(
                    self._fetch_channel_history_for_search,
                    ch["id"],
                    ch["name"],
                    effective_limit,
                    user_cache,
                ): ch
                for ch in bot_channels
            }

            for future in as_completed(futures):
                try:
                    messages = future.result()
                    all_messages.extend(messages)
                except Exception:
                    pass

        scored_results = []
        for msg in all_messages:
            text_lower = msg["text"].lower()
            if query_terms and not any(term in text_lower for term in query_terms):
                continue

            if from_user:
                username = user_cache.get(msg["user_id"], msg["user_id"])
                normalized_user = from_user.lower().lstrip("@")
                if normalized_user not in {username.lower(), msg["user_id"].lower()}:
                    continue

            score = self._score_match(query_terms, msg["text"])
            msg["user"] = user_cache.get(msg["user_id"], msg["user_id"])
            msg["text"] = self._resolve_mentions(msg["text"], user_cache)
            msg["_score"] = score
            scored_results.append(msg)

        scored_results.sort(key=lambda x: (-x["_score"], -float(x["timestamp"])))
        for msg in scored_results:
            del msg["_score"]

        return scored_results[:max_results]

    def get_channel_history_page(
        self,
        channel: str,
        limit: int = _DEFAULT_THREAD_REPLY_LIMIT,
        cursor: str | None = None,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
        inclusive: bool = False,
    ) -> dict[str, Any]:
        """Fetch a resumable page of channel history for ETL-style backfills.

        This follows Slack's cursor pagination model and accepts explicit date
        windows, which is the pattern Slack recommends for large historical
        exports. Use `next_cursor` to continue a backfill without rescanning
        the same date range.
        """
        user_cache = self._get_user_cache()
        channel_id = self._resolve_channel(channel)
        channel_name = self._resolve_channel_name(channel, channel_id)
        normalized_oldest = self._normalize_ts(oldest)
        normalized_latest = self._normalize_ts(latest)

        requested_limit = max(1, min(int(limit), self._MAX_PAGE_SIZE))

        def fetch_page(next_cursor: str | None, batch_limit: int) -> dict[str, Any]:
            kwargs: dict[str, Any] = {
                "channel": channel_id,
                "limit": batch_limit,
            }
            if next_cursor:
                kwargs["cursor"] = next_cursor
            if normalized_oldest is not None:
                kwargs["oldest"] = normalized_oldest
            if normalized_latest is not None:
                kwargs["latest"] = normalized_latest
            if normalized_oldest is not None or normalized_latest is not None:
                kwargs["inclusive"] = inclusive
            return self._retry_on_ratelimit(
                self._client.conversations_history,
                method_key="conversations.history",
                **kwargs,
            )

        try:
            raw_messages, next_cursor, has_more = self._collect_cursor_pages(
                fetch_page,
                result_key="messages",
                limit=requested_limit,
                cursor=cursor,
            )
        except SlackApiError as e:
            self._raise_slack_api_error(
                e,
                slack_method="conversations.history",
                access_path="bot_token",
                requested_channel=channel,
                resolved_channel=channel_id,
            )

        messages = [self._serialize_message(msg, channel_id, user_cache) for msg in raw_messages]

        return {
            "channel": channel_name,
            "channel_id": channel_id,
            "messages": messages,
            "count": len(messages),
            "has_more": has_more,
            "next_cursor": next_cursor,
            "window": {
                "oldest": normalized_oldest,
                "latest": normalized_latest,
                "inclusive": inclusive,
            },
            "order": "desc",
        }

    def get_channel_history_proxy(
        self,
        channel_id: str,
        cursor: str | None = None,
        include_all_metadata: bool | None = None,
        inclusive: bool | None = None,
        latest: str | int | float | None = None,
        limit: int | None = None,
        oldest: str | int | float | None = None,
    ) -> dict[str, Any]:
        """Fetch Slack channel history through the Centaur API server proxy.

        This maps to Slack's documented `conversations.history` arguments,
        except `token` is intentionally omitted because the API server supplies
        Slack credentials. `channel_id` must be an explicit Slack conversation
        ID authorized by the principal's `slack.history_channels` claim.
        """
        if not self._api_server_proxy_enabled():
            raise RuntimeError(
                "Slack channel history proxy requires the API server sandbox capability, "
                "but it is disabled for this principal."
            )

        normalized_channel_id = self._clean_channel_ref(channel_id).upper()
        if len(normalized_channel_id) < 9 or not self._looks_like_channel_id(normalized_channel_id):
            raise ValueError("channel_id must be a Slack conversation ID like C123456789")

        params: dict[str, Any] = {
            "cursor": cursor,
            "include_all_metadata": include_all_metadata,
            "inclusive": inclusive,
            "latest": self._normalize_ts(latest),
            "limit": None,
            "oldest": self._normalize_ts(oldest),
        }
        if limit is not None:
            requested_limit = int(limit)
            if not 1 <= requested_limit <= self._MAX_SLACK_HISTORY_PROXY_PAGE_SIZE:
                raise ValueError("limit must be between 1 and 999")
            params["limit"] = requested_limit

        channel_path = urllib.parse.quote(normalized_channel_id, safe="")
        return self._centaur_api_get_json(
            f"/api/slack/channels/{channel_path}/history",
            params,
        )

    def get_thread_replies_proxy(
        self,
        channel_id: str,
        thread_ts: str,
        cursor: str | None = None,
        inclusive: bool | None = None,
        latest: str | int | float | None = None,
        limit: int | None = None,
        oldest: str | int | float | None = None,
    ) -> dict[str, Any]:
        """Fetch Slack thread replies through the Centaur API server."""
        if not self._api_server_proxy_enabled():
            raise RuntimeError(
                "Slack thread replies require the API server sandbox capability, "
                "but it is disabled for this principal."
            )

        normalized_channel_id = self._normalize_explicit_channel_id(channel_id)
        normalized_thread_ts = self._normalize_ts(thread_ts)
        if normalized_thread_ts is None:
            raise ValueError("thread_ts is required")

        params: dict[str, Any] = {
            "cursor": cursor,
            "inclusive": inclusive,
            "latest": self._normalize_ts(latest),
            "limit": None,
            "oldest": self._normalize_ts(oldest),
        }
        if limit is not None:
            requested_limit = int(limit)
            if not 1 <= requested_limit <= self._MAX_SLACK_HISTORY_PROXY_PAGE_SIZE:
                raise ValueError("limit must be between 1 and 999")
            params["limit"] = requested_limit

        channel_path = urllib.parse.quote(normalized_channel_id, safe="")
        thread_path = urllib.parse.quote(normalized_thread_ts, safe="")
        return self._centaur_api_get_json(
            f"/api/slack/channels/{channel_path}/threads/{thread_path}/replies",
            params,
        )

    def get_channel_history(
        self,
        channel: str,
        limit: int = 50,
        cursor: str | None = None,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
        inclusive: bool = False,
    ) -> list[dict]:
        """Get recent messages from a channel or a bounded history window."""
        return self.get_channel_history_page(
            channel=channel,
            limit=limit,
            cursor=cursor,
            oldest=oldest,
            latest=latest,
            inclusive=inclusive,
        )["messages"]

    def get_thread_replies_page(
        self,
        channel: str,
        thread_ts: str,
        limit: int = _DEFAULT_THREAD_REPLY_LIMIT,
        cursor: str | None = None,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
        inclusive: bool = True,
    ) -> dict[str, Any]:
        """Fetch a resumable page of thread replies for ETL-style sync jobs."""
        user_cache = self._get_user_cache()
        channel_id = self._resolve_channel(channel)
        normalized_oldest = self._normalize_ts(oldest)
        normalized_latest = self._normalize_ts(latest)
        normalized_thread_ts = self._normalize_ts(thread_ts)

        if normalized_thread_ts is None:
            raise ValueError("thread_ts is required")

        requested_limit = max(1, min(int(limit), self._MAX_PAGE_SIZE))

        def fetch_page(next_cursor: str | None, batch_limit: int) -> dict[str, Any]:
            kwargs: dict[str, Any] = {
                "channel": channel_id,
                "ts": normalized_thread_ts,
                "limit": batch_limit,
                "inclusive": inclusive,
            }
            if next_cursor:
                kwargs["cursor"] = next_cursor
            if normalized_oldest is not None:
                kwargs["oldest"] = normalized_oldest
            if normalized_latest is not None:
                kwargs["latest"] = normalized_latest
            return self._retry_on_ratelimit(
                self._client.conversations_replies,
                method_key="conversations.replies",
                **kwargs,
            )

        try:
            raw_messages, next_cursor, has_more = self._collect_cursor_pages(
                fetch_page,
                result_key="messages",
                limit=requested_limit,
                cursor=cursor,
            )
        except SlackApiError as e:
            self._raise_slack_api_error(
                e,
                slack_method="conversations.replies",
                access_path="bot_token",
                requested_channel=channel,
                resolved_channel=channel_id,
            )

        messages = [self._serialize_message(msg, channel_id, user_cache) for msg in raw_messages]

        return {
            "channel_id": channel_id,
            "thread_ts": normalized_thread_ts,
            "messages": messages,
            "count": len(messages),
            "requested_limit": limit,
            "effective_limit": requested_limit,
            "has_more": has_more,
            "next_cursor": next_cursor,
            "continuation_available": has_more,
            "window": {
                "oldest": normalized_oldest,
                "latest": normalized_latest,
                "inclusive": inclusive,
            },
            "order": "asc",
        }

    def get_thread_replies(
        self,
        channel_id: str,
        thread_ts: str,
        limit: int = _DEFAULT_THREAD_REPLY_LIMIT,
        cursor: str | None = None,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
        inclusive: bool = True,
    ) -> list[dict]:
        """Get replies in a thread, optionally within a bounded time window."""
        return self.get_thread_replies_page(
            channel=channel_id,
            thread_ts=thread_ts,
            limit=limit,
            cursor=cursor,
            oldest=oldest,
            latest=latest,
            inclusive=inclusive,
        )["messages"]

    def sync_channel_history(
        self,
        channel: str,
        state: dict[str, Any] | None = None,
        limit: int = 200,
        lookback_days: int = 30,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
    ) -> dict[str, Any]:
        """Run a Fivetran-style incremental history sync.

        The first run defaults to a bounded lookback window. Later runs accept a
        `state` payload, reuse its watermark, and re-read the trailing window to
        catch edits or deletes without forcing a full rescan.
        """
        sync_state = dict(state or {})
        cursor = sync_state.get("cursor")
        watermark = self._normalize_ts(sync_state.get("watermark"))
        normalized_oldest = self._normalize_ts(oldest) or sync_state.get("oldest")
        normalized_latest = self._normalize_ts(latest) or sync_state.get("latest")

        if cursor is None and normalized_oldest is None:
            if watermark is not None:
                lookback_seconds = max(lookback_days, 0) * 86400
                normalized_oldest = self._format_ts(max(float(watermark) - lookback_seconds, 0.0))
            elif lookback_days > 0:
                normalized_oldest = self._format_ts(max(time.time() - (lookback_days * 86400), 0.0))

        page = self.get_channel_history_page(
            channel=channel,
            limit=limit,
            cursor=cursor,
            oldest=normalized_oldest,
            latest=normalized_latest,
            inclusive=True,
        )

        latest_seen = watermark
        if page["messages"]:
            latest_seen = self._format_ts(
                max(float(message["timestamp"]) for message in page["messages"])
            )

        next_state: dict[str, Any] = {
            "cursor": page["next_cursor"] if page["has_more"] else None,
            "watermark": latest_seen or watermark,
            "lookback_days": lookback_days,
            "oldest": page["window"]["oldest"] if page["has_more"] else None,
            "latest": page["window"]["latest"] if page["has_more"] else None,
        }

        return {
            **page,
            "sync_state": next_state,
        }

    def list_channels(self, limit: int = 200) -> list[dict]:
        """List Slack channels visible to the bot (public and private).

        Bot-visible channels include any private channel the bot has been
        added to; restricting to ``types="public_channel"`` silently drops
        them. This is the broader-than-membership variant of
        ``list_bot_channels`` and shares the same fix.
        """
        channels = []
        cursor = None

        while True:
            try:
                response = self._retry_on_ratelimit(
                    self._client.conversations_list,
                    method_key="conversations.list",
                    types="public_channel,private_channel",
                    limit=min(limit - len(channels), 200),
                    cursor=cursor,
                    exclude_archived=True,
                )
            except SlackApiError as e:
                self._raise_slack_api_error(
                    e,
                    slack_method="conversations.list",
                    access_path="bot_token",
                )
            except SlackRateLimitError:
                cached = self._load_channel_cache()
                if cached:
                    cached_channels, _ = cached
                    return cached_channels[:limit]
                raise

            for channel in response.get("channels", []):
                channels.append(
                    {
                        "id": channel.get("id", ""),
                        "name": channel.get("name", ""),
                        "purpose": channel.get("purpose", {}).get("value", ""),
                        "topic": channel.get("topic", {}).get("value", ""),
                        "member_count": channel.get("num_members", 0),
                        "is_private": channel.get("is_private", False),
                        "is_member": channel.get("is_member", False),
                    }
                )

            cursor = response.get("response_metadata", {}).get("next_cursor")
            if not cursor or len(channels) >= limit:
                break

        return sorted(channels, key=lambda x: x["name"])

    def list_channels_proxy(self, limit: int = 200, history_only: bool = False) -> list[dict]:
        """List Slack channels exposed by the Centaur API server proxy JWT."""
        if not self._api_server_proxy_enabled():
            raise RuntimeError(
                "Slack channel listing proxy requires the API server sandbox capability, "
                "but it is disabled for this principal."
            )

        response = self._centaur_api_get_json("/api/slack/channels", {})
        channels = response.get("channels", []) or []
        if history_only:
            channels = [channel for channel in channels if channel.get("can_read_history")]
        normalized_channels = [
            {
                "id": channel.get("id", ""),
                "name": channel.get("name", "") or channel.get("id", ""),
                "purpose": channel.get("purpose", ""),
                "topic": channel.get("topic", ""),
                "member_count": channel.get("member_count", 0),
                "is_private": channel.get("is_private", False),
                "is_member": channel.get("is_member", False),
                "can_upload": channel.get("can_upload", False),
                "can_download": channel.get("can_download", False),
                "can_read_history": channel.get("can_read_history", False),
            }
            for channel in channels
        ]
        normalized_channels.sort(key=lambda channel: (channel["name"].lower(), channel["id"]))
        return normalized_channels[:limit]

    def list_files_proxy(
        self,
        channel_id: str,
        limit: int | None = None,
        page: int | None = None,
    ) -> dict[str, Any]:
        """List Slack files through the Centaur API server proxy.

        This maps to Slack's `files.list` for channels authorized by the
        principal's `slack.download_channels` claim.
        """
        if not self._api_server_proxy_enabled():
            raise RuntimeError(
                "Slack file listing proxy requires the API server sandbox capability, "
                "but it is disabled for this principal."
            )

        requested_limit: int | None = None
        if limit is not None:
            requested_limit = int(limit)
            if not 1 <= requested_limit <= self._MAX_SLACK_FILES_LIST_PAGE_SIZE:
                raise ValueError("limit must be between 1 and 200")

        requested_page: int | None = None
        if page is not None:
            requested_page = int(page)
            if requested_page < 1:
                raise ValueError("page must be greater than 0")

        params: dict[str, Any] = {
            "channel_id": self._normalize_explicit_channel_id(channel_id),
            "limit": requested_limit,
            "page": requested_page,
        }
        return self._centaur_api_get_json("/api/slack/files", params)

    def get_channel_members_proxy(self, channel_id: str, limit: int = 1000) -> list[dict]:
        """List Slack channel members through the Centaur API server proxy."""
        if not self._api_server_proxy_enabled():
            raise RuntimeError(
                "Slack channel members proxy requires the API server sandbox capability, "
                "but it is disabled for this principal."
            )

        requested_limit = int(limit)
        if not 1 <= requested_limit <= 10_000:
            raise ValueError("limit must be between 1 and 10000")

        normalized_channel_id = self._normalize_explicit_channel_id(channel_id)
        channel_path = urllib.parse.quote(normalized_channel_id, safe="")
        user_cache = self._get_user_cache()
        member_ids: list[str] = []
        cursor: str | None = None

        while len(member_ids) < requested_limit:
            page_limit = min(requested_limit - len(member_ids), 1000)
            response = self._centaur_api_get_json(
                f"/api/slack/channels/{channel_path}/members",
                {"limit": page_limit, "cursor": cursor},
            )
            member_ids.extend(str(member_id) for member_id in response.get("members", []) or [])
            cursor = response.get("response_metadata", {}).get("next_cursor") or None
            if not cursor:
                break

        return [
            {
                "id": member_id,
                "name": user_cache.get(member_id, member_id),
            }
            for member_id in member_ids[:requested_limit]
        ]

    def list_users(self, limit: int = 200) -> list[dict]:
        """List workspace users."""
        users = []
        cursor = None

        while len(users) < limit:
            try:
                kwargs: dict[str, Any] = {
                    "limit": min(limit - len(users), self._MAX_PAGE_SIZE),
                }
                if cursor:
                    kwargs["cursor"] = cursor
                response = self._retry_on_ratelimit(
                    self._client.users_list,
                    method_key="users.list",
                    **kwargs,
                )
            except SlackApiError as e:
                self._raise_slack_api_error(
                    e,
                    slack_method="users.list",
                    access_path="bot_token",
                )

            for user in response.get("members", []):
                if user.get("deleted"):
                    continue
                profile = user.get("profile", {}) or {}
                users.append(
                    {
                        "id": user.get("id", ""),
                        "name": user.get("name", ""),
                        "real_name": user.get("real_name", ""),
                        "display_name": profile.get("display_name", ""),
                        "email": profile.get("email", ""),
                        "title": profile.get("title", ""),
                        "is_bot": user.get("is_bot", False),
                        "is_deleted": user.get("deleted", False),
                        "team_id": user.get("team_id", "") or user.get("team", ""),
                    }
                )

            cursor = response.get("response_metadata", {}).get("next_cursor")
            if not cursor:
                break

        return sorted(users[:limit], key=lambda x: x["name"])

    def get_channel_members(self, channel: str) -> list[dict]:
        """Get all members of a Slack channel with their user info.

        Args:
            channel: Channel name (without #) or channel ID

        Returns:
            List of member dicts with id, name, real_name, email
        """
        channel_id = self._resolve_channel(channel)

        # Get all member IDs in the channel
        member_ids = []
        cursor = None

        while True:
            try:
                kwargs = {"channel": channel_id, "limit": 200}
                if cursor:
                    kwargs["cursor"] = cursor
                response = self._retry_on_ratelimit(self._client.conversations_members, **kwargs)
            except SlackApiError as e:
                self._raise_slack_api_error(
                    e,
                    slack_method="conversations.members",
                    access_path="bot_token",
                    requested_channel=channel,
                    resolved_channel=channel_id,
                )

            member_ids.extend(response.get("members", []))

            cursor = response.get("response_metadata", {}).get("next_cursor")
            if not cursor:
                break

        # Use bulk user cache instead of fresh API call
        user_cache = self._get_user_cache()

        members = []
        for member_id in member_ids:
            name = user_cache.get(member_id)
            if name:
                members.append(
                    {
                        "id": member_id,
                        "name": name,
                    }
                )

        return members

    def get_channel_member_emails(self, channel: str) -> list[str]:
        """Get email addresses of all non-bot members in a Slack channel.

        Args:
            channel: Channel name (without #) or channel ID

        Returns:
            List of email addresses (excludes members without email)
        """
        members = self.get_channel_members(channel)
        return [m["email"] for m in members if m.get("email")]

    def get_user_email(self, user_id: str) -> str | None:
        """Get a user's email address by their Slack user ID.

        Args:
            user_id: Slack user ID (e.g., 'U123ABC')

        Returns:
            Email address or None if not found
        """
        try:
            response = self._client.users_info(user=user_id)
            user = response.get("user", {})
            return user.get("profile", {}).get("email")
        except SlackApiError:
            return None

    def get_user_profile(self, user_id: str) -> dict:
        """Get a user's full Slack profile by their user ID.

        Returns profile fields including name, email, title, phone, status,
        timezone, and custom profile fields (e.g. Telegram, Skype, pronouns).

        Args:
            user_id: Slack user ID (e.g., 'U123ABC')

        Returns:
            Dict with id, name, real_name, display_name, email, title, phone,
            status_text, status_emoji, timezone, tz_label, image, skype,
            and custom_fields (dict of label -> value for non-empty custom fields)
        """
        try:
            user_response = self._retry_on_ratelimit(
                self._client.users_info,
                user=user_id,
                method_key="users.info",
            )
            profile_response = self._retry_on_ratelimit(
                self._client.users_profile_get,
                user=user_id,
                include_labels=True,
                method_key="users.profile.get",
            )
        except SlackApiError as e:
            self._raise_slack_api_error(
                e,
                slack_method="users.profile.get",
                access_path="bot_token",
            )

        user = user_response.get("user", {})
        profile = profile_response.get("profile", {}) or user.get("profile", {})

        # Extract custom profile fields (Telegram, Skype, pronouns, etc.)
        custom_fields: dict[str, str] = {}
        raw_custom_fields: dict[str, dict] = {}
        for field_id, field_data in (profile.get("fields") or {}).items():
            label = field_data.get("label") or field_id
            value = field_data.get("value")
            if value:
                custom_fields[label] = value
                raw_custom_fields[field_id] = dict(field_data)

        return {
            "id": user.get("id", ""),
            "name": user.get("name", ""),
            "real_name": user.get("real_name", ""),
            "display_name": profile.get("display_name", ""),
            "email": profile.get("email", ""),
            "title": profile.get("title", ""),
            "phone": profile.get("phone", ""),
            "status_text": profile.get("status_text", ""),
            "status_emoji": profile.get("status_emoji", ""),
            "timezone": user.get("tz", ""),
            "tz_label": user.get("tz_label", ""),
            "image": profile.get("image_192", ""),
            "skype": profile.get("skype", ""),
            "is_bot": user.get("is_bot", False),
            "deleted": user.get("deleted", False),
            "custom_fields": custom_fields,
            "raw_custom_fields": raw_custom_fields,
        }

    def _format_requester_attribution(self) -> str:
        """Get requester attribution from environment variables.

        When running inside the agent container, SLACK_REQUESTER_ID and SLACK_REQUESTER_NAME
        are set to identify who requested the work.

        Returns:
            Attribution string like "_(requested by <@U123>)_" or empty string.
        """
        requester_id = os.getenv("SLACK_REQUESTER_ID")  # noqa: TID251
        requester_name = os.getenv("SLACK_REQUESTER_NAME")  # noqa: TID251

        if requester_id:
            return f"\n\n_(requested by <@{requester_id}>)_"
        elif requester_name:
            return f"\n\n_(requested by @{requester_name})_"
        return ""

    @staticmethod
    def _normalize_message_text(text: str) -> str:
        """Convert shell-friendly escaped line breaks into Slack line breaks."""
        return text.replace("\\r\\n", "\n").replace("\\n", "\n").replace("\\r", "\r")

    def send_message(
        self,
        channel: str,
        text: str,
        thread_ts: str | None = None,
        no_attribution: bool = False,
        blocks: list | None = None,
        unfurl_links: bool | None = None,
        unfurl_media: bool | None = None,
    ) -> dict:
        """Send a message to a channel or Slack user DM.

        Args:
            channel: Channel name (with or without #), channel ID, or Slack user ID
            text: Message text to send
            thread_ts: Optional thread timestamp to reply in thread
            no_attribution: If True, skip adding requester attribution
            blocks: Optional Slack Block Kit blocks for rich formatting
            unfurl_links: Override Slack's link unfurl behavior for this message
            unfurl_media: Override Slack's media unfurl behavior for this message

        Returns:
            Dict with channel, ts, permalink
        """
        channel_id = self._resolve_message_destination(channel)

        message_text = self._normalize_message_text(text)
        if not no_attribution:
            attribution = self._format_requester_attribution()
            if attribution:
                message_text += attribution

        try:
            kwargs = {"channel": channel_id, "text": message_text}
            if thread_ts:
                kwargs["thread_ts"] = thread_ts
            if blocks:
                kwargs["blocks"] = blocks
            if unfurl_links is not None:
                kwargs["unfurl_links"] = unfurl_links
            if unfurl_media is not None:
                kwargs["unfurl_media"] = unfurl_media
            response = self._client.chat_postMessage(**kwargs)
            return {
                "channel": channel_id,
                "ts": response.get("ts", ""),
                "permalink": f"https://slack.com/archives/{channel_id}/p{response.get('ts', '').replace('.', '')}",
            }
        except SlackApiError as e:
            raise RuntimeError(f"Slack API error: {e.response['error']}") from e

    def send_dm(
        self,
        user_id: str,
        text: str,
        no_attribution: bool = False,
        blocks: list | None = None,
        unfurl_links: bool | None = None,
        unfurl_media: bool | None = None,
    ) -> dict:
        """Send a direct message to a Slack user.

        Args:
            user_id: Slack user ID, e.g. U123 or <@U123>
            text: Message text to send
            no_attribution: If True, skip adding requester attribution
            blocks: Optional Slack Block Kit blocks for rich formatting
            unfurl_links: Override Slack's link unfurl behavior for this message
            unfurl_media: Override Slack's media unfurl behavior for this message

        Returns:
            Dict with channel, ts, permalink
        """
        return self.send_message(
            user_id,
            text,
            no_attribution=no_attribution,
            blocks=blocks,
            unfurl_links=unfurl_links,
            unfurl_media=unfurl_media,
        )

    @staticmethod
    def _preview_for_bytes(data: bytes, filename: str) -> dict[str, Any]:
        """Return cheap metadata that helps identify generated artifacts."""
        preview: dict[str, Any] = {"size_bytes": len(data)}
        mime_type, _ = mimetypes.guess_type(filename)
        if mime_type:
            preview["mime_type"] = mime_type
        suffix = Path(filename).suffix.lower()
        if suffix == ".csv":
            sample = data[:64_000].decode("utf-8", errors="replace")
            lines = [line for line in sample.splitlines() if line.strip()]
            if lines:
                preview["csv_rows_sampled"] = max(len(lines) - 1, 0)
                preview["csv_columns"] = len(lines[0].split(","))
        elif suffix in {".png", ".jpg", ".jpeg", ".gif", ".webp"}:
            preview["file_type"] = "image"
        elif suffix == ".pdf" and data.startswith(b"%PDF"):
            preview["file_type"] = "pdf"
        elif suffix in {".mp4", ".mov", ".webm"}:
            preview["file_type"] = "video"
        return preview

    @staticmethod
    def _file_shared_in_thread(shares: dict, channel: str, thread_ts: str | None) -> bool:
        """Whether files.info `shares` shows the file landed in the target.

        Slack's schema is ``shares.{public,private}[channel_id] -> [entries]``,
        where each entry has the share-message ``ts`` and (for a threaded
        share) ``thread_ts`` of the parent. The file is in our thread if any
        entry for the channel matches our ``thread_ts`` (on either field, since
        a root-level share reports ``ts == thread_ts``). With no thread
        requested, any share into the channel counts.
        """
        entries: list = []
        for scope in ("public", "private"):
            by_channel = shares.get(scope) if isinstance(shares, dict) else None
            if isinstance(by_channel, dict):
                entries.extend(by_channel.get(channel) or [])
        if not entries:
            return False
        if thread_ts is None:
            return True
        return any(
            isinstance(e, dict) and thread_ts in (e.get("thread_ts"), e.get("ts")) for e in entries
        )

    def upload_file(
        self,
        channel: str | None = None,
        channel_id: str | None = None,
        title: str | None = None,
        comment: str | None = None,
        thread_ts: str | None = None,
        content_base64: str | None = None,
        filename: str | None = None,
        alt_text: str | None = None,
    ) -> dict:
        """Upload a file to Slack.

        Accepts file bytes as base64 content.
        There is deliberately no local-path option: this tool runs in-process
        on the API server, so a caller-supplied path would read the API host's
        filesystem (secrets, configs) and exfiltrate it to Slack.

        channel/channel_id and thread_ts are required. The upload tool does not
        infer the active Slack thread; callers must pass the API-owned Slack
        channel ID and thread timestamp explicitly.

        alt_text: accepted for backwards compatibility but currently NOT sent
            to Slack, because slack_sdk's files_upload_v2 mishandles alt_txt
            (https://github.com/slackapi/python-slack-sdk/issues/1818).
        """
        requested_channel = channel or channel_id
        if not requested_channel:
            raise ValueError("channel is required; pass channel/channel_id explicitly")
        if not thread_ts:
            raise ValueError("thread_ts is required; pass the Slack thread timestamp explicitly")
        resolved_channel = self._resolve_channel(requested_channel)
        effective_thread_ts = thread_ts

        try:
            kwargs = {
                "channel": resolved_channel,
            }
            upload_bytes: bytes | None = None
            effective_filename = filename
            if content_base64:
                upload_bytes = base64.b64decode(content_base64)
                effective_filename = filename or "upload.png"
            else:
                raise ValueError("content_base64 is required")
            # Pass bytes via `file=` (binary upload) rather than `content=`,
            # which slack_sdk treats as snippet text.
            kwargs["file"] = upload_bytes
            kwargs["filename"] = effective_filename
            if title:
                kwargs["title"] = title
            elif effective_filename:
                kwargs["title"] = effective_filename
            preview = self._preview_for_bytes(
                upload_bytes or b"", effective_filename or "upload.bin"
            )
            if comment:
                kwargs["initial_comment"] = comment
            elif effective_filename:
                kwargs["initial_comment"] = f"Uploaded `{effective_filename}`."
            if effective_thread_ts:
                kwargs["thread_ts"] = effective_thread_ts
            # We intentionally do NOT forward alt_text to Slack. Passing alt_txt
            # through slack_sdk's files_upload_v2 is broken and can cause the
            # upload/share to misbehave — see
            # https://github.com/slackapi/python-slack-sdk/issues/1818. The
            # parameter is kept on the signature for backwards compatibility but
            # is deliberately ignored until the SDK bug is resolved.
            _ = alt_text

            # Upload once, then poll files.info with an exponential backoff
            # (0/1/2/4/8s) to let the async share propagate. On a suspected
            # drop, log the full files.info file object so the share state is
            # visible.
            response = self._client.files_upload_v2(**kwargs)
            file_info = response.get("file", {})
            file_id = file_info.get("id", "")

            landed = False
            verify_failed = False
            info_file: dict = {}
            for delay in (0, 1, 2, 4, 8):
                if delay:
                    time.sleep(delay)
                try:
                    info = self._client.files_info(file=file_id)
                except Exception:
                    # Verification unavailable (e.g. missing files:read scope).
                    verify_failed = True
                    break
                info_file = info.get("file", {}) if info else {}
                landed = self._file_shared_in_thread(
                    info_file.get("shares") or {},
                    resolved_channel,
                    effective_thread_ts,
                )
                if landed:
                    break

            if not landed and not verify_failed:
                logger.warning(
                    "slack_upload_file_share_dropped",
                    file_id=file_id,
                    channel=resolved_channel,
                    thread_ts=effective_thread_ts,
                    files_info=info_file,
                )

            return {
                "id": file_id,
                "name": file_info.get("name", ""),
                "permalink": file_info.get("permalink", ""),
                "url": file_info.get("url_private", ""),
                "preview": preview,
            }
        except SlackApiError as e:
            self._raise_slack_api_error(
                e,
                slack_method="files.upload_v2",
                access_path="file_upload",
                requested_channel=requested_channel,
                resolved_channel=resolved_channel,
            )

    def upload_file_proxy(
        self,
        channel_id: str,
        content_base64: str,
        filename: str,
        thread_ts: str | None = None,
        title: str | None = None,
        initial_comment: str | None = None,
        content_type: str | None = None,
        alt_txt: str | None = None,
        snippet_type: str | None = None,
    ) -> dict[str, Any]:
        """Upload a file through the Centaur API server Slack proxy.

        This maps to `/api/slack/files/upload`. The caller supplies file bytes
        as base64, and the API server handles Slack credentials and channel
        authorization through the principal-scoped JWT.
        """
        if not self._api_server_proxy_enabled():
            raise RuntimeError(
                "Slack file upload proxy requires the API server sandbox capability, "
                "but it is disabled for this principal."
            )
        normalized_channel_id = self._normalize_explicit_channel_id(channel_id)
        effective_filename = str(filename).strip()
        if not effective_filename:
            raise ValueError("filename is required")
        try:
            body = base64.b64decode(content_base64, validate=True)
        except (binascii.Error, ValueError) as exc:
            raise ValueError("content_base64 must be valid base64") from exc
        if not body:
            raise ValueError("content_base64 must not be empty")

        params = {
            "channel_id": normalized_channel_id,
            "filename": effective_filename,
            "thread_ts": self._normalize_ts(thread_ts),
            "title": title,
            "initial_comment": initial_comment,
            "content_type": content_type,
            "alt_txt": alt_txt,
            "snippet_type": snippet_type,
        }
        return self._centaur_api_post_bytes_json(
            "/api/slack/files/upload",
            params,
            body,
            content_type or "application/octet-stream",
        )

    def download_file_proxy(self, file_id: str, channel_id: str) -> dict[str, Any]:
        """Download a Slack file through the Centaur API server Slack proxy.

        Returns base64-encoded file bytes plus filename, content type, and size.
        """
        if not self._api_server_proxy_enabled():
            raise RuntimeError(
                "Slack file download proxy requires the API server sandbox capability, "
                "but it is disabled for this principal."
            )
        normalized_file_id = self._normalize_file_id(file_id)
        normalized_channel_id = self._normalize_explicit_channel_id(channel_id)
        body, headers = self._centaur_api_get_bytes(
            f"/api/slack/files/{urllib.parse.quote(normalized_file_id, safe='')}/download",
            {"channel_id": normalized_channel_id},
            self._MAX_DOWNLOAD_BYTES,
        )
        filename = (
            self._content_disposition_filename(headers.get("content-disposition"))
            or normalized_file_id
        )
        content_type = headers.get("content-type") or "application/octet-stream"
        return {
            "file_id": normalized_file_id,
            "channel_id": normalized_channel_id,
            "filename": filename,
            "content_type": content_type,
            "size_bytes": len(body),
            "content_base64": base64.b64encode(body).decode(),
        }

    def file_info_proxy(self, file_id: str, channel_id: str) -> dict[str, Any]:
        """Fetch Slack file metadata through the Centaur API server proxy."""
        if not self._api_server_proxy_enabled():
            raise RuntimeError(
                "Slack file info proxy requires the API server sandbox capability, "
                "but it is disabled for this principal."
            )
        normalized_file_id = self._normalize_file_id(file_id)
        normalized_channel_id = self._normalize_explicit_channel_id(channel_id)
        return self._centaur_api_get_json(
            f"/api/slack/files/{urllib.parse.quote(normalized_file_id, safe='')}/info",
            {"channel_id": normalized_channel_id},
        )

    def list_usergroups(self) -> list[dict]:
        """List all user groups in the workspace."""
        try:
            response = self._client.usergroups_list(include_users=True)
        except SlackApiError as e:
            self._raise_slack_api_error(
                e,
                slack_method="usergroups.list",
                access_path="bot_token",
            )

        groups = []
        for group in response.get("usergroups", []):
            groups.append(
                {
                    "id": group.get("id", ""),
                    "handle": group.get("handle", ""),
                    "name": group.get("name", ""),
                    "description": group.get("description", ""),
                    "users": group.get("users", []),
                    "user_count": len(group.get("users", [])),
                }
            )

        return sorted(groups, key=lambda x: x["handle"])

    def create_usergroup(
        self, handle: str, name: str, description: str = "", user_ids: list[str] | None = None
    ) -> dict:
        """Create a new user group."""
        try:
            response = self._client.usergroups_create(
                name=name,
                handle=handle,
                description=description,
            )
            group = response.get("usergroup", {})
            group_id = group.get("id")

            if user_ids and group_id:
                self._client.usergroups_users_update(usergroup=group_id, users=",".join(user_ids))

            return {
                "id": group_id,
                "handle": group.get("handle", ""),
                "name": group.get("name", ""),
            }
        except SlackApiError as e:
            raise RuntimeError(f"Slack API error: {e.response['error']}") from e

    def update_usergroup_users(self, group_id_or_handle: str, user_ids: list[str]) -> dict:
        """Update users in an existing user group."""
        group_id = group_id_or_handle
        if not group_id.startswith("S"):
            groups = self.list_usergroups()
            for g in groups:
                if g["handle"] == group_id_or_handle:
                    group_id = g["id"]
                    break
            else:
                raise RuntimeError(f"User group '@{group_id_or_handle}' not found")

        try:
            response = self._client.usergroups_users_update(
                usergroup=group_id, users=",".join(user_ids)
            )
            group = response.get("usergroup", {})
            return {
                "id": group.get("id", ""),
                "handle": group.get("handle", ""),
                "name": group.get("name", ""),
                "users": response.get("users", user_ids),
            }
        except SlackApiError as e:
            raise RuntimeError(f"Slack API error: {e.response['error']}") from e

    def get_message_files(self, channel_id: str, message_ts: str) -> list[dict]:
        """Get files attached to a specific message."""
        try:
            response = self._client.conversations_replies(
                channel=channel_id,
                ts=message_ts,
                limit=1,
                inclusive=True,
            )
        except SlackApiError as e:
            self._raise_slack_api_error(
                e,
                slack_method="conversations.replies",
                access_path="bot_token",
                requested_channel=channel_id,
                resolved_channel=channel_id,
            )

        messages = response.get("messages", [])
        if not messages:
            return []

        msg = messages[0]
        files = []
        for f in msg.get("files", []):
            files.append(
                {
                    "id": f.get("id", ""),
                    "name": f.get("name", ""),
                    "title": f.get("title", ""),
                    "mimetype": f.get("mimetype", ""),
                    "filetype": f.get("filetype", ""),
                    "url_private": f.get("url_private", ""),
                    "size": f.get("size", 0),
                }
            )

        return files

    # Slack file downloads buffer the file in memory before writing it, so cap the
    # size regardless of Slack's own (much larger) per-file limit.
    _MAX_DOWNLOAD_BYTES = 10 * 1024 * 1024

    def _fetch_slack_file(self, url: str) -> tuple[str, str, bytes]:
        """Download a Slack file's bytes: returns ``(filename, mime_type, body)``.

        ``url`` must be an ``https://files.slack.com/`` URL. The bot token is
        sent only to that host, so it can never be aimed at a Slack API
        endpoint (e.g. api.test) that would echo the credential back.
        """
        if not self.token:
            raise RuntimeError("SLACK_BOT_TOKEN not set")

        parsed = urlparse(url)
        if parsed.scheme != "https" or (parsed.hostname or "").lower() != "files.slack.com":
            raise ValueError(
                f"Slack file downloads only accept https://files.slack.com/ URLs; refusing {url!r}"
            )

        req = urllib.request.Request(url, headers={"Authorization": f"Bearer {self.token}"})
        with urllib.request.urlopen(req) as response:
            # Read one byte past the cap so an oversized file is rejected
            # without buffering an unbounded response.
            body = response.read(self._MAX_DOWNLOAD_BYTES + 1)
            mime_type = response.headers.get_content_type()
        if len(body) > self._MAX_DOWNLOAD_BYTES:
            raise ValueError(
                f"Slack file exceeds the {self._MAX_DOWNLOAD_BYTES}-byte download limit"
            )

        return Path(parsed.path).name or "slack-file", mime_type, body

    def search_files(
        self,
        channel_id: str,
        query: str,
        max_results: int = 20,
    ) -> list[dict]:
        """Search files in one Slack channel using files.list with metadata filter.

        Note: search.files requires a user token. This uses files.list as a
        bot-token-compatible alternative that filters by filename/type.

        Args:
            channel_id: Slack channel ID to search
            query: Search query string (matches against filenames)
            max_results: Maximum results to return

        Returns:
            List of file dicts with id, name, title, filetype, user, channels, permalink
        """
        requested_limit = max(1, int(max_results))
        if not self._api_server_proxy_enabled():
            raise RuntimeError(
                "Slack file listing proxy requires the API server sandbox capability, "
                "but it is disabled for this principal."
            )
        results: list[dict] = []
        user_cache = self._get_user_cache()
        page_limit = self._MAX_SLACK_FILES_LIST_PAGE_SIZE
        page = 1
        while len(results) < requested_limit:
            response = self.list_files_proxy(
                channel_id=channel_id,
                limit=page_limit,
                page=page,
            )
            results.extend(self._filter_file_search_results(response, query, user_cache))
            if not response.get("has_more"):
                break
            page += 1
        return results[:requested_limit]

    def search_files_direct(
        self,
        query: str,
        max_results: int = 20,
    ) -> list[dict]:
        """Search files directly through Slack's `files.list` API."""
        requested_limit = max(1, int(max_results))
        user_cache = self._get_user_cache()
        results: list[dict] = []
        page = 1
        while len(results) < requested_limit:
            try:
                response = self._retry_on_ratelimit(
                    self._client.files_list,
                    count=self._MAX_SLACK_FILES_LIST_PAGE_SIZE,
                    page=page,
                )
            except SlackApiError as e:
                self._raise_slack_api_error(
                    e,
                    slack_method="files.list",
                    access_path="bot_token",
                )
            results.extend(self._filter_file_search_results(response, query, user_cache))
            if not self._files_list_response_has_more(response):
                break
            page += 1
        return results[:requested_limit]

    def _files_list_response_has_more(self, response: dict) -> bool:
        """Return whether a Slack files.list response has another page."""
        paging = response.get("paging") or {}
        try:
            return int(paging.get("page", 0)) < int(paging.get("pages", 0))
        except (TypeError, ValueError):
            return False

    def _filter_file_search_results(
        self,
        response: dict,
        query: str,
        user_cache: dict[str, str],
    ) -> list[dict]:
        """Filter a files.list response by filename/title and normalize rows."""
        files = response.get("files", [])
        query_lower = query.lower()

        results = []
        for f in files:
            name = f.get("name", "")
            title = f.get("title", "")
            if query_lower and query_lower not in name.lower() and query_lower not in title.lower():
                continue
            user_id = f.get("user", "")
            results.append(
                {
                    "id": f.get("id", ""),
                    "name": name,
                    "title": title,
                    "filetype": f.get("filetype", ""),
                    "size": f.get("size", 0),
                    "user": user_cache.get(user_id, user_id),
                    "channels": f.get("channels", []),
                    "permalink": f.get("permalink", ""),
                    "url_private": f.get("url_private", ""),
                    "created": f.get("created", 0),
                }
            )

        return results

    def search_users(
        self,
        query: str,
        max_results: int = 20,
    ) -> list[dict]:
        """Search workspace users by name, email, or title.

        Uses users.list with local filtering. The users:read.email scope
        ensures email addresses are included in results.

        Args:
            query: Search string to match against name, real_name, email, or title
            max_results: Maximum results to return

        Returns:
            List of user dicts with id, name, real_name, email, title, timezone
        """
        all_users = self.list_users(limit=1000)
        query_lower = query.lower()

        matches = []
        for u in all_users:
            searchable = f"{u['name']} {u['real_name']} {u['email']} {u['title']}".lower()
            if query_lower in searchable:
                matches.append(u)

        return matches[:max_results]

    def dump_channel_with_threads(
        self,
        channel_name: str,
        limit: int = _DEFAULT_DUMP_MESSAGE_LIMIT,
        min_replies: int = 0,
        cursor: str | None = None,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
        replies_limit: int = _DEFAULT_THREAD_REPLY_LIMIT,
        max_threads: int = _DEFAULT_DUMP_THREAD_LIMIT,
    ) -> dict:
        """Dump a bounded channel history page with thread replies expanded.

        Args:
            channel_name: Channel name (without #)
            limit: Maximum messages to fetch from channel
            min_replies: Only include threads with >= this many replies (0 = all)
            max_threads: Maximum thread reply lookups to expand for this page

        Returns:
            Dict with channel info, messages (with replies inline), and stats
        """
        requested_limit = max(1, min(int(limit), self._MAX_PAGE_SIZE))
        requested_replies_limit = max(1, min(int(replies_limit), self._MAX_PAGE_SIZE))
        max_threads = max(0, int(max_threads))
        page = self.get_channel_history_page(
            channel_name,
            limit=requested_limit,
            cursor=cursor,
            oldest=oldest,
            latest=latest,
            inclusive=True,
        )
        channel_id = page["channel_id"]

        all_messages = []
        expanded_threads = 0
        skipped_threads = 0
        for msg in page["messages"]:
            ts = msg["timestamp"]
            reply_count = msg.get("reply_count", 0)
            thread_ts = msg.get("thread_ts") or ts

            message_data = {
                **msg,
                "replies": [],
                "replies_has_more": False,
                "replies_next_cursor": None,
            }

            if reply_count > 0 and (min_replies == 0 or reply_count >= min_replies):
                if expanded_threads >= max_threads:
                    skipped_threads += 1
                    message_data["replies_has_more"] = True
                    message_data["replies_next_cursor"] = "not_fetched_thread_limit"
                else:
                    try:
                        thread_page = self.get_thread_replies_page(
                            channel=channel_id,
                            thread_ts=thread_ts,
                            limit=requested_replies_limit,
                        )
                        message_data["replies"] = thread_page["messages"][1:]
                        message_data["replies_has_more"] = thread_page["has_more"]
                        message_data["replies_next_cursor"] = thread_page["next_cursor"]
                        expanded_threads += 1
                    except RuntimeError:
                        pass

            all_messages.append(message_data)

        threads_with_replies = sum(1 for m in all_messages if m["replies"])
        total_replies = sum(len(m["replies"]) for m in all_messages)

        return {
            "channel": page["channel"],
            "channel_id": channel_id,
            "messages": all_messages,
            "has_more": page["has_more"],
            "next_cursor": page["next_cursor"],
            "continuation_available": bool(page["has_more"] or skipped_threads),
            "window": page["window"],
            "limits": {
                "message_limit": requested_limit,
                "reply_limit": requested_replies_limit,
                "thread_limit": max_threads,
            },
            "stats": {
                "total_messages": len(all_messages),
                "threads_fetched": threads_with_replies,
                "threads_expanded": expanded_threads,
                "threads_skipped_by_limit": skipped_threads,
                "total_replies": total_replies,
            },
        }

    def close(self):
        """Close the underlying HTTP session."""
        pass  # WebClient doesn't need explicit close


def _client() -> SlackClient:
    from centaur_sdk import secret

    return SlackClient(
        bot_token=secret("SLACK_BOT_TOKEN"),
        search_token=secret("SLACK_SEARCH_TOKEN", ""),
    )


def get_slack_client() -> SlackClient:
    """Get a cached Slack client instance for CLI compatibility."""
    return _client()


def _retry_on_ratelimit(func, *args, **kwargs):
    return _client()._retry_on_ratelimit(func, *args, **kwargs)


def get_user_cache(client: SlackClient | None = None) -> dict[str, str]:
    slack_client = client or _client()
    return slack_client._get_user_cache()


def list_bot_channels(*args, **kwargs):
    return _client().list_bot_channels(*args, **kwargs)


def resolve_mentions(
    text: str, client: SlackClient | None = None, user_cache: dict[str, str] | None = None
) -> str:
    slack_client = client or _client()
    resolved_user_cache = user_cache or slack_client._get_user_cache()
    return slack_client._resolve_mentions(text, resolved_user_cache)


def search_messages(*args, **kwargs):
    return _client().search_messages(*args, **kwargs)


def get_channel_history_page(*args, **kwargs):
    return _client().get_channel_history_page(*args, **kwargs)


def get_channel_history_proxy(*args, **kwargs):
    return _client().get_channel_history_proxy(*args, **kwargs)


def get_thread_replies_proxy(*args, **kwargs):
    return _client().get_thread_replies_proxy(*args, **kwargs)


def get_channel_history(*args, **kwargs):
    return _client().get_channel_history(*args, **kwargs)


def get_thread_replies_page(*args, **kwargs):
    return _client().get_thread_replies_page(*args, **kwargs)


def get_thread_replies(*args, **kwargs):
    return _client().get_thread_replies(*args, **kwargs)


def sync_channel_history(*args, **kwargs):
    return _client().sync_channel_history(*args, **kwargs)


def list_channels(*args, **kwargs):
    return _client().list_channels(*args, **kwargs)


def list_channels_proxy(*args, **kwargs):
    return _client().list_channels_proxy(*args, **kwargs)


def list_files_proxy(*args, **kwargs):
    return _client().list_files_proxy(*args, **kwargs)


def list_users(*args, **kwargs):
    return _client().list_users(*args, **kwargs)


def get_channel_members(*args, **kwargs):
    return _client().get_channel_members(*args, **kwargs)


def get_channel_members_proxy(*args, **kwargs):
    return _client().get_channel_members_proxy(*args, **kwargs)


def get_channel_member_emails(*args, **kwargs):
    return _client().get_channel_member_emails(*args, **kwargs)


def get_user_email(*args, **kwargs):
    return _client().get_user_email(*args, **kwargs)


def send_message(*args, **kwargs):
    return _client().send_message(*args, **kwargs)


def send_dm(*args, **kwargs):
    return _client().send_dm(*args, **kwargs)


def upload_file(*args, **kwargs):
    return _client().upload_file(*args, **kwargs)


def upload_file_proxy(*args, **kwargs):
    return _client().upload_file_proxy(*args, **kwargs)


def download_file_proxy(*args, **kwargs):
    return _client().download_file_proxy(*args, **kwargs)


def file_info_proxy(*args, **kwargs):
    return _client().file_info_proxy(*args, **kwargs)


def list_usergroups(*args, **kwargs):
    return _client().list_usergroups(*args, **kwargs)


def create_usergroup(*args, **kwargs):
    return _client().create_usergroup(*args, **kwargs)


def update_usergroup_users(*args, **kwargs):
    return _client().update_usergroup_users(*args, **kwargs)


def get_message_files(*args, **kwargs):
    return _client().get_message_files(*args, **kwargs)


def _fetch_slack_file(*args, **kwargs):
    return _client()._fetch_slack_file(*args, **kwargs)


def dump_channel_with_threads(*args, **kwargs):
    return _client().dump_channel_with_threads(*args, **kwargs)


def search_files(*args, **kwargs):
    return _client().search_files(*args, **kwargs)


def search_files_direct(*args, **kwargs):
    return _client().search_files_direct(*args, **kwargs)


def search_users(*args, **kwargs):
    return _client().search_users(*args, **kwargs)


def get_user_profile(*args, **kwargs):
    return _client().get_user_profile(*args, **kwargs)
