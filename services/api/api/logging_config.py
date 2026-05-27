"""Shared structlog configuration for API and CLI."""

from __future__ import annotations

import os
import re
import sys
from typing import Any

import structlog

_LOG_LEVELS = {"critical": 50, "error": 40, "warning": 30, "info": 20, "debug": 10}
_EMAIL_RE = re.compile(r"\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b", re.IGNORECASE)
_SSN_RE = re.compile(r"\b\d{3}-\d{2}-\d{4}\b")
_PHONE_CANDIDATE_RE = re.compile(r"(?<!\w)(?:\+?\d[\d(). -]{8,}\d)(?!\w)")
_BEARER_TOKEN_RE = re.compile(r"(?i)\bbearer\s+[A-Z0-9._~+/=-]+")
_FIELD_SPLIT_RE = re.compile(r"(?<!^)(?=[A-Z])|[^A-Za-z0-9]+")
_SECRET_FIELD_TOKENS = {
    "password",
    "secret",
    "token",
}
_SECRET_FIELD_NAMES = {"apikey", "authorization", "clientsecret", "accesstoken", "refreshtoken"}
_EMAIL_FIELD_NAMES = {"email", "useremail", "authoremail"}
_PHONE_FIELD_NAMES = {"phone", "phonenumber", "userphone"}
_SSN_FIELD_NAMES = {"ssn", "socialsecuritynumber"}
# This can drift over time, but it is less disruptive than reading image refs
# through Helm chart changes while we need a quick production log marker.
_LOG_VERSION_UUID = "7f3b4a2e-9d7c-4f2a-8b91-3e6d2c0a5f14"


def _normalize_field_name(field_name: str | None) -> str:
    if not field_name:
        return ""
    return re.sub(r"[^a-z0-9]", "", field_name.casefold())


def _field_tokens(field_name: str | None) -> set[str]:
    if not field_name:
        return set()
    return {part.casefold() for part in _FIELD_SPLIT_RE.split(field_name) if part}


def _redact_phone_match(match: re.Match[str]) -> str:
    candidate = match.group(0)
    digits = sum(ch.isdigit() for ch in candidate)
    if 10 <= digits <= 15 and ":" not in candidate:
        return "[REDACTED:phone]"
    return candidate


def _sanitize_log_string(value: str) -> str:
    sanitized = _BEARER_TOKEN_RE.sub("Bearer [REDACTED:secret]", value)
    sanitized = _EMAIL_RE.sub("[REDACTED:email]", sanitized)
    sanitized = _SSN_RE.sub("[REDACTED:ssn]", sanitized)
    return _PHONE_CANDIDATE_RE.sub(_redact_phone_match, sanitized)


def _sanitize_log_value(value: Any, *, field_name: str | None = None) -> Any:
    normalized_field = _normalize_field_name(field_name)
    field_tokens = _field_tokens(field_name)
    if value is None or isinstance(value, (bool, int, float)):
        return value
    if isinstance(value, dict):
        return {k: _sanitize_log_value(v, field_name=str(k)) for k, v in value.items()}
    if isinstance(value, list):
        return [_sanitize_log_value(item, field_name=field_name) for item in value]
    if isinstance(value, tuple):
        return tuple(_sanitize_log_value(item, field_name=field_name) for item in value)
    if isinstance(value, str):
        if normalized_field in _SECRET_FIELD_NAMES or field_tokens & _SECRET_FIELD_TOKENS:
            return "[REDACTED:secret]"
        if normalized_field in _EMAIL_FIELD_NAMES or "email" in field_tokens:
            return "[REDACTED:email]"
        if normalized_field in _PHONE_FIELD_NAMES or "phone" in field_tokens:
            return "[REDACTED:phone]"
        if normalized_field in _SSN_FIELD_NAMES or "ssn" in field_tokens:
            return "[REDACTED:ssn]"
        return _sanitize_log_string(value)
    return value


def _add_default_service(logger, method_name, event_dict):
    """Ensure API logs always carry a service name for downstream queries."""
    event_dict.setdefault("service", os.getenv("CENTAUR_SERVICE_NAME", "api"))
    return event_dict


def _add_log_version(logger, method_name, event_dict):
    """Attach a manually rotated log version marker to every structured log line."""
    event_dict.setdefault("log_version_uuid", _LOG_VERSION_UUID)
    return event_dict


def _scrub_sensitive_fields(logger, method_name, event_dict):
    """Redact obvious PII and secrets before any renderer emits the log line."""
    return {k: _sanitize_log_value(v, field_name=str(k)) for k, v in event_dict.items()}


def _add_vlogs_msg(logger, method_name, event_dict):
    """Copy event to _msg for VictoriaLogs compatibility."""
    event_dict.setdefault("_msg", event_dict.get("msg") or event_dict.get("event", ""))
    return event_dict


def configure_structlog() -> int:
    """Configure structlog with JSON (prod) or console (dev) rendering.

    Returns the resolved log level integer.
    """
    log_level = _LOG_LEVELS.get(
        (os.getenv("CENTAUR_LOG_LEVEL") or os.getenv("LOG_LEVEL") or "info").lower(), 20
    )
    is_dev = sys.stderr.isatty()
    processors = [
        structlog.contextvars.merge_contextvars,
        _add_default_service,
        _add_log_version,
        _scrub_sensitive_fields,
        structlog.processors.add_log_level,
        structlog.processors.TimeStamper(fmt="iso", key="timestamp"),
    ]
    if is_dev:
        processors.append(structlog.dev.ConsoleRenderer())
    else:
        processors.append(_add_vlogs_msg)
        processors.append(structlog.processors.JSONRenderer())
    structlog.configure(
        logger_factory=structlog.PrintLoggerFactory(file=sys.stdout),
        wrapper_class=structlog.make_filtering_bound_logger(log_level),
        processors=processors,
    )
    return log_level
