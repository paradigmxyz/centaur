from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest
from typer.testing import CliRunner

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from company_context import cli


@pytest.mark.parametrize(
    ("args", "client_method"),
    [
        (["query", "SELECT 1"], "query"),
        (["search", "roadmap"], "search"),
        (["search-dm-conversations", "alex"], "search_dm_conversations"),
        (["search-dms", "roadmap"], "search_dms"),
        (["list"], "list_documents"),
        (["read", "doc-1"], "read_document"),
    ],
)
def test_commands_default_to_json(monkeypatch, args, client_method):
    payload = {
        "status": "ok",
        "results": [{"document_id": "doc-1", "title": "Roadmap"}],
    }
    fake_client = type(
        "FakeClient",
        (),
        {client_method: lambda self, **kwargs: payload},
    )

    monkeypatch.setattr(cli, "CompanyContextClient", fake_client)

    result = CliRunner().invoke(cli.app, args)

    assert result.exit_code == 0, result.output
    assert json.loads(result.output) == payload


def test_search_table_flag_uses_human_readable_output(monkeypatch):
    class FakeClient:
        def search(self, **kwargs):
            return {
                "status": "ok",
                "results": [
                    {
                        "document_id": "doc-1",
                        "source": "slack",
                        "source_type": "message",
                        "occurred_at": "2026-09-10",
                        "title": "Roadmap",
                        "preview": "Launch plans",
                    }
                ],
            }

    monkeypatch.setattr(cli, "CompanyContextClient", FakeClient)

    result = CliRunner().invoke(cli.app, ["search", "roadmap", "--table"])

    assert result.exit_code == 0, result.output
    assert "Company Context Search (1)" in result.output
    assert "Roadmap" in result.output


def test_search_no_hybrid_flag_forces_keyword_mode(monkeypatch):
    calls = []

    class FakeClient:
        def search(self, **kwargs):
            calls.append(kwargs)
            return {"status": "ok", "results": [], "search_mode": "keyword"}

    monkeypatch.setattr(cli, "CompanyContextClient", FakeClient)

    result = CliRunner().invoke(
        cli.app,
        ["search", "roadmap", "--no-hybrid", "--json"],
    )

    assert result.exit_code == 0, result.output
    assert calls == [
        {
            "query": "roadmap",
            "limit": 10,
            "source": None,
            "source_type": None,
            "occurred_after": None,
            "occurred_before": None,
            "hybrid": False,
        }
    ]
