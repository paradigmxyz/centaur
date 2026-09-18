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


def test_attio_sync_uses_one_week_cutoff_on_every_page(monkeypatch):
    attio = _load("workflows.attio_sync")
    now = dt.datetime(2026, 9, 17, 13, 15, tzinfo=dt.UTC)

    class Clock(dt.datetime):
        @classmethod
        def now(cls, _tz=None):
            nonlocal now
            value = now
            now += dt.timedelta(minutes=1)
            return value

    monkeypatch.setattr(attio.dt, "datetime", Clock)
    calls = []

    class Client:
        async def list_meetings(self, **kwargs):
            calls.append(kwargs)
            return {"data": [], "next_cursor": "page-2" if len(calls) == 1 else None}

    asyncio.run(
        attio._sync_meetings(
            client=Client(),
            pool=None,
            page_size=17,
            updated_after=Clock(2026, 9, 16, 9, tzinfo=dt.UTC),
            max_meetings=None,
            include_transcripts=False,
            run_id="run_1",
        )
    )
    assert calls == [
        {
            "limit": 17,
            "cursor": cursor,
            "sort": "start_asc",
            "ends_from": "2026-09-16T09:00:00Z",
            "starts_before": "2026-09-24T13:15:00Z",
        }
        for cursor in (None, "page-2")
    ]


def test_attio_sync_parallelism_limit_counts_and_failure_order(monkeypatch):
    attio = _load("workflows.attio_sync")
    active = peak = 0
    fetched = []
    written = []
    first_batch_started = asyncio.Event()
    later_meeting_written = asyncio.Event()

    class Client:
        async def list_meetings(self, **kwargs):
            assert kwargs["cursor"] is None
            return {
                "data": [{"id": {"meeting_id": str(i)}} for i in range(8)],
                "next_cursor": "next-page",
            }

        async def get_meeting(self, meeting_id):
            nonlocal active, peak
            fetched.append(meeting_id)
            active += 1
            peak = max(peak, active)
            if active == 5:
                first_batch_started.set()
            await first_batch_started.wait()
            await asyncio.sleep(0)
            active -= 1
            return {"id": {"meeting_id": meeting_id}}

        async def list_call_recordings(self, meeting_id, **_kwargs):
            return {
                "data": [{"id": {"call_recording_id": meeting_id}}]
                if int(meeting_id) % 2
                else []
            }

        async def get_call_transcript(self, meeting_id, recording_id, **_kwargs):
            assert meeting_id == recording_id
            return {"data": [{"text": f"Transcript {meeting_id}"}]}

    async def upsert(_pool, *, meeting, call_recordings, transcript_payload, run_id):
        meeting_id = meeting["id"]["meeting_id"]
        assert run_id == "run_parallel"
        assert (
            bool(call_recordings)
            == bool(transcript_payload)
            == bool(int(meeting_id) % 2)
        )
        if meeting_id == "1":
            await later_meeting_written.wait()
            raise RuntimeError("database write failed")
        written.append(meeting_id)
        if meeting_id == "4":
            later_meeting_written.set()
        return dt.datetime(2026, 7, 1 + int(meeting_id), tzinfo=dt.UTC)

    monkeypatch.setattr(attio, "_upsert_meeting", upsert)

    async def run():
        return await asyncio.wait_for(
            attio._sync_meetings(
                client=Client(),
                pool=None,
                page_size=8,
                updated_after=None,
                max_meetings=7,
                include_transcripts=True,
                run_id="run_parallel",
            ),
            timeout=2,
        )

    result = asyncio.run(run())
    assert peak == 5
    assert fetched == [str(i) for i in range(7)]
    assert set(written) == {"0", "2", "3", "4", "5", "6"}
    assert result.meetings_seen == 7
    assert result.meetings_upserted == 6
    assert result.call_recordings_seen == result.transcripts_upserted == 3
    assert result.detail_failures == ["1: database write failed"]
    assert result.watermark == dt.datetime(2026, 7, 1, tzinfo=dt.UTC)


def test_attio_nested_dates_do_not_advance_watermark_into_future(monkeypatch):
    attio = _load("workflows.attio_sync")
    now = dt.datetime(2026, 9, 17, 13, 15, tzinfo=dt.UTC)

    class Clock(dt.datetime):
        @classmethod
        def now(cls, _tz=None):
            return now

    monkeypatch.setattr(attio.dt, "datetime", Clock)
    writes = []

    class Pool:
        async def execute(self, _sql, *args):
            writes.append(args)

    class Client:
        async def list_meetings(self, **kwargs):
            assert kwargs["starts_before"] == "2026-09-24T13:15:00Z"
            return {"data": [{"id": {"meeting_id": "upcoming"}}]}

        async def get_meeting(self, meeting_id):
            return {
                "id": {"meeting_id": meeting_id},
                "start": {"datetime": "2026-09-23T10:00:00+02:00"},
                "end": {"datetime": "2026-09-23T11:30:00+02:00"},
                "created_at": "2025-12-09T19:21:06.771Z",
            }

    result = asyncio.run(
        attio._sync_meetings(
            client=Client(),
            pool=Pool(),
            page_size=50,
            updated_after=None,
            max_meetings=None,
            include_transcripts=False,
            run_id="run_1",
        )
    )
    assert result.meetings_upserted == 1
    assert writes[0][14] == Clock(2026, 9, 23, 8, tzinfo=dt.UTC)
    assert writes[0][15] == writes[0][17] == Clock(2026, 9, 23, 9, 30, tzinfo=dt.UTC)
    assert result.watermark == now
    assert attio._source_datetime({"start": {"date": "2026-09-20"}}, "start") == Clock(
        2026, 9, 20, tzinfo=dt.UTC
    )
    assert attio._source_datetime(
        {"starts_at": "2026-09-19T16:00:00Z"}, "starts_at"
    ) == Clock(2026, 9, 19, 16, tzinfo=dt.UTC)
