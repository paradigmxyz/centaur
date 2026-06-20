"""Automated feedback collection and analysis for bot improvement.

Collects feedback from Slack channels where the bot operates, identifies issues,
and generates actionable improvements for SYSTEM_AGENTS.md and CLIs.
"""

import json
import os
import re
import sqlite3
import urllib.error
import urllib.request
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from slack_sdk import WebClient
from slack_sdk.errors import SlackApiError

from .client import (
    _retry_on_ratelimit,
    get_slack_client,
    get_user_cache,
    list_bot_channels,
    resolve_mentions,
)

# Feedback database location
FEEDBACK_DB_PATH = Path.home() / ".cache" / "paradigm-slack" / "feedback.db"

# Heuristic signals for feedback detection
NEGATIVE_REACTIONS = {"thumbsdown", "-1", "x", "confused", "thinking_face", "bug", "facepalm"}
POSITIVE_REACTIONS = {"thumbsup", "+1", "white_check_mark", "fire", "heart", "tada", "rocket"}
NEGATIVE_KEYWORDS = [
    "wrong",
    "broken",
    "doesn't work",
    "didn't work",
    "should have",
    "why didn't",
    "failed",
    "error",
    "not what i",
    "that's not",
    "incorrect",
    "try again",
    "still wrong",
    "can't find",
    "unable to",
]
POSITIVE_KEYWORDS = ["perfect", "worked", "thanks", "great", "exactly", "awesome", "nice"]

# Pattern to match Amp thread IDs
AMP_THREAD_PATTERN = re.compile(
    r"T-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", re.I
)
BOT_ERROR_PATTERNS = [
    re.compile(r"\berror:", re.I),
    re.compile(r"\bfailed:", re.I),
    re.compile(r"\bexception\b", re.I),
    re.compile(r"\btimeout(?:ms|s)?\b", re.I),
    re.compile(r"\bcontainer exited\b", re.I),
    re.compile(r"\btool not found\b", re.I),
    re.compile(
        r"\b(?:could not|couldn't|was not able to|wasn't able to|unable to)\b.*\b(parse|process|load|open|read|find|download)\b",
        re.I,
    ),
]


@dataclass
class FeedbackSignals:
    """Signals extracted from a thread indicating feedback type."""

    has_negative_reaction: bool = False
    has_positive_reaction: bool = False
    negative_keywords_found: list[str] = field(default_factory=list)
    positive_keywords_found: list[str] = field(default_factory=list)
    user_message_count: int = 0
    bot_message_count: int = 0
    has_bot_error: bool = False
    repeated_requests: bool = False  # User had to rephrase >2 times


@dataclass
class FeedbackItem:
    """A structured feedback item."""

    id: int | None
    slack_channel: str
    slack_thread_ts: str
    permalink: str
    amp_thread_id: str | None
    category: str  # cli_bug, routing_error, missing_capability, success, unclear
    severity: str  # low, medium, high
    summary: str
    cli_involved: str | None
    evidence: dict[str, Any]
    reporter_user: str
    status: str  # new, triaged, in_progress, fixed, wontfix
    created_at: str
    updated_at: str


@dataclass
class SaveFeedbackResult:
    """Result for save_feedback_item."""

    item_id: int
    inserted: bool


