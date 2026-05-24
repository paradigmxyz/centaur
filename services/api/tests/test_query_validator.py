"""Unit tests for ``/agent/query``'s table-allowlist + dangerous-function
validator.

These tests cover the pure validator only and do not require a running
Postgres — they exercise ``_validate_query_safety`` (and its helpers)
directly so they run in any environment that can import ``api.routers.agent``.

The full end-to-end ``/agent/query`` flow is exercised by the existing
integration suite (which boots Postgres). Adding a DB-backed test for every
regression case would double-charge the integration runtime; the
pure-function tests below give the same coverage for the validator's
classification logic.
"""

from __future__ import annotations

import pytest

# The router module pulls in the rest of the API, which is too expensive (and
# requires DB env vars) to import for a pure-function test. Import the
# validator and helpers via a focused module access pattern so the test
# stays cheap and standalone.
import importlib

_agent_router = importlib.import_module("api.routers.agent")

_validate_query_safety = _agent_router._validate_query_safety
_normalize_table_ref = _agent_router._normalize_table_ref
_strip_sql_noise = _agent_router._strip_sql_noise
HTTPException = _agent_router.HTTPException


# ── _normalize_table_ref ────────────────────────────────────────────────────

@pytest.mark.parametrize(
    "ident, expected",
    [
        ("api_keys", ("api_keys", None)),
        ("API_KEYS", ("api_keys", None)),
        ('"api_keys"', ("api_keys", None)),
        ("public.api_keys", ("api_keys", "public")),
        ('"public"."api_keys"', ("api_keys", "public")),
        ("pg_catalog.pg_class", ("pg_class", "pg_catalog")),
        ('"weird.name"', ("weird.name", None)),  # dot is inside quotes
        ('"has""quote"', ('has"quote', None)),
    ],
)
def test_normalize_table_ref(ident: str, expected: tuple[str, str | None]):
    assert _normalize_table_ref(ident) == expected


# ── _strip_sql_noise ────────────────────────────────────────────────────────

def test_strip_sql_noise_blanks_string_literal():
    sql = "SELECT 'pg_catalog.pg_class' FROM attachments"
    scrubbed = _strip_sql_noise(sql)
    assert "pg_class" not in scrubbed
    assert "attachments" in scrubbed


def test_strip_sql_noise_blanks_line_comment():
    sql = "SELECT 1 FROM attachments -- DROP TABLE api_keys\n"
    scrubbed = _strip_sql_noise(sql)
    assert "DROP" not in scrubbed
    assert "attachments" in scrubbed


def test_strip_sql_noise_blanks_block_comment():
    sql = "SELECT * /* FROM pg_user */ FROM attachments"
    scrubbed = _strip_sql_noise(sql)
    assert "pg_user" not in scrubbed
    assert "attachments" in scrubbed


# ── _validate_query_safety: allowed ─────────────────────────────────────────

@pytest.mark.parametrize(
    "sql",
    [
        # SYSTEM_PROMPT example #1
        "SELECT id, thread_key, name, mime_type, length(data) as bytes "
        "FROM attachments ORDER BY created_at DESC LIMIT 10",
        # SYSTEM_PROMPT example #2
        "SELECT execution_id, thread_key, status, harness, created_at, "
        "started_at, completed_at, EXTRACT(EPOCH FROM (completed_at - started_at)) "
        "as duration_s, result_text FROM agent_execution_requests "
        "ORDER BY created_at DESC LIMIT 20",
        # Schema-qualified to the allowlisted schema
        "SELECT * FROM public.api_keys",
        # Quoted identifier
        'SELECT * FROM "api_keys"',
        # Quoted schema+table
        'SELECT * FROM "public"."chat_messages"',
        # JOIN across two allowlisted tables
        "SELECT a.id, m.thread_key FROM attachments a JOIN chat_messages m "
        "ON a.message_id = m.id",
        # IN subquery against another allowlisted table
        "SELECT * FROM chat_messages WHERE thread_key IN "
        "(SELECT thread_key FROM sandbox_sessions WHERE state = 'running')",
        # String literal that happens to look like a forbidden table name
        "SELECT 'pg_catalog.pg_class' AS lookalike FROM attachments",
        # Block comment that hides a forbidden reference
        "SELECT * /* FROM pg_authid */ FROM attachments",
    ],
)
def test_validate_allows_documented_queries(sql: str):
    # Must not raise
    _validate_query_safety(sql)


# ── _validate_query_safety: rejected ────────────────────────────────────────

@pytest.mark.parametrize(
    "sql, reason_substr",
    [
        # System catalogs that would leak schema/role data
        ("SELECT * FROM pg_authid", "outside the read allowlist"),
        ("SELECT * FROM pg_catalog.pg_class", "non-allowlisted schema"),
        ("SELECT * FROM information_schema.tables", "non-allowlisted schema"),
        # User-controlled non-allowlisted tables that DO live in the
        # production schema today (these are what would expose Slack PII,
        # company context docs, meeting transcripts, workflow IO, etc.)
        ("SELECT * FROM slack_sync_messages", "outside the read allowlist"),
        ("SELECT * FROM company_context_documents", "outside the read allowlist"),
        ("SELECT * FROM muesli_meetings", "outside the read allowlist"),
        ("SELECT * FROM workflow_runs", "outside the read allowlist"),
        ("SELECT * FROM usage_stats", "outside the read allowlist"),
        # JOIN to a non-allowlisted table
        (
            "SELECT a.id FROM attachments a JOIN slack_sync_messages m "
            "ON a.thread_key = m.thread_key",
            "outside the read allowlist",
        ),
        # Schema-qualified, quoted, non-public
        ('SELECT * FROM "pg_catalog"."pg_authid"', "non-allowlisted schema"),
        # Three-part identifiers (db.schema.table) are not allowed — caller
        # cannot escape the public schema via cross-db references.
        (
            "SELECT * FROM other_db.public.api_keys",
            "non-allowlisted schema",
        ),
    ],
)
def test_validate_rejects_non_allowlisted(sql: str, reason_substr: str):
    with pytest.raises(HTTPException) as excinfo:
        _validate_query_safety(sql)
    assert excinfo.value.status_code == 400
    assert reason_substr in excinfo.value.detail


# ── _validate_query_safety: dangerous functions ─────────────────────────────

@pytest.mark.parametrize(
    "sql, fn",
    [
        ("SELECT pg_read_file('/etc/passwd')", "pg_read_file"),
        ("SELECT * FROM attachments WHERE id = pg_ls_dir('/var/lib/postgresql')[1]", "pg_ls_dir"),
        ("SELECT * FROM dblink('dbname=x', 'SELECT 1') AS t(x int)", "dblink"),
        ("SELECT pg_terminate_backend(12345)", "pg_terminate_backend"),
        # Mixed-case / whitespace around the parens — must still be caught
        ("SELECT  Pg_Read_File ( '/etc/hostname' )", "pg_read_file"),
    ],
)
def test_validate_rejects_blocked_functions(sql: str, fn: str):
    with pytest.raises(HTTPException) as excinfo:
        _validate_query_safety(sql)
    assert excinfo.value.status_code == 400
    assert fn in excinfo.value.detail


def test_validate_allows_blocked_function_name_in_string_literal():
    """A function name that only appears inside a string literal must not
    trip the dangerous-function check — otherwise audit-style queries
    that mention these names in logged content would be unfairly blocked.
    """
    sql = "SELECT * FROM chat_messages WHERE parts::text LIKE '%pg_read_file(%'"
    _validate_query_safety(sql)  # must not raise
