from __future__ import annotations

import asyncio
import datetime as dt
import importlib
import json
import sys
import types


def _install_workflow_stubs() -> None:
    api_module = sys.modules.get("api") or types.ModuleType("api")
    runtime_control = sys.modules.get("api.runtime_control") or types.ModuleType(
        "api.runtime_control"
    )
    runtime_control.canonical_json = lambda value: json.dumps(value, sort_keys=True)

    etl_metrics = types.ModuleType("workflows.etl_metrics")
    for name in (
        "record_etl_items_failed",
        "record_etl_items_seen",
        "record_etl_items_upserted",
    ):
        setattr(etl_metrics, name, lambda *_args, **_kwargs: None)

    workflow_engine = types.ModuleType("api.workflow_engine")
    workflow_engine.WorkflowContext = object

    slack_shared = types.ModuleType("workflows.slack.shared")
    slack_shared.env_flag_enabled = lambda _name, default=True: default
    slack_shared.positive_int = lambda value, default: (
        int(value) if value is not None and int(value) > 0 else default
    )

    api_module.runtime_control = runtime_control
    api_module.workflow_engine = workflow_engine
    sys.modules.setdefault("api", api_module)
    sys.modules["api.runtime_control"] = runtime_control
    sys.modules["api.workflow_engine"] = workflow_engine
    sys.modules["workflows.etl_metrics"] = etl_metrics
    sys.modules["workflows.slack.shared"] = slack_shared


def _load(name: str):
    _install_workflow_stubs()
    return importlib.import_module(name)


def test_attio_page_helpers_accept_common_cursor_shapes():
    attio = _load("workflows.attio_sync")

    assert attio._page_items({"data": [{"id": 1}, "skip"]}) == [{"id": 1}]
    assert attio._page_items({"data": {"data": [{"id": 2}]}}) == [{"id": 2}]
    assert attio._page_items({"meetings": [{"id": 3}]}) == [{"id": 3}]
    assert attio._next_cursor({"pagination": {"next_cursor": "cur_1"}}) == "cur_1"
    assert attio._next_cursor({"meta": {"nextCursor": "cur_2"}}) == "cur_2"


def test_attio_transcript_text_uses_speaker_or_participant():
    attio = _load("workflows.attio_sync")

    text = attio._transcript_text(
        [
            {"speaker": {"name": "Dana"}, "text": "Budget approved"},
            {"participant": {"display_name": "Eli"}, "content": "Sending next steps"},
            {"speaker_name": "Fran", "transcript": "Thanks"},
        ]
    )

    assert text == "Dana: Budget approved\nEli: Sending next steps\nFran: Thanks"


def test_attio_sync_uses_supported_meeting_sort():
    attio = _load("workflows.attio_sync")

    class FakeAttioClient:
        def __init__(self) -> None:
            self.sort = None

        async def list_meetings(self, **kwargs):
            self.sort = kwargs.get("sort")
            return {"data": []}

    client = FakeAttioClient()

    asyncio.run(
        attio._sync_meetings(
            client=client,
            pool=None,
            page_size=50,
            updated_after=None,
            max_meetings=None,
            include_transcripts=False,
            run_id="run_1",
        )
    )

    assert client.sort == "start_asc"


def test_attio_sync_retries_transient_meeting_detail_failure(monkeypatch):
    attio = _load("workflows.attio_sync")

    class FakeAttioClient:
        def __init__(self) -> None:
            self.detail_attempts = 0

        async def list_meetings(self, **_kwargs):
            return {"data": [{"id": {"meeting_id": "mtg_1"}}]}

        async def get_meeting(self, _meeting_id):
            self.detail_attempts += 1
            if self.detail_attempts < 3:
                raise RuntimeError("Name or service not known")
            return {"id": {"meeting_id": "mtg_1"}, "updated_at": "2026-07-10T12:00:00Z"}

    async def fake_upsert(*_args, **_kwargs):
        return dt.datetime(2026, 7, 10, 12, tzinfo=dt.UTC)

    sleeps: list[int] = []

    async def fake_sleep(delay):
        sleeps.append(delay)

    monkeypatch.setattr(attio, "_upsert_meeting", fake_upsert)
    monkeypatch.setattr(attio.asyncio, "sleep", fake_sleep)
    client = FakeAttioClient()

    result = asyncio.run(
        attio._sync_meetings(
            client=client,
            pool=None,
            page_size=50,
            updated_after=None,
            max_meetings=None,
            include_transcripts=False,
            run_id="run_1",
        )
    )

    assert client.detail_attempts == 3
    assert sleeps == [1, 2]
    assert result.meetings_seen == 1
    assert result.meetings_upserted == 1
    assert result.detail_failures == []