class CentaurAgentClient:
    """Minimal client for starting a background improvement agent run."""

    def __init__(self, base_url: str | None = None, api_key: str | None = None):
        self.base_url = (base_url or os.environ.get("CENTAUR_API_URL") or "http://api:8000").rstrip("/")
        self.api_key = api_key or _load_centaur_api_key()
        if not self.api_key:
            raise RuntimeError("CENTAUR_AGENT_API_KEY not set")

    def _request_json(self, method: str, path: str, payload: dict[str, Any] | None = None) -> dict[str, Any]:
        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Accept": "application/json",
        }
        data: bytes | None = None
        if payload is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(payload).encode("utf-8")

        request = urllib.request.Request(
            f"{self.base_url}{path}",
            data=data,
            headers=headers,
            method=method,
        )
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                raw = response.read().decode("utf-8")
                return json.loads(raw) if raw else {}
        except urllib.error.HTTPError as exc:
            raw = exc.read().decode("utf-8", errors="replace")
            detail = raw
            try:
                body = json.loads(raw)
                detail = body.get("message") or body.get("detail") or raw
            except json.JSONDecodeError:
                pass
            raise RuntimeError(f"Centaur API error {exc.code} on {path}: {detail}") from exc

    def start_improvement_run(
        self,
        prompt: str,
        *,
        harness: str = "amp",
        persona_id: str = "eng",
        thread_key: str | None = None,
    ) -> dict[str, Any]:
        """Spawn, message, and execute a background improvement agent run."""
        thread_key = thread_key or f"feedback-improvement:{datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')}:{uuid.uuid4().hex[:8]}"
        spawn = self._request_json(
            "POST",
            "/agent/spawn",
            {
                "thread_key": thread_key,
                "harness": harness,
                "persona_id": persona_id,
            },
        )
        assignment_generation = spawn["assignment_generation"]

        self._request_json(
            "POST",
            "/agent/message",
            {
                "thread_key": thread_key,
                "assignment_generation": assignment_generation,
                "role": "user",
                "parts": [{"type": "text", "text": prompt}],
                "metadata": {"source": "slack-feedback-loop"},
            },
        )

        execute = self._request_json(
            "POST",
            "/agent/execute",
            {
                "thread_key": thread_key,
                "assignment_generation": assignment_generation,
                "execute_id": f"feedback-improvement-{uuid.uuid4().hex[:12]}",
                "harness": harness,
                "delivery": {"platform": "dev"},
                "metadata": {"source": "slack-feedback-loop"},
            },
        )

        return {
            "thread_key": thread_key,
            "assignment_generation": assignment_generation,
            "execution_id": execute["execution_id"],
            "status": execute.get("status"),
        }


def _ensure_column(conn: sqlite3.Connection, table: str, column: str, definition: str) -> None:
    columns = {
        row["name"]
        for row in conn.execute(f"PRAGMA table_info({table})").fetchall()
    }
    if column not in columns:
        conn.execute(f"ALTER TABLE {table} ADD COLUMN {column} {definition}")


def _row_to_feedback_item(row: sqlite3.Row) -> FeedbackItem:
    return FeedbackItem(
        id=row["id"],
        slack_channel=row["slack_channel"],
        slack_thread_ts=row["slack_thread_ts"],
        permalink=row["permalink"],
        amp_thread_id=row["amp_thread_id"],
        category=row["category"],
        severity=row["severity"],
        summary=row["summary"],
        cli_involved=row["cli_involved"],
        evidence=json.loads(row["evidence"]),
        reporter_user=row["reporter_user"],
        status=row["status"],
        created_at=row["created_at"],
        updated_at=row["updated_at"],
    )


def _severity_filter_clause(min_severity: str | None) -> tuple[str, list[str]]:
    severity_order = {"low": 0, "medium": 1, "high": 2}
    if not min_severity or min_severity not in severity_order:
        return "", []

    min_val = severity_order[min_severity]
    valid = [s for s, v in severity_order.items() if v >= min_val]
    return f" AND severity IN ({','.join('?' * len(valid))})", valid


def _load_centaur_api_key() -> str | None:
    return os.environ.get("CENTAUR_AGENT_API_KEY")


def _bot_message_looks_like_error(text: str) -> bool:
    return any(pattern.search(text) for pattern in BOT_ERROR_PATTERNS)


