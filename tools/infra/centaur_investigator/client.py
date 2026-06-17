"""Privacy-safe Centaur thread investigation helper."""

from __future__ import annotations

import importlib.util
import re
from datetime import UTC, datetime
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, unquote, urlparse

DEFAULT_LIMIT = 25
MAX_LIMIT = 200
DEFAULT_WINDOW_HOURS = 24
MAX_WINDOW_HOURS = 24 * 30
MAX_LOG_LIMIT = 500

_SLACK_URL_RE = re.compile(r"https?://[^\s<>|]+/archives/[A-Z0-9]+/p\d{10,20}[^\s<>|]*")
_SLACK_THREAD_KEY_RE = re.compile(
    r"\b(?P<thread_key>[A-Za-z][A-Za-z0-9_.-]*:"
    r"(?:(?P<team>T[A-Z0-9]+):)?(?P<channel>[CDG][A-Z0-9]+):"
    r"(?P<thread_ts>\d{10}\.\d{1,6}))\b"
)
_CHANNEL_TS_RE = re.compile(
    r"\b(?P<channel>[CDG][A-Z0-9]+):(?P<thread_ts>\d{10}\.\d{1,6})\b"
)
_KEY_SOURCE_RE = re.compile(r"^[A-Za-z][A-Za-z0-9_.-]*:")


def _clamp(value: int, *, minimum: int, maximum: int) -> int:
    return max(minimum, min(maximum, int(value)))


def _isoformat(value: Any) -> str | None:
    if isinstance(value, datetime):
        return value.isoformat()
    return None


def _normalize_ts(value: str | None) -> str | None:
    if not value:
        return None
    text = unquote(str(value)).strip()
    if not text:
        return None
    if "." in text:
        left, right = text.split(".", 1)
        if left.isdigit() and right.isdigit():
            return f"{left}.{right[:6].ljust(6, '0')}"
        return None
    digits = re.sub(r"\D", "", text)
    if len(digits) <= 10:
        return None
    return f"{digits[:10]}.{digits[10:16].ljust(6, '0')}"


def _slack_ts_to_datetime(ts: str | None) -> datetime | None:
    if not ts:
        return None
    try:
        return datetime.fromtimestamp(float(ts), tz=UTC)
    except (TypeError, ValueError, OSError):
        return None


def _dedupe(values: list[str]) -> list[str]:
    seen: set[str] = set()
    result: list[str] = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        result.append(value)
    return result