def test_attio_sync_continues_after_exhausted_detail_failure(monkeypatch):
    attio = _load("workflows.attio_sync")

    class FakeAttioClient:
        def __init__(self) -> None:
            self.detail_attempts: dict[str, int] = {}

        async def list_meetings(self, **_kwargs):
            return {
                "data": [
                    {"id": {"meeting_id": "mtg_failed"}},
                    {"id": {"meeting_id": "mtg_ok"}},
                ]
            }

        async def get_meeting(self, meeting_id):
            self.detail_attempts[meeting_id] = (
                self.detail_attempts.get(meeting_id, 0) + 1
            )
            if meeting_id == "mtg_failed":
                raise RuntimeError("Name or service not known")
            return {
                "id": {"meeting_id": meeting_id},
                "updated_at": "2026-07-10T12:00:00Z",
            }

    async def fake_upsert(*_args, **_kwargs):
        return dt.datetime(2026, 7, 10, 12, tzinfo=dt.UTC)

    async def no_sleep(_delay):
        return None

    monkeypatch.setattr(attio, "_upsert_meeting", fake_upsert)
    monkeypatch.setattr(attio.asyncio, "sleep", no_sleep)
    client = FakeAttioClient()

    result = asyncio.run(
        attio._sync_meetings(
            client=client,
            pool=None,
            page_size=50,
            updated_after=None,
            max_meetings=None,
            include_transcripts=False,
            run_id="run_1",
        )
    )

    assert client.detail_attempts == {"mtg_failed": 3, "mtg_ok": 1}
    assert result.meetings_seen == 2
    assert result.meetings_upserted == 1
    assert len(result.detail_failures) == 1
    assert result.watermark is None


def test_attio_handler_records_partial_detail_progress(monkeypatch):
    attio = _load("workflows.attio_sync")

    class FakeContext:
        run_id = "workflow-run-1"
        _pool = object()

        def __init__(self) -> None:
            self.logs: list[tuple[str, dict]] = []

        def log(self, message, **fields):
            self.logs.append((message, fields))

    async def no_op(*_args, **_kwargs):
        return None

    async def fake_sync(**_kwargs):
        return attio.SyncResult(
            meetings_seen=3,
            meetings_upserted=2,
            call_recordings_seen=1,
            transcripts_upserted=1,
            detail_failures=["mtg_failed: Name or service not known"],
        )

    recorded: dict[str, object] = {}

    async def record_finish(*_args, **kwargs):
        recorded.update(kwargs)

    monkeypatch.setattr(attio, "env_flag_enabled", lambda *_args, **_kwargs: True)
    monkeypatch.setattr(attio, "_record_run_start", no_op)
    monkeypatch.setattr(attio, "_load_checkpoint", no_op)
    monkeypatch.setattr(attio, "_sync_meetings", fake_sync)
    monkeypatch.setattr(attio, "_update_checkpoint_failure", no_op)
    monkeypatch.setattr(attio, "_record_run_finish", record_finish)
    ctx = FakeContext()

    result = asyncio.run(attio.handler(attio.Input(), ctx))

    assert result["status"] == "failed"
    assert result["meetings_seen"] == 3
    assert result["meetings_upserted"] == 2
    assert recorded["status"] == "failed"
    assert recorded["counts"] == {
        "meetings_seen": 3,
        "meetings_upserted": 2,
        "call_recordings_seen": 1,
        "transcripts_upserted": 1,
    }
    assert ctx.logs == [("attio_sync_meeting_details_failed", {"failures": 1})]