def init_db() -> sqlite3.Connection:
    """Initialize the feedback database."""
    FEEDBACK_DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(FEEDBACK_DB_PATH)
    conn.row_factory = sqlite3.Row

    conn.executescript("""
        CREATE TABLE IF NOT EXISTS ingestion_state (
            channel_id TEXT PRIMARY KEY,
            last_processed_ts TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS feedback_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slack_channel TEXT NOT NULL,
            slack_thread_ts TEXT NOT NULL,
            permalink TEXT NOT NULL,
            amp_thread_id TEXT,
            category TEXT NOT NULL,
            severity TEXT NOT NULL,
            summary TEXT NOT NULL,
            cli_involved TEXT,
            evidence TEXT NOT NULL,
            reporter_user TEXT,
            status TEXT NOT NULL DEFAULT 'new',
            dedupe_key TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(slack_channel, slack_thread_ts)
        );

        CREATE INDEX IF NOT EXISTS idx_feedback_status ON feedback_items(status);
        CREATE INDEX IF NOT EXISTS idx_feedback_category ON feedback_items(category);
        CREATE INDEX IF NOT EXISTS idx_feedback_created ON feedback_items(created_at);
    """)
    _ensure_column(conn, "feedback_items", "agent_thread_key", "TEXT")
    _ensure_column(conn, "feedback_items", "agent_execution_id", "TEXT")
    _ensure_column(conn, "feedback_items", "dispatch_count", "INTEGER NOT NULL DEFAULT 0")
    _ensure_column(conn, "feedback_items", "last_dispatched_at", "TEXT")
    _ensure_column(conn, "feedback_items", "last_dispatch_error", "TEXT")
    conn.commit()
    return conn


def get_last_processed_ts(conn: sqlite3.Connection, channel_id: str) -> str | None:
    """Get the last processed timestamp for a channel."""
    row = conn.execute(
        "SELECT last_processed_ts FROM ingestion_state WHERE channel_id = ?",
        (channel_id,),
    ).fetchone()
    return row["last_processed_ts"] if row else None


def update_last_processed_ts(conn: sqlite3.Connection, channel_id: str, ts: str) -> None:
    """Update the last processed timestamp for a channel."""
    now = datetime.now(timezone.utc).isoformat()
    conn.execute(
        """INSERT INTO ingestion_state (channel_id, last_processed_ts, updated_at)
           VALUES (?, ?, ?)
           ON CONFLICT(channel_id) DO UPDATE SET last_processed_ts = ?, updated_at = ?""",
        (channel_id, ts, now, ts, now),
    )
    conn.commit()


def extract_amp_thread_id(messages: list[dict]) -> str | None:
    """Extract Amp thread ID from bot messages."""
    for msg in reversed(messages):
        if msg.get("is_bot"):
            match = AMP_THREAD_PATTERN.search(msg.get("text", ""))
            if match:
                return match.group(0)
    return None


def extract_cli_mentions(text: str) -> list[str]:
    """Extract CLI names mentioned in text."""
    known_clis = [
        "paradigmdb",
        "figma",
        "anchorage",
        "coinbase",
        "falconx",
        "unit410",
        "bitgo",
        "slack",
        "gsuite",
        "defillama",
        "allium",
        "coingecko",
        "dune",
        "idxs",
        "posthog",
        "artemis",
        "standard-metrics",
        "sigma",
        "affinity",
    ]
    found = []
    text_lower = text.lower()
    for cli in known_clis:
        if cli in text_lower:
            found.append(cli)
    return found


def analyze_thread_signals(messages: list[dict], bot_user_id: str | None = None) -> FeedbackSignals:
    """Analyze a thread for feedback signals."""
    signals = FeedbackSignals()

    for msg in messages:
        # Check reactions
        reactions = msg.get("reactions", [])
        for r in reactions:
            name = r.get("name", "").lower()
            if name in NEGATIVE_REACTIONS:
                signals.has_negative_reaction = True
            if name in POSITIVE_REACTIONS:
                signals.has_positive_reaction = True

        # Check if bot message
        is_bot = msg.get("bot_id") or (bot_user_id and msg.get("user") == bot_user_id)
        if is_bot:
            signals.bot_message_count += 1
            msg["is_bot"] = True
            # Check for error patterns in bot output
            text = msg.get("text", "").lower()
            if _bot_message_looks_like_error(text):
                signals.has_bot_error = True
        else:
            signals.user_message_count += 1
            msg["is_bot"] = False

            # Check keywords in user messages
            text = msg.get("text", "").lower()
            for kw in NEGATIVE_KEYWORDS:
                if kw in text and kw not in signals.negative_keywords_found:
                    signals.negative_keywords_found.append(kw)
            for kw in POSITIVE_KEYWORDS:
                if kw in text and kw not in signals.positive_keywords_found:
                    signals.positive_keywords_found.append(kw)

    # Repeated requests = user sent >3 messages (had to keep trying)
    if signals.user_message_count > 3 and signals.bot_message_count > 0:
        signals.repeated_requests = True

    return signals


