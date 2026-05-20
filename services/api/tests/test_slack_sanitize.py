"""Tests for sanitize_for_slack."""

from __future__ import annotations

import pytest

from api.slack_sanitize import sanitize_for_slack


def test_empty_input_returns_empty_string():
    assert sanitize_for_slack(None) == ""
    assert sanitize_for_slack("") == ""


def test_passes_through_clean_prose_unchanged():
    text = "Spock — Paradigm's investment agent. What are we looking at?"
    assert sanitize_for_slack(text) == text


def test_strips_k8s_status_block():
    raw = (
        "The sandbox spawn failed: "
        '{"kind":"Status","apiVersion":"v1","status":"Failure",'
        '"message":"pods already exists","reason":"AlreadyExists","code":409}'
        " — retrying."
    )
    out = sanitize_for_slack(raw)
    assert "[k8s status omitted]" in out
    assert "AlreadyExists" not in out
    assert "Status" not in out


def test_strips_tool_error_envelope():
    raw = (
        "websearch failed: "
        '{"detail":{"upstream":{"message":"bad gateway"}},"error_type":"InternalServerError",'
        '"status_code":502}'
        " — falling back to internal cache."
    )
    out = sanitize_for_slack(raw)
    assert "[tool error omitted]" in out
    assert "InternalServerError" not in out
    assert "falling back" in out


@pytest.mark.parametrize(
    "label",
    ["Codex", "Agent", "Amp", "Claude Code", "Pi", "claude code", "CODEX"],
)
def test_strips_thread_trailer_by_engine_label(label: str):
    raw = f"All done. {label} thread `019e3c91-4030-7910-86c8-4c756f73bdc5`"
    out = sanitize_for_slack(raw)
    assert "thread" not in out.lower()
    assert "019e" not in out
    assert "All done." in out


def test_strips_interactive_elements_suffix():
    raw = "Reply body. Codex thread `019e3c91-4030-7910`, with interactive elements"
    out = sanitize_for_slack(raw)
    assert "interactive elements" not in out.lower()
    assert "Reply body." in out


def test_strips_execution_ids():
    raw = "No final text was captured. Execution: `exe_e77594af2e0b4893`."
    out = sanitize_for_slack(raw)
    assert "exe_" not in out
    assert "[execution id omitted]" in out


def test_strips_curl_exit_code_blob():
    raw = "Hit curl: (28) Operation timed out after 30000 milliseconds — retrying."
    out = sanitize_for_slack(raw)
    assert "Operation timed out" not in out
    assert "transport_error(28)" in out


def test_is_idempotent():
    raw = (
        'Done. {"kind":"Status","status":"Failure","reason":"x","code":409} '
        "Codex thread `abc12345-9999-7910-86c8-4c756f73bdc5`, with interactive elements"
    )
    once = sanitize_for_slack(raw)
    twice = sanitize_for_slack(once)
    assert once == twice


def test_does_not_strip_unrelated_json():
    raw = 'Here is the schema: {"name":"alice","age":30} — looks fine.'
    out = sanitize_for_slack(raw)
    assert '"name":"alice"' in out


def test_does_not_strip_normal_uuid_in_prose():
    raw = "The migration uses uuid 019e3c91-4030-7910 and execution exe_e77594af2e0b4893 for tracking."
    out = sanitize_for_slack(raw)
    assert "019e3c91-4030-7910" in out
    assert "exe_e77594af2e0b4893" in out


def test_collapses_excess_blank_lines_left_by_strippers():
    raw = (
        "Top of message.\n\n"
        '{"kind":"Status","status":"Failure","reason":"x"}\n\n\n\n'
        "Bottom of message."
    )
    out = sanitize_for_slack(raw)
    assert "\n\n\n" not in out
    assert "Top of message." in out
    assert "Bottom of message." in out
