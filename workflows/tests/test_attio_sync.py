from __future__ import annotations

import asyncio
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