def should_process_thread(signals: FeedbackSignals) -> bool:
    """Determine if a thread should be processed for feedback."""
    # Skip threads without bot interaction
    if signals.bot_message_count == 0:
        return False

    # Process if any negative signal
    if signals.has_negative_reaction:
        return True
    if signals.negative_keywords_found:
        return True
    if signals.has_bot_error:
        return True
    if signals.repeated_requests:
        return True

    # Process successful interactions too (for positive examples)
    if signals.has_positive_reaction or signals.positive_keywords_found:
        return True

    return False


def classify_feedback(signals: FeedbackSignals, messages: list[dict]) -> tuple[str, str]:
    """Classify feedback category and severity."""
    # Determine category
    if signals.has_bot_error:
        category = "cli_bug"
    elif (
        signals.has_positive_reaction or signals.positive_keywords_found
    ) and not signals.has_negative_reaction and not signals.negative_keywords_found:
        category = "success"
    elif signals.repeated_requests:
        category = "routing_error"
    elif signals.negative_keywords_found:
        # Check if it's about missing capability vs wrong behavior
        text = " ".join(m.get("text", "") for m in messages).lower()
        if "should have" in text or "why didn't" in text or "can't" in text:
            category = "missing_capability"
        else:
            category = "routing_error"
    else:
        category = "unclear"

    # Determine severity
    if signals.has_bot_error:
        severity = "high"
    elif signals.has_negative_reaction and signals.repeated_requests:
        severity = "high"
    elif signals.has_negative_reaction or len(signals.negative_keywords_found) >= 2:
        severity = "medium"
    else:
        severity = "low"

    return category, severity


def fetch_threads_since(
    client: WebClient,
    channel_id: str,
    since_ts: str | None = None,
    limit: int | None = 200,
    bot_user_id: str | None = None,
    latest_ts: str | None = None,
) -> list[dict]:
    """Fetch threads from a channel since a timestamp, with full replies."""
    threads = []
    cursor = None

    # Default to last 7 days if no checkpoint
    if not since_ts:
        since_ts = str((datetime.now(timezone.utc) - timedelta(days=7)).timestamp())

    while limit is None or len(threads) < limit:
        page_limit = 100 if limit is None else min(limit - len(threads), 100)
        history_kwargs: dict[str, Any] = {
            "channel": channel_id,
            "oldest": since_ts,
            "inclusive": False,
            "limit": page_limit,
            "cursor": cursor,
        }
        if latest_ts is not None:
            history_kwargs["latest"] = latest_ts
        try:
            response = _retry_on_ratelimit(
                client.conversations_history,
                **history_kwargs,
            )
        except SlackApiError as e:
            raise RuntimeError(f"Slack API error: {e.response['error']}")

        for msg in response.get("messages", []):
            ts = msg.get("ts", "")
            thread_ts = msg.get("thread_ts", ts)
            reply_count = msg.get("reply_count", 0)

            # Fetch full thread if it has replies
            thread_messages = [msg]
            if reply_count > 0:
                try:
                    # Paginate thread replies
                    thread_cursor = None
                    while True:
                        thread_response = _retry_on_ratelimit(
                            client.conversations_replies,
                            channel=channel_id,
                            ts=thread_ts,
                            limit=200,
                            cursor=thread_cursor,
                        )
                        # Skip first message (already have it)
                        replies = (
                            thread_response.get("messages", [])[1:]
                            if not thread_cursor
                            else thread_response.get("messages", [])
                        )
                        thread_messages.extend(replies)

                        thread_cursor = thread_response.get("response_metadata", {}).get(
                            "next_cursor"
                        )
                        if not thread_cursor:
                            break
                except SlackApiError:
                    pass

            # Analyze signals
            signals = analyze_thread_signals(thread_messages, bot_user_id)

            if should_process_thread(signals):
                threads.append(
                    {
                        "thread_ts": thread_ts,
                        "messages": thread_messages,
                        "signals": signals,
                        "reply_count": len(thread_messages) - 1,
                    }
                )

        cursor = response.get("response_metadata", {}).get("next_cursor")
        if not cursor:
            break

    return threads


