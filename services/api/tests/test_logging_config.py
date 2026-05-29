"""Unit tests for API log scrubbing."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from api.logging_config import _add_log_version, _scrub_sensitive_fields


def test_scrub_sensitive_fields_redacts_nested_pii_and_secrets():
    payload = {
        "event": "tool_call_started",
        "email": "alice@example.com",
        "user_phone": "+1 (415) 555-1212",
        "details": {
            "token": "super-secret-token",
            "note": "Email bob@example.com or call 415-555-1212. SSN 123-45-6789.",
        },
        "authorization": "Bearer abc.def.ghi",
    }

    redacted = _scrub_sensitive_fields(None, "info", payload)

    assert redacted["event"] == "tool_call_started"
    assert redacted["email"] == "[REDACTED:email]"
    assert redacted["user_phone"] == "[REDACTED:phone]"
    assert redacted["details"]["token"] == "[REDACTED:secret]"
    assert redacted["authorization"] == "[REDACTED:secret]"
    assert redacted["details"]["note"] == (
        "Email [REDACTED:email] or call [REDACTED:phone]. SSN [REDACTED:ssn]."
    )


def test_scrub_sensitive_fields_keeps_non_sensitive_values_readable():
    payload = {
        "event": "message_buffered",
        "secretary_name": "Jordan Example",
        "thread_key": "slack:C123:1778864764.480189",
        "count": 2,
        "note": "Processed request on 2026-05-15 at 17:11 UTC",
    }

    redacted = _scrub_sensitive_fields(None, "info", payload)

    assert redacted == payload


def test_add_log_version_includes_static_uuid():
    enriched = _add_log_version(None, "info", {"event": "execute_completed"})

    assert enriched["event"] == "execute_completed"
    assert enriched["log_version_uuid"] == "013ca634-6a30-4047-8511-8e5483f313ea"