def _log_field_expr(field: str, value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'{field}:"{escaped}"'


def _thread_key_candidates(
    *,
    channel_id: str,
    thread_ts: str,
    team_id: str | None = None,
    source: str = "slack",
) -> list[str]:
    candidates = []
    if team_id:
        candidates.extend(
            [
                f"{source}:{team_id}:{channel_id}:{thread_ts}",
                f"slack:{team_id}:{channel_id}:{thread_ts}",
                f"chat:{team_id}:{channel_id}:{thread_ts}",
            ]
        )
    candidates.extend(
        [
            f"{source}:{channel_id}:{thread_ts}",
            f"slack:{channel_id}:{thread_ts}",
            f"chat:{channel_id}:{thread_ts}",
        ]
    )
    return _dedupe(candidates)


def _first_qs(query: dict[str, list[str]], *names: str) -> str | None:
    for name in names:
        values = query.get(name)
        if values:
            return values[0]
    return None


def _clean_reference_text(reference: str) -> str:
    text = reference.strip()
    if text.startswith("<") and ">" in text:
        text = text[1 : text.index(">")]
    if "|" in text and text.startswith("http"):
        text = text.split("|", 1)[0]
    return text.strip()


def parse_slack_reference(reference: str) -> dict[str, Any]:
    """Parse a Slack permalink or Centaur thread key into identifiers only."""
    text = _clean_reference_text(reference)
    direct = _SLACK_THREAD_KEY_RE.search(text)
    if direct:
        thread_key = direct.group("thread_key")
        channel_id = direct.group("channel")
        team_id = direct.group("team")
        thread_ts = _normalize_ts(direct.group("thread_ts"))
        if not thread_ts:
            return {"status": "error", "error": "invalid thread timestamp"}
        source = thread_key.split(":", 1)[0]
        return {
            "status": "ok",
            "input": reference,
            "kind": "thread_key",
            "source": source,
            "team_id": team_id,
            "channel_id": channel_id,
            "message_ts": thread_ts,
            "thread_ts": thread_ts,
            "thread_datetime": _isoformat(_slack_ts_to_datetime(thread_ts)),
            "thread_key": thread_key,
            "thread_key_candidates": _thread_key_candidates(
                channel_id=channel_id,
                thread_ts=thread_ts,
                team_id=team_id,
                source=source,
            ),
        }

    channel_ts = _CHANNEL_TS_RE.search(text)
    if channel_ts:
        channel_id = channel_ts.group("channel")
        thread_ts = _normalize_ts(channel_ts.group("thread_ts"))
        if thread_ts:
            return {
                "status": "ok",
                "input": reference,
                "kind": "channel_ts",
                "source": "slack",
                "team_id": None,
                "channel_id": channel_id,
                "message_ts": thread_ts,
                "thread_ts": thread_ts,
                "thread_datetime": _isoformat(_slack_ts_to_datetime(thread_ts)),
                "thread_key": f"slack:{channel_id}:{thread_ts}",
                "thread_key_candidates": _thread_key_candidates(
                    channel_id=channel_id,
                    thread_ts=thread_ts,
                ),
            }

    url_match = _SLACK_URL_RE.search(text)
    if not url_match and text.startswith(("http://", "https://", "slack://")):
        url = text
    elif url_match:
        url = url_match.group(0)
    else:
        return {"status": "error", "error": "no Slack permalink or thread_key found"}

    parsed = urlparse(url)
    query = parse_qs(parsed.query)
    team_id = _first_qs(query, "team", "team_id")
    channel_id = _first_qs(query, "cid", "channel", "channel_id", "id")
    message_ts = _normalize_ts(_first_qs(query, "message", "ts"))

    path_match = re.search(r"/archives/(?P<channel>[A-Z0-9]+)/p(?P<ts>\d+)", parsed.path)
    if path_match:
        channel_id = channel_id or path_match.group("channel")
        message_ts = message_ts or _normalize_ts(path_match.group("ts"))

    thread_ts = _normalize_ts(_first_qs(query, "thread_ts")) or message_ts
    if parsed.scheme == "slack":
        channel_id = channel_id or _first_qs(query, "id")
        thread_ts = _normalize_ts(_first_qs(query, "thread_ts", "message", "ts")) or thread_ts
        message_ts = message_ts or _normalize_ts(_first_qs(query, "message", "ts"))

    if not channel_id or not thread_ts:
        return {"status": "error", "error": "could not parse Slack channel and thread timestamp"}

    message_ts = message_ts or thread_ts
    return {
        "status": "ok",
        "input": reference,
        "kind": "slack_permalink",
        "source": "slack",
        "team_id": team_id,
        "channel_id": channel_id,
        "message_ts": message_ts,
        "thread_ts": thread_ts,
        "thread_datetime": _isoformat(_slack_ts_to_datetime(thread_ts)),
        "message_datetime": _isoformat(_slack_ts_to_datetime(message_ts)),
        "thread_key": f"slack:{channel_id}:{thread_ts}",
        "thread_key_candidates": _thread_key_candidates(
            channel_id=channel_id,
            thread_ts=thread_ts,
            team_id=team_id,
        ),
        "permalink": f"https://slack.com/archives/{channel_id}/p{message_ts.replace('.', '')}",
    }


def _safe_load_module(module_name: str, path: Path) -> Any | None:
    if not path.exists():
        return None
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        return None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CentaurInvestigatorClient:
    """Investigate a Centaur thread without exposing message context."""

    def parse_thread_reference(self, reference: str) -> dict[str, Any]:
        """Parse a Slack thread permalink or Centaur thread key."""
        return parse_slack_reference(reference)

    def session_state(
        self,
        thread_key: str,
        limit: int = DEFAULT_LIMIT,
        include_observability: bool = True,
        window_hours: int = DEFAULT_WINDOW_HOURS,
        logs_limit: int = 100,
    ) -> dict[str, Any]:
        """Inspect a thread key using identifiers and observability only."""
        if not thread_key.strip() or not _KEY_SOURCE_RE.match(thread_key):
            return {"status": "error", "error": "thread_key must be namespaced"}

        result = {
            "status": "ok",
            "parsed": {
                "kind": "thread_key",
                "thread_key": thread_key.strip(),
                "thread_key_candidates": [thread_key.strip()],
            },
            "thread_keys": [thread_key.strip()],
            "analysis": self._summarize(
                parsed={"thread_key": thread_key.strip()},
                observability_enabled=include_observability,
            ),
            "postgres": {
                "status": "role_only",
                "note": (
                    "centaur_readonly is managed by migrations. This tool does not "
                    "query Postgres or expose stored conversation context."
                ),
            },
        }
        if include_observability:
            result["observability"] = self._observability(
                thread_keys=[thread_key.strip()],
                execution_ids=[],
                window_hours=_clamp(window_hours, minimum=1, maximum=MAX_WINDOW_HOURS),
                logs_limit=_clamp(logs_limit, minimum=1, maximum=MAX_LOG_LIMIT),
            )
        return result

    def investigate_slack_thread(
        self,
        reference: str,
        limit: int = DEFAULT_LIMIT,
        include_observability: bool = True,
        window_hours: int = DEFAULT_WINDOW_HOURS,
        logs_limit: int = 100,
    ) -> dict[str, Any]:
        """Investigate a Slack thread link without exposing message context."""
        parsed = parse_slack_reference(reference)
        if parsed.get("status") != "ok":
            return parsed

        thread_keys = parsed.get("thread_key_candidates") or [parsed.get("thread_key")]
        thread_keys = [str(value) for value in thread_keys if value]
        result = {
            "status": "ok",
            "parsed": parsed,
            "thread_keys": thread_keys,
            "analysis": self._summarize(
                parsed=parsed,
                observability_enabled=include_observability,
            ),
            "postgres": {
                "status": "role_only",
                "note": (
                    "centaur_readonly is managed by migrations. This tool does not "
                    "query Postgres or expose stored conversation context."
                ),
            },
        }
        if include_observability:
            result["observability"] = self._observability(
                thread_keys=thread_keys,
                execution_ids=[],
                window_hours=_clamp(window_hours, minimum=1, maximum=MAX_WINDOW_HOURS),
                logs_limit=_clamp(logs_limit, minimum=1, maximum=MAX_LOG_LIMIT),
            )
        return result

    def investigate(
        self,
        query: str,
        limit: int = DEFAULT_LIMIT,
        include_observability: bool = True,
        window_hours: int = DEFAULT_WINDOW_HOURS,
        logs_limit: int = 100,
    ) -> dict[str, Any]:
        """Investigate natural-language text containing a Slack link or thread_key."""
        parsed = parse_slack_reference(query)
        if parsed.get("status") == "ok":
            return self.investigate_slack_thread(
                query,
                limit=limit,
                include_observability=include_observability,
                window_hours=window_hours,
                logs_limit=logs_limit,
            )
        direct_key = re.search(r"\b[A-Za-z][A-Za-z0-9_.-]*:[^\s<>|]+\b", query)
        if direct_key:
            return self.session_state(
                direct_key.group(0),
                limit=limit,
                include_observability=include_observability,
                window_hours=window_hours,
                logs_limit=logs_limit,
            )
        return {
            "status": "error",
            "error": "query must contain a Slack permalink or Centaur thread_key",
        }

    @staticmethod
    def _summarize(
        *,
        parsed: dict[str, Any],
        observability_enabled: bool,
    ) -> dict[str, Any]:
        findings = [
            "Parsed thread identifiers without querying message or event context.",
            "Postgres access is currently role-only; no Centaur data views are exposed.",
        ]
        if observability_enabled:
            findings.append("Best-effort vlogs/vmetrics enrichment is enabled.")
        else:
            findings.append("Observability enrichment is disabled for this call.")

        warnings = []
        if parsed.get("channel_id") and parsed.get("thread_ts"):
            warnings.append(
                "This result only contains identifiers and observability metadata; "
                "it intentionally omits Slack message text and stored transcript context."
            )

        return {
            "summary": " ".join(findings),
            "findings": findings,
            "warnings": warnings,
            "primary_source": "identifiers_and_observability",
        }

    def _observability(
        self,
        *,
        thread_keys: list[str],
        execution_ids: list[str],
        window_hours: int,
        logs_limit: int,
    ) -> dict[str, Any]:
        result: dict[str, Any] = {
            "source": "best_effort_vlogs_vmetrics",
            "window_hours": window_hours,
            "privacy_note": (
                "Only aggregate observability metadata is returned. Raw log rows, "
                "Slack message text, and stored transcript context are never requested."
            ),
            "vlogs": {"status": "skipped"},
            "vmetrics": {"status": "skipped"},
        }

        infra_dir = Path(__file__).resolve().parent.parent
        vlogs_module = _safe_load_module(
            "_centaur_investigator_vlogs_client",
            infra_dir / "vlogs" / "client.py",
        )
        if vlogs_module is not None:
            try:
                vlogs = vlogs_module.VictoriaLogsClient()
                primary_thread = thread_keys[0] if thread_keys else ""
                thread_query = (
                    f"_time:{window_hours}h {_log_field_expr('thread_key', primary_thread)}"
                    if primary_thread
                    else ""
                )
                result["vlogs"] = {
                    "status": "ok",
                    "thread_key": primary_thread,
                    "log_hits": vlogs.hits(thread_query, step="1h") if thread_query else {},
                    "error_hits": (
                        vlogs.hits(f"{thread_query} AND level:error", step="1h")
                        if thread_query
                        else {}
                    ),
                    "event_names": (
                        vlogs.field_values("event", query=thread_query, limit=min(100, logs_limit))
                        if thread_query
                        else []
                    ),
                    "services": (
                        vlogs.field_values("service", query=thread_query, limit=min(50, logs_limit))
                        if thread_query
                        else []
                    ),
                    "tool_usage": (
                        vlogs.tool_usage_by_thread(
                            thread_key=primary_thread,
                            start=f"{window_hours}h",
                            limit=min(100, logs_limit),
                        )
                        if primary_thread
                        else []
                    ),
                    "execution_log_hits": {
                        execution_id: vlogs.hits(
                            f"_time:{window_hours}h {_log_field_expr('execution_id', execution_id)}",
                            step="1h",
                        )
                        for execution_id in execution_ids[:3]
                    },
                }
            except Exception as exc:
                result["vlogs"] = {"status": "error", "error": str(exc)}

        vmetrics_module = _safe_load_module(
            "_centaur_investigator_vmetrics_client",
            infra_dir / "vmetrics" / "client.py",
        )
        if vmetrics_module is not None:
            try:
                vmetrics = vmetrics_module.VictoriaMetricsClient()
                result["vmetrics"] = {
                    "status": "ok",
                    "ready": vmetrics.ready(),
                    "session_metric_names": vmetrics.metric_names(prefix="session_")[:50],
                    "centaur_metric_names": vmetrics.metric_names(prefix="centaur_")[:50],
                }
            except Exception as exc:
                result["vmetrics"] = {"status": "error", "error": str(exc)}

        return result


def _client() -> CentaurInvestigatorClient:
    return CentaurInvestigatorClient()