def save_feedback_item(conn: sqlite3.Connection, item: FeedbackItem) -> SaveFeedbackResult:
    """Save or update a feedback item."""
    now = datetime.now(timezone.utc).isoformat()
    dedupe_key = f"{item.category}:{item.cli_involved or 'none'}:{item.summary[:50]}"
    existing_row = conn.execute(
        "SELECT id FROM feedback_items WHERE slack_channel = ? AND slack_thread_ts = ?",
        (item.slack_channel, item.slack_thread_ts),
    ).fetchone()

    conn.execute(
        """INSERT INTO feedback_items
           (slack_channel, slack_thread_ts, permalink, amp_thread_id, category, severity,
            summary, cli_involved, evidence, reporter_user, status, dedupe_key, created_at, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
           ON CONFLICT(slack_channel, slack_thread_ts) DO UPDATE SET
             category = ?, severity = ?, summary = ?, cli_involved = ?, evidence = ?,
             status = CASE WHEN status = 'new' THEN 'new' ELSE status END,
             updated_at = ?""",
        (
            item.slack_channel,
            item.slack_thread_ts,
            item.permalink,
            item.amp_thread_id,
            item.category,
            item.severity,
            item.summary,
            item.cli_involved,
            json.dumps(item.evidence),
            item.reporter_user,
            item.status,
            dedupe_key,
            item.created_at or now,
            now,
            # For update
            item.category,
            item.severity,
            item.summary,
            item.cli_involved,
            json.dumps(item.evidence),
            now,
        ),
    )
    conn.commit()
    row = conn.execute(
        "SELECT id FROM feedback_items WHERE slack_channel = ? AND slack_thread_ts = ?",
        (item.slack_channel, item.slack_thread_ts),
    ).fetchone()
    return SaveFeedbackResult(item_id=row["id"], inserted=existing_row is None)


