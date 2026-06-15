from __future__ import annotations

import datetime as dt
import importlib
import json
import sys
import types
from pathlib import Path


def _load_projection_module():
    repo_root = Path(__file__).resolve().parents[2]
    if str(repo_root) not in sys.path:
        sys.path.insert(0, str(repo_root))

    api_module = sys.modules.get("api") or types.ModuleType("api")
    runtime_control = sys.modules.get("api.runtime_control") or types.ModuleType(
        "api.runtime_control"
    )
    runtime_control.canonical_json = lambda value: json.dumps(value, sort_keys=True)
    runtime_control.decode_jsonb = lambda value, default: (
        value if value is not None else default
    )

    vm_metrics = types.ModuleType("api.vm_metrics")
    for name in (
        "observe_company_context_document_size",
        "record_company_context_documents_changed",
        "set_company_context_projection_lag",
        "set_etl_active_scopes",
        "set_etl_failed_scopes",
        "set_etl_scope_sync_freshness_seconds",
    ):
        setattr(vm_metrics, name, lambda *_args, **_kwargs: None)

    workflow_engine = types.ModuleType("api.workflow_engine")
    workflow_engine.WorkflowContext = object

    api_module.runtime_control = runtime_control
    api_module.vm_metrics = vm_metrics
    api_module.workflow_engine = workflow_engine
    sys.modules.setdefault("api", api_module)
    sys.modules.setdefault("api.runtime_control", runtime_control)
    sys.modules.setdefault("api.vm_metrics", vm_metrics)
    sys.modules.setdefault("api.workflow_engine", workflow_engine)

    return importlib.import_module("workflows.company_context_documents")


projection = _load_projection_module()


def test_slack_attachment_document_indexes_metadata_without_private_url():
    row = {
        "channel_id": "C123",
        "channel_name": "eng",
        "message_ts": "1770000000.000100",
        "slack_file_id": "F123",
        "name": "roadmap.pdf",
        "title": "Q3 Roadmap",
        "mimetype": "application/pdf",
        "filetype": "pdf",
        "size_bytes": 12345,
        "permalink": "https://example.slack.com/files/U123/F123/roadmap.pdf",
        "download_status": "downloaded",
        "download_error": "",
        "content_sha256": "abc123",
        "updated_at": dt.datetime(2026, 6, 15, 12, 1, tzinfo=dt.UTC),
        "occurred_at": dt.datetime(2026, 6, 15, 12, 0, tzinfo=dt.UTC),
        "thread_ts": "1770000000.000100",
        "parent_message_ts": None,
        "user_id": "U123",
        "user_name": "alice",
        "real_name": "Alice Example",
        "display_name": "alice",
        "text": "Please review <#C999|product> and <@U456>",
        "message_permalink": "https://example.slack.com/archives/C123/p1770000000000100",
        "url_private": "https://files.slack.com/files-pri/T/F123/roadmap.pdf",
    }

    document = projection._slack_attachment_document(
        row,
        users_by_id={"U456": "bob"},
        channels_by_id={"C999": "product"},
    )

    assert document is not None
    assert document["document_id"] == "slack:attachment:C123:1770000000.000100:F123"
    assert document["source_type"] == "slack_attachment"
    assert document["title"] == "Slack attachment: Q3 Roadmap"
    assert document["url"] == "https://example.slack.com/files/U123/F123/roadmap.pdf"
    assert "- Filename: roadmap.pdf" in document["body"]
    assert "- MIME type: application/pdf" in document["body"]
    assert "- File type: pdf" in document["body"]
    assert "- Content SHA-256: abc123" in document["body"]
    assert "Please review #product and @bob" in document["body"]
    assert "files-pri" not in document["body"]
    assert "url_private" not in document["metadata"]
    assert document["metadata"]["message_permalink"].endswith("p1770000000000100")
