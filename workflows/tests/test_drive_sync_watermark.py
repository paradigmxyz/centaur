"""The sync watermark must not advance past a document whose fetch failed.

The next incremental run only queries files modified after
(watermark - overlap), so a failed file left below the stored watermark is
never fetched again and its content stays missing from company context until
the source document happens to be edited.
"""

from __future__ import annotations

import asyncio
import datetime as dt
import importlib
import json
import os
import sys
import types
from pathlib import Path


def _load_drive_sync_module():
    repo_root = Path(__file__).resolve().parents[2]
    if str(repo_root) not in sys.path:
        sys.path.insert(0, str(repo_root))

    api_module = sys.modules.get("api") or types.ModuleType("api")
    runtime_control = sys.modules.get("api.runtime_control") or types.ModuleType(
        "api.runtime_control"
    )
    if not hasattr(runtime_control, "canonical_json"):
        runtime_control.canonical_json = lambda value: json.dumps(
            value, sort_keys=True
        )

    etl_metrics = sys.modules.get("workflows.etl_metrics") or types.ModuleType(
        "workflows.etl_metrics"
    )
    # etl_metrics re-exports the legacy api.metrics surface; satisfy any
    # metric helper with a no-op so importing modules load without it.
    etl_metrics.__getattr__ = lambda _name: (lambda *_args, **_kwargs: None)

    workflow_engine = sys.modules.get("api.workflow_engine") or types.ModuleType(
        "api.workflow_engine"
    )
    if not hasattr(workflow_engine, "WorkflowContext"):
        workflow_engine.WorkflowContext = object

    api_metrics = sys.modules.get("api.metrics") or types.ModuleType("api.metrics")
    api_metrics.__getattr__ = lambda _name: (lambda *_args, **_kwargs: None)

    api_module.runtime_control = runtime_control
    api_module.workflow_engine = workflow_engine
    api_module.metrics = api_metrics
    sys.modules.setdefault("api", api_module)
    sys.modules["api.runtime_control"] = runtime_control
    sys.modules["api.metrics"] = api_metrics
    sys.modules["workflows.etl_metrics"] = etl_metrics
    sys.modules["api.workflow_engine"] = workflow_engine

    return importlib.import_module("workflows.gsuite.drive_sync")


drive_sync = _load_drive_sync_module()

GOOGLE_DOC_MIME_TYPE = drive_sync.GOOGLE_DOC_MIME_TYPE


class FakePool:
    def __init__(self) -> None:
        self.execute_calls: list[tuple[str, tuple]] = []
        self.fetchrow_calls: list[tuple[str, tuple]] = []

    async def execute(self, sql: str, *args) -> str:
        self.execute_calls.append((sql, args))
        return "OK"

    async def fetchrow(self, sql: str, *args):
        self.fetchrow_calls.append((sql, args))
        return None

    def checkpoint_watermark(self):
        """The watermark written by the success checkpoint upsert."""
        for sql, args in self.execute_calls:
            if "google_drive_sync_checkpoints" in sql and "last_success_at" in sql:
                return args[1]
        raise AssertionError("no success checkpoint was written")


class FakeCtx:
    def __init__(self, pool: FakePool) -> None:
        self._pool = pool
        self.run_id = "wf-run-1"

    def log(self, *_args, **_kwargs) -> None:
        pass


class FakeClient:
    def __init__(self, files: list[dict], fail_ids: set[str]) -> None:
        self._files = files
        self._fail_ids = fail_ids

    def list_docs(self, *, query: str, page_size: int, page_token=None) -> dict:
        return {"files": self._files}

    def docs_get_text(self, document_id: str) -> str:
        if document_id in self._fail_ids:
            raise RuntimeError("429 rate limited")
        return f"text:{document_id}"


def _doc(file_id: str, modified: str | None) -> dict:
    file = {"id": file_id, "mimeType": GOOGLE_DOC_MIME_TYPE, "name": file_id}
    if modified is not None:
        file["modifiedTime"] = modified
    return file


def _run_handler(files: list[dict], fail_ids: set[str]):
    pool = FakePool()
    original_client = drive_sync._client
    drive_sync._client = lambda: FakeClient(files, fail_ids)
    os.environ["GOOGLE_DRIVE_ETL_ENABLED"] = "true"
    try:
        result = asyncio.run(drive_sync.handler(drive_sync.Input(), FakeCtx(pool)))
    finally:
        drive_sync._client = original_client
        os.environ.pop("GOOGLE_DRIVE_ETL_ENABLED", None)
    return result, pool


def test_watermark_is_clamped_to_the_earliest_failed_fetch() -> None:
    # B (10:00) fails while A (12:00) succeeds. Advancing the watermark to
    # 12:00 would leave B below every future incremental query; the checkpoint
    # must stop at B's modifiedTime so the next run retries it.
    result, pool = _run_handler(
        files=[
            _doc("doc-a", "2026-07-07T12:00:00Z"),
            _doc("doc-b", "2026-07-07T10:00:00Z"),
        ],
        fail_ids={"doc-b"},
    )

    assert result["status"] == "partial_failed"
    watermark = pool.checkpoint_watermark()
    assert watermark == dt.datetime(2026, 7, 7, 10, 0, tzinfo=dt.timezone.utc)


def test_watermark_is_preserved_when_a_failure_has_no_modified_time() -> None:
    # A failure with no usable modifiedTime cannot be re-included by clamping;
    # the checkpoint upsert must write NULL, which keeps the stored watermark
    # (COALESCE) so the next run rescans the same window.
    result, pool = _run_handler(
        files=[
            _doc("doc-a", "2026-07-07T12:00:00Z"),
            _doc("doc-b", None),
        ],
        fail_ids={"doc-b"},
    )

    assert result["status"] == "partial_failed"
    assert pool.checkpoint_watermark() is None


def test_watermark_advances_normally_when_every_fetch_succeeds() -> None:
    result, pool = _run_handler(
        files=[
            _doc("doc-a", "2026-07-07T12:00:00Z"),
            _doc("doc-b", "2026-07-07T10:00:00Z"),
        ],
        fail_ids=set(),
    )

    assert result["status"] == "completed"
    watermark = pool.checkpoint_watermark()
    assert watermark == dt.datetime(2026, 7, 7, 12, 0, tzinfo=dt.timezone.utc)