def collect_feedback(
    channels: list[str] | None = None,
    limit_per_channel: int | None = 200,
    since_days: int | None = None,
    latest_ts: str | None = None,
    update_checkpoint: bool = True,
) -> dict[str, Any]:
    """Collect feedback from specified channels.

    Args:
        channels: Channel names to scan. Defaults to ["test-bot"].
        limit_per_channel: Max threads to process per channel. None means no limit.
        since_days: Override checkpoint, scan last N days.
        latest_ts: Optional exclusive upper bound timestamp for history fetch.
        update_checkpoint: Whether to move the ingestion checkpoint forward.

    Returns:
        Stats about collection run.
    """
    channels = channels or ["test-bot"]
    client = get_slack_client()
    user_cache = get_user_cache(client)
    conn = init_db()

    # Get bot user ID
    try:
        auth_response = client.auth_test()
        bot_user_id = auth_response.get("user_id")
    except SlackApiError:
        bot_user_id = None

    # Resolve channel IDs
    all_channels = list_bot_channels()
    channel_map = {ch["name"]: ch["id"] for ch in all_channels}

    stats = {
        "channels_scanned": 0,
        "threads_analyzed": 0,
        "feedback_items_created": 0,
        "feedback_items_updated": 0,
        "by_category": {},
        "by_severity": {},
    }

    for channel_name in channels:
        channel_id = channel_map.get(channel_name.lstrip("#"))
        if not channel_id:
            continue

        # Get checkpoint or use since_days
        if since_days is not None:
            since_ts = str((datetime.now(timezone.utc) - timedelta(days=since_days)).timestamp())
        else:
            since_ts = get_last_processed_ts(conn, channel_id)

        # Fetch and analyze threads
        threads = fetch_threads_since(
            client,
            channel_id,
            since_ts,
            limit_per_channel,
            bot_user_id,
            latest_ts=latest_ts,
        )

        max_ts = since_ts or "0"
        for thread in threads:
            signals: FeedbackSignals = thread["signals"]
            messages = thread["messages"]
            thread_ts = thread["thread_ts"]

            # Track max timestamp
            for msg in messages:
                if msg.get("ts", "0") > max_ts:
                    max_ts = msg["ts"]

            # Classify
            category, severity = classify_feedback(signals, messages)

            # Extract metadata
            amp_thread_id = extract_amp_thread_id(messages)
            all_text = " ".join(m.get("text", "") for m in messages)
            clis = extract_cli_mentions(all_text)

            # Get reporter (first non-bot user)
            reporter = None
            for msg in messages:
                if not msg.get("is_bot"):
                    user_id = msg.get("user", "")
                    reporter = user_cache.get(user_id, user_id)
                    break

            # Build summary from first user message
            summary = ""
            for msg in messages:
                if not msg.get("is_bot"):
                    summary = resolve_mentions(msg.get("text", "")[:200], client, user_cache)
                    break

            # Build permalink
            permalink = f"https://slack.com/archives/{channel_id}/p{thread_ts.replace('.', '')}"

            # Create feedback item
            item = FeedbackItem(
                id=None,
                slack_channel=channel_name,
                slack_thread_ts=thread_ts,
                permalink=permalink,
                amp_thread_id=amp_thread_id,
                category=category,
                severity=severity,
                summary=summary,
                cli_involved=",".join(clis) if clis else None,
                evidence={
                    "negative_reactions": signals.has_negative_reaction,
                    "positive_reactions": signals.has_positive_reaction,
                    "negative_keywords": signals.negative_keywords_found,
                    "positive_keywords": signals.positive_keywords_found,
                    "user_messages": signals.user_message_count,
                    "bot_messages": signals.bot_message_count,
                    "bot_error": signals.has_bot_error,
                    "repeated_requests": signals.repeated_requests,
                },
                reporter_user=reporter,
                status="new",
                created_at=datetime.now(timezone.utc).isoformat(),
                updated_at=datetime.now(timezone.utc).isoformat(),
            )

            save_result = save_feedback_item(conn, item)
            stats["threads_analyzed"] += 1
            if save_result.inserted:
                stats["feedback_items_created"] += 1
            else:
                stats["feedback_items_updated"] += 1
            stats["by_category"][category] = stats["by_category"].get(category, 0) + 1
            stats["by_severity"][severity] = stats["by_severity"].get(severity, 0) + 1

        # Update checkpoint
        if update_checkpoint and max_ts and max_ts != "0":
            update_last_processed_ts(conn, channel_id, max_ts)

        stats["channels_scanned"] += 1

    conn.close()
    return stats


def get_feedback_digest(
    since_days: int = 7,
    status: str | None = None,
    category: str | None = None,
    min_severity: str | None = None,
) -> list[FeedbackItem]:
    """Get feedback items for digest.

    Args:
        since_days: Look back N days.
        status: Filter by status (new, triaged, etc.).
        category: Filter by category.
        min_severity: Minimum severity (low, medium, high).

    Returns:
        List of feedback items.
    """
    conn = init_db()
    since_date = (datetime.now(timezone.utc) - timedelta(days=since_days)).isoformat()

    query = "SELECT * FROM feedback_items WHERE created_at >= ?"
    params: list[Any] = [since_date]

    if status:
        query += " AND status = ?"
        params.append(status)

    if category:
        query += " AND category = ?"
        params.append(category)

    severity_clause, severity_params = _severity_filter_clause(min_severity)
    query += severity_clause
    params.extend(severity_params)

    query += " ORDER BY CASE severity WHEN 'high' THEN 0 WHEN 'medium' THEN 1 ELSE 2 END, created_at DESC"

    rows = conn.execute(query, params).fetchall()
    conn.close()
    return [_row_to_feedback_item(row) for row in rows]


def get_actionable_feedback_items(
    since_days: int = 7,
    min_severity: str = "medium",
    statuses: tuple[str, ...] = ("new", "triaged"),
    limit: int | None = None,
) -> list[FeedbackItem]:
    """Return feedback items worth dispatching to the improvement agent."""
    conn = init_db()
    since_date = (datetime.now(timezone.utc) - timedelta(days=since_days)).isoformat()
    placeholders = ",".join("?" * len(statuses))
    query = (
        "SELECT * FROM feedback_items WHERE created_at >= ? "
        "AND category != 'success' "
        f"AND status IN ({placeholders})"
    )
    params: list[Any] = [since_date, *statuses]
    severity_clause, severity_params = _severity_filter_clause(min_severity)
    query += severity_clause
    params.extend(severity_params)
    query += " ORDER BY CASE severity WHEN 'high' THEN 0 WHEN 'medium' THEN 1 ELSE 2 END, created_at DESC"
    if limit is not None:
        query += " LIMIT ?"
        params.append(limit)

    rows = conn.execute(query, params).fetchall()
    conn.close()
    return [_row_to_feedback_item(row) for row in rows]


def build_improvement_prompt(items: list[FeedbackItem], channels: list[str]) -> str:
    """Build the prompt for a background engineering improvement run."""
    feedback_payload = []
    for item in items:
        feedback_payload.append(
            {
                "id": item.id,
                "channel": item.slack_channel,
                "permalink": item.permalink,
                "summary": item.summary,
                "category": item.category,
                "severity": item.severity,
                "cli_involved": item.cli_involved,
                "amp_thread_id": item.amp_thread_id,
                "evidence": item.evidence,
            }
        )

    prompt = [
        "You are working on paradigmxyz/centaur.",
        "Investigate and fix the highest-leverage issues surfaced by Slack feedback.",
        "Use git-branch paradigmxyz/centaur before editing because the host mount is read-only.",
        "Read the linked Slack permalinks and Amp threads when they are relevant, then make code changes in the repo.",
        "Prefer the smallest fixes that materially improve agent behavior.",
        "When done, open a PR with a concise summary of the fixes.",
        "",
        f"Channels scanned: {', '.join(channels)}",
        f"Feedback item count in this batch: {len(items)}",
        "",
        "Structured feedback:",
        json.dumps(feedback_payload, indent=2),
    ]
    return "\n".join(prompt)


def mark_feedback_items_dispatched(
    item_ids: list[int],
    agent_thread_key: str,
    agent_execution_id: str,
    *,
    dispatch_error: str | None = None,
) -> None:
    """Mark feedback items as dispatched to the background improvement agent."""
    if not item_ids:
        return

    conn = init_db()
    now = datetime.now(timezone.utc).isoformat()
    placeholders = ",".join("?" * len(item_ids))
    if dispatch_error:
        conn.execute(
            f"UPDATE feedback_items SET last_dispatch_error = ?, updated_at = ? WHERE id IN ({placeholders})",
            [dispatch_error, now, *item_ids],
        )
    else:
        conn.execute(
            f"""
            UPDATE feedback_items
            SET status = 'in_progress',
                agent_thread_key = ?,
                agent_execution_id = ?,
                dispatch_count = COALESCE(dispatch_count, 0) + 1,
                last_dispatched_at = ?,
                last_dispatch_error = NULL,
                updated_at = ?
            WHERE id IN ({placeholders})
            """,
            [agent_thread_key, agent_execution_id, now, now, *item_ids],
        )
    conn.commit()
    conn.close()


def run_improvement_cycle(
    *,
    channels: list[str],
    since_days: int,
    limit_per_channel: int | None,
    max_items: int,
    min_severity: str = "medium",
    harness: str = "amp",
    persona_id: str = "eng",
    dry_run: bool = False,
    agent_client: CentaurAgentClient | None = None,
) -> dict[str, Any]:
    """Run one full improvement cycle: collect, select, dispatch, and mark items."""
    collect_stats = collect_feedback(
        channels=channels,
        limit_per_channel=limit_per_channel,
        since_days=since_days,
    )
    items = get_actionable_feedback_items(
        since_days=since_days,
        min_severity=min_severity,
        limit=max_items,
    )
    prompt = build_improvement_prompt(items, channels)
    result: dict[str, Any] = {
        "collect_stats": collect_stats,
        "actionable_items": len(items),
        "item_ids": [item.id for item in items if item.id is not None],
        "prompt": prompt,
        "dispatched": False,
    }
    if not items:
        return result

    if dry_run:
        return result

    agent_client = agent_client or CentaurAgentClient()
    try:
        run = agent_client.start_improvement_run(
            prompt,
            harness=harness,
            persona_id=persona_id,
        )
    except Exception as exc:
        mark_feedback_items_dispatched(result["item_ids"], "", "", dispatch_error=str(exc))
        raise
    result.update(run)
    result["dispatched"] = True
    mark_feedback_items_dispatched(result["item_ids"], run["thread_key"], run["execution_id"])
    return result


def backfill_feedback(
    *,
    channels: list[str],
    since_days: int,
    limit_per_channel: int | None,
) -> dict[str, Any]:
    """Run a historical backfill without the usual per-channel cap."""
    return collect_feedback(
        channels=channels,
        limit_per_channel=limit_per_channel,
        since_days=since_days,
    )


def format_digest_markdown(items: list[FeedbackItem]) -> str:
    """Format feedback items as markdown digest."""
    if not items:
        return "No feedback items found for the specified criteria."

    # Group by category
    by_category: dict[str, list[FeedbackItem]] = {}
    for item in items:
        by_category.setdefault(item.category, []).append(item)

    lines = ["# Feedback Digest\n"]

    # Summary
    lines.append("## Summary\n")
    lines.append(f"- **Total items**: {len(items)}")
    for cat, cat_items in sorted(by_category.items()):
        lines.append(f"- **{cat}**: {len(cat_items)}")
    lines.append("")

    # By category
    category_order = ["cli_bug", "routing_error", "missing_capability", "unclear", "success"]
    for cat in category_order:
        cat_items = by_category.get(cat, [])
        if not cat_items:
            continue

        emoji = {
            "cli_bug": "🐛",
            "routing_error": "🔀",
            "missing_capability": "➕",
            "success": "✅",
            "unclear": "❓",
        }.get(cat, "📝")
        lines.append(f"## {emoji} {cat.replace('_', ' ').title()} ({len(cat_items)})\n")

        for item in cat_items:
            sev_emoji = {"high": "🔴", "medium": "🟡", "low": "🟢"}.get(item.severity, "⚪")
            lines.append(f"### {sev_emoji} [{item.summary[:60]}...]({item.permalink})")
            lines.append(f"- **Reporter**: {item.reporter_user or 'unknown'}")
            if item.cli_involved:
                lines.append(f"- **CLI**: `{item.cli_involved}`")
            if item.amp_thread_id:
                lines.append(
                    f"- **Amp Thread**: [{item.amp_thread_id}](https://ampcode.com/threads/{item.amp_thread_id})"
                )
            lines.append(f"- **Status**: {item.status}")

            # Evidence summary
            ev = item.evidence
            signals = []
            if ev.get("bot_error"):
                signals.append("bot error")
            if ev.get("negative_reactions"):
                signals.append("👎 reaction")
            if ev.get("repeated_requests"):
                signals.append("repeated requests")
            if ev.get("negative_keywords"):
                signals.append(f"keywords: {', '.join(ev['negative_keywords'][:3])}")
            if signals:
                lines.append(f"- **Signals**: {', '.join(signals)}")
            lines.append("")

    return "\n".join(lines)


def update_feedback_status(item_id: int, status: str) -> bool:
    """Update the status of a feedback item."""
    conn = init_db()
    now = datetime.now(timezone.utc).isoformat()
    cursor = conn.execute(
        "UPDATE feedback_items SET status = ?, updated_at = ? WHERE id = ?",
        (status, now, item_id),
    )
    conn.commit()
    affected = cursor.rowcount
    conn.close()
    return affected > 0
