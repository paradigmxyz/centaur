from __future__ import annotations

import asyncio
import inspect
import json

import httpx
import pytest

from meeting_scheduler import cli, client


async def _async_value(value):
    return value


def test_serialize_row_decodes_jsonb_metadata_from_asyncpg():
    row = {
        "occurrence_key": "meeting:1",
        "metadata": json.dumps(
            {
                "post_meeting_zoom_uuid": "/abc+def==",
                "post_meeting_status": "processing",
            }
        ),
    }

    serialized = client._serialize_row(row)

    assert serialized is not None
    assert serialized["metadata"] == {
        "post_meeting_zoom_uuid": "/abc+def==",
        "post_meeting_status": "processing",
    }


@pytest.mark.parametrize("metadata", ["{invalid", "[]"])
def test_serialize_row_preserves_unrecognized_jsonb_metadata(metadata):
    serialized = client._serialize_row({"occurrence_key": "meeting:1", "metadata": metadata})

    assert serialized is not None
    assert serialized["metadata"] == metadata


def test_zoom_create_defaults_to_centaur_join_anytime_and_cloud_recording(monkeypatch):
    monkeypatch.setenv("MEETING_ZOOM_HOST_USER_ID", "centaur@example.com")
    monkeypatch.setenv("MEETING_ZOOM_SCHEDULE_FOR_USERS", "{}")
    scheduler = client.MeetingSchedulerClient()
    calls = []
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda method, path, **kwargs: calls.append((method, path, kwargs)) or {"id": "1"},
    )

    scheduler._zoom_create(
        title="Planning",
        start=client._parse_rfc3339("2099-08-17T10:00:00Z", field="start"),
        duration=30,
        time_zone="UTC",
        occurrence_key="cadence:1",
        organizer_calendar_key="default",
    )

    payload = calls[0][2]["payload"]
    assert calls[0][1] == "/users/me/meetings"
    assert payload["settings"] == {
        "auto_recording": "cloud",
        "join_before_host": True,
        "jbh_time": 0,
        "meeting_authentication": False,
        "waiting_room": False,
    }
    assert "tracking_fields" not in payload
    assert payload["agenda"].startswith("centaur-occurrence:")
    assert "schedule_for" not in payload


def test_zoom_create_never_delegates_to_another_user(monkeypatch):
    monkeypatch.setenv("MEETING_ZOOM_HOST_USER_ID", "centaur@example.com")
    monkeypatch.setenv("MEETING_ZOOM_SCHEDULE_FOR_USERS", json.dumps({"default": "delegate@example.com"}))
    scheduler = client.MeetingSchedulerClient()
    calls = []
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda method, path, **kwargs: calls.append(kwargs) or {"id": "1"},
    )

    scheduler._zoom_create(
        title="Planning",
        start=client._parse_rfc3339("2099-08-17T10:00:00Z", field="start"),
        duration=30,
        time_zone="UTC",
        occurrence_key="cadence:1",
        organizer_calendar_key="default",
    )

    assert "schedule_for" not in calls[0]["payload"]


def test_zoom_create_assigns_requester_as_alternative_host_without_delegating(
    monkeypatch,
):
    monkeypatch.setenv("MEETING_ZOOM_HOST_USER_ID", "centaur@example.com")
    scheduler = client.MeetingSchedulerClient()
    calls = []
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda method, path, **kwargs: calls.append((method, path, kwargs)) or {"id": "1"},
    )

    scheduler._zoom_create(
        title="Planning",
        start=client._parse_rfc3339("2099-08-17T10:00:00Z", field="start"),
        duration=30,
        time_zone="UTC",
        occurrence_key="request:1",
        organizer_calendar_key="centaur",
        alternative_host_email="proposer@example.com",
    )

    payload = calls[0][2]["payload"]
    assert payload["settings"]["alternative_hosts"] == "proposer@example.com"
    assert "schedule_for" not in payload


def test_ensure_zoom_alternative_host_repairs_and_verifies_provider_state(monkeypatch):
    scheduler = client.MeetingSchedulerClient()
    calls = []
    responses = iter(
        [
            {"id": "1", "settings": {"alternative_hosts": ""}},
            {},
            {
                "id": "1",
                "join_url": "https://zoom.example/j/1",
                "settings": {"alternative_hosts": "proposer@example.com"},
            },
        ]
    )
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda method, path, **kwargs: calls.append((method, path, kwargs)) or next(responses),
    )

    result = scheduler._ensure_zoom_alternative_host(
        {"id": "1", "join_url": "https://zoom.example/j/1"},
        "PROPOSER@example.com",
    )

    assert [call[:2] for call in calls] == [
        ("GET", "/meetings/1"),
        ("PATCH", "/meetings/1"),
        ("GET", "/meetings/1"),
    ]
    assert calls[1][2]["payload"] == {"settings": {"alternative_hosts": "proposer@example.com"}}
    assert result["settings"]["alternative_hosts"] == "proposer@example.com"


def test_ensure_zoom_alternative_host_fails_when_zoom_does_not_apply_it(monkeypatch):
    scheduler = client.MeetingSchedulerClient()
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda method, path, **kwargs: {} if method == "PATCH" else {"id": "1", "settings": {}},
    )

    with pytest.raises(client.MeetingSchedulerError, match="did not assign"):
        scheduler._ensure_zoom_alternative_host(
            {"id": "1", "join_url": "https://zoom.example/j/1"},
            "proposer@example.com",
        )


def test_zoom_find_by_occurrence_uses_agenda_marker(monkeypatch):
    monkeypatch.setenv("MEETING_ZOOM_HOST_USER_ID", "centaur@example.com")
    scheduler = client.MeetingSchedulerClient()
    key = "request:agenda-marker"
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda *_args, **_kwargs: {
            "meetings": [
                {"id": "unrelated", "agenda": "other"},
                {"id": "expected", "agenda": client._zoom_occurrence_marker(key)},
            ]
        },
    )

    assert scheduler._zoom_find_by_occurrence(key)["id"] == "expected"


def test_get_recording_fetches_transcript_without_returning_signed_urls(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda *_args, **_kwargs: {
            "id": "123",
            "uuid": "/abc+def==",
            "topic": "Planning",
            "recording_files": [
                {
                    "id": "file-1",
                    "file_type": "TRANSCRIPT",
                    "status": "completed",
                    "download_url": "https://us02web.zoom.us/rec/download/signed",
                }
            ],
        },
    )
    monkeypatch.setattr(scheduler, "_zoom_download_transcript", lambda _url: "WEBVTT\n")

    result = scheduler.get_recording("123")

    assert result["transcript"] == "WEBVTT\n"
    assert result["transcript_status"] == "ready"
    assert result["meeting_uuid"] == "/abc+def=="
    assert "download_url" not in result["recording_files"][0]


def test_get_recording_keeps_empty_transcript_pending(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda *_args, **_kwargs: {
            "id": "123",
            "uuid": "/abc+def==",
            "recording_files": [
                {
                    "file_type": "TRANSCRIPT",
                    "status": "completed",
                    "download_url": "https://us02web.zoom.us/rec/download/signed",
                }
            ],
        },
    )
    monkeypatch.setattr(scheduler, "_zoom_download_transcript", lambda _url: "")

    result = scheduler.get_recording("/abc+def==")

    assert result["transcript"] == ""
    assert result["transcript_status"] == "pending"


def test_get_recording_resolves_missing_uuid_from_matching_past_instance(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    calls = []

    def zoom_request(method, path, **_kwargs):
        calls.append((method, path))
        if path == "/meetings/123/recordings":
            return {
                "id": "123",
                "start_time": "2026-09-02T14:19:01Z",
                "recording_files": [],
            }
        if path == "/past_meetings/123/instances":
            return {
                "meetings": [
                    {"uuid": "older-uuid", "start_time": "2026-08-26T14:19:01Z"},
                    {"uuid": "/abc+def==", "start_time": "2026-09-02T14:19:01Z"},
                ]
            }
        raise AssertionError(f"unexpected Zoom path: {path}")

    monkeypatch.setattr(scheduler, "_zoom_request", zoom_request)

    result = scheduler.get_recording("123")

    assert calls == [
        ("GET", "/meetings/123/recordings"),
        ("GET", "/past_meetings/123/instances"),
    ]
    assert result["meeting_uuid"] == "/abc+def=="


def test_get_recording_keeps_transcript_when_optional_instance_lookup_times_out(
    monkeypatch,
):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()

    def zoom_request(_method, path, **_kwargs):
        if path == "/meetings/123/recordings":
            return {
                "id": "123",
                "start_time": "2026-09-02T14:19:01Z",
                "recording_files": [
                    {
                        "file_type": "TRANSCRIPT",
                        "download_url": "https://zoom.us/recording.vtt",
                    }
                ],
            }
        raise httpx.ReadTimeout("past instances timed out")

    monkeypatch.setattr(scheduler, "_zoom_request", zoom_request)
    monkeypatch.setattr(scheduler, "_zoom_download_transcript", lambda _url: "spoken text")

    result = scheduler.get_recording("123")

    assert result["meeting_uuid"] is None
    assert result["meeting_uuid_resolution_error"] == "ReadTimeout"
    assert result["transcript_status"] == "ready"
    assert result["transcript"] == "spoken text"


def test_zoom_transcript_download_rejects_non_zoom_hosts():
    with pytest.raises(client.MeetingSchedulerError, match="invalid transcript"):
        client.MeetingSchedulerClient()._zoom_download_transcript(
            "https://example.com/recording.vtt"
        )


def test_zoom_transcript_download_follows_bounded_zoom_redirect(monkeypatch):
    scheduler = client.MeetingSchedulerClient()
    requests = []

    def handler(request):
        requests.append(request)
        if request.url.host == "us02web.zoom.us":
            return httpx.Response(
                302,
                headers={"location": "https://file.zoom.us/rec/transcript.vtt"},
            )
        return httpx.Response(200, content=b"WEBVTT\n\nHello world.")

    transport = httpx.MockTransport(handler)
    real_client = httpx.AsyncClient
    monkeypatch.setattr(
        client.httpx,
        "AsyncClient",
        lambda *args, **kwargs: real_client(*args, transport=transport, **kwargs),
    )
    monkeypatch.setattr(
        scheduler,
        "_zoom_headers",
        lambda: {"Authorization": "Bearer placeholder"},
    )

    transcript = scheduler._zoom_download_transcript("https://us02web.zoom.us/rec/download/signed")

    assert transcript == "WEBVTT\n\nHello world."
    assert [request.url.host for request in requests] == [
        "us02web.zoom.us",
        "file.zoom.us",
    ]
    assert requests[0].headers["authorization"] == "Bearer placeholder"
    assert "authorization" not in requests[1].headers


def test_zoom_transcript_download_rejects_redirect_outside_zoom(monkeypatch):
    scheduler = client.MeetingSchedulerClient()
    requests = []

    def handler(request):
        requests.append(request)
        return httpx.Response(302, headers={"location": "https://example.com/transcript.vtt"})

    transport = httpx.MockTransport(handler)
    real_client = httpx.AsyncClient
    monkeypatch.setattr(
        client.httpx,
        "AsyncClient",
        lambda *args, **kwargs: real_client(*args, transport=transport, **kwargs),
    )

    with pytest.raises(client.MeetingSchedulerError, match="invalid transcript"):
        scheduler._zoom_download_transcript("https://us02web.zoom.us/rec/download/signed")

    assert len(requests) == 1


@pytest.mark.parametrize(
    "location",
    [
        "http://file.zoom.us/transcript.vtt",
        "https://zoom.us.example.com/transcript.vtt",
    ],
)
def test_zoom_transcript_download_rejects_unsafe_redirects(monkeypatch, location):
    scheduler = client.MeetingSchedulerClient()
    requests = []

    def handler(request):
        requests.append(request)
        return httpx.Response(302, headers={"location": location})

    transport = httpx.MockTransport(handler)
    real_client = httpx.AsyncClient
    monkeypatch.setattr(
        client.httpx,
        "AsyncClient",
        lambda *args, **kwargs: real_client(*args, transport=transport, **kwargs),
    )

    with pytest.raises(client.MeetingSchedulerError, match="invalid transcript"):
        scheduler._zoom_download_transcript("https://us02web.zoom.us/rec/download/signed")

    assert len(requests) == 1


def test_zoom_transcript_download_supports_relative_redirect(monkeypatch):
    scheduler = client.MeetingSchedulerClient()
    requests = []

    def handler(request):
        requests.append(request)
        if request.url.path == "/rec/download/signed":
            return httpx.Response(302, headers={"location": "/rec/transcript.vtt"})
        return httpx.Response(200, content=b"WEBVTT\n\nHello world.")

    transport = httpx.MockTransport(handler)
    real_client = httpx.AsyncClient
    monkeypatch.setattr(
        client.httpx,
        "AsyncClient",
        lambda *args, **kwargs: real_client(*args, transport=transport, **kwargs),
    )

    assert (
        scheduler._zoom_download_transcript("https://us02web.zoom.us/rec/download/signed")
        == "WEBVTT\n\nHello world."
    )
    assert requests[1].url == httpx.URL("https://us02web.zoom.us/rec/transcript.vtt")


def test_zoom_transcript_download_enforces_redirect_limit(monkeypatch):
    scheduler = client.MeetingSchedulerClient()
    requests = []

    def handler(request):
        requests.append(request)
        return httpx.Response(302, headers={"location": "/rec/again"})

    transport = httpx.MockTransport(handler)
    real_client = httpx.AsyncClient
    monkeypatch.setattr(
        client.httpx,
        "AsyncClient",
        lambda *args, **kwargs: real_client(*args, transport=transport, **kwargs),
    )

    with pytest.raises(client.MeetingSchedulerError, match="redirect limit"):
        scheduler._zoom_download_transcript("https://us02web.zoom.us/rec/download/signed")

    assert len(requests) == 6


def test_zoom_transcript_download_rejects_redirect_without_location(monkeypatch):
    scheduler = client.MeetingSchedulerClient()
    transport = httpx.MockTransport(lambda _request: httpx.Response(302))
    real_client = httpx.AsyncClient
    monkeypatch.setattr(
        client.httpx,
        "AsyncClient",
        lambda *args, **kwargs: real_client(*args, transport=transport, **kwargs),
    )

    with pytest.raises(client.MeetingSchedulerError, match="had no location"):
        scheduler._zoom_download_transcript("https://us02web.zoom.us/rec/download/signed")


def test_zoom_transcript_download_enforces_overall_stream_deadline(monkeypatch):
    scheduler = client.MeetingSchedulerClient()

    class SlowStream(httpx.AsyncByteStream):
        async def __aiter__(self):
            await asyncio.sleep(0.05)
            yield b"WEBVTT\n"

    transport = httpx.MockTransport(lambda _request: httpx.Response(200, stream=SlowStream()))
    real_client = httpx.AsyncClient
    monkeypatch.setattr(
        client.httpx,
        "AsyncClient",
        lambda *args, **kwargs: real_client(*args, transport=transport, **kwargs),
    )
    monkeypatch.setattr(client, "ZOOM_TRANSCRIPT_DOWNLOAD_TIMEOUT_SECONDS", 0.01)

    with pytest.raises(client.MeetingSchedulerError, match="time limit"):
        scheduler._zoom_download_transcript("https://us02web.zoom.us/rec/download/signed")


def test_zoom_transcript_download_streams_with_size_limit(monkeypatch):
    scheduler = client.MeetingSchedulerClient()

    def handler(_request):
        return httpx.Response(200, content=b"x" * (client.MAX_TRANSCRIPT_BYTES + 1))

    transport = httpx.MockTransport(handler)
    real_client = httpx.AsyncClient
    monkeypatch.setattr(
        client.httpx,
        "AsyncClient",
        lambda *args, **kwargs: real_client(*args, transport=transport, **kwargs),
    )

    with pytest.raises(client.MeetingSchedulerError, match="size limit"):
        scheduler._zoom_download_transcript("https://us02web.zoom.us/rec/download/signed")


def test_get_summary_uses_zoom_summary_endpoint_and_strips_signed_urls(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    calls = []
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda method, path, **_kwargs: (
            calls.append((method, path))
            or {
                "meeting_id": "123",
                "meeting_summary": "Decisions were made.",
                "next_steps": ["Ship it"],
                "share_url": "https://zoom.us/private/signed",
            }
        ),
    )

    result = scheduler.get_summary("123")

    assert calls == [("GET", "/meetings/123/meeting_summary")]
    assert result["meeting_summary"] == "Decisions were made."
    assert result["next_steps"] == ["Ship it"]
    assert "share_url" not in result


def test_get_summary_double_encodes_completed_uuid_that_starts_with_slash(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    calls = []
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda method, path, **_kwargs: calls.append((method, path)) or {},
    )

    scheduler.get_summary("/abc+def==")

    assert calls == [("GET", "/meetings/%252Fabc%252Bdef%253D%253D/meeting_summary")]


def test_collect_post_meeting_artifacts_is_ready_with_transcript_and_summary(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    monkeypatch.setattr(
        scheduler,
        "get_recording",
        lambda _meeting_id: {
            "transcript_status": "ready",
            "transcript": "WEBVTT\nHello world.",
            "recording_files": [{"id": "file-1", "file_type": "TRANSCRIPT"}],
        },
    )
    monkeypatch.setattr(
        scheduler,
        "get_summary",
        lambda _meeting_id: {"meeting_summary": "Decisions were made."},
    )

    result = scheduler.collect_post_meeting_artifacts("123")

    assert result["ready"] is True
    assert result["transcript"] == "WEBVTT\nHello world."
    assert result["summary_text"] == "Decisions were made."
    assert result["summary_source"] == "zoom"
    assert result["processing_errors"] == []


def test_collect_post_meeting_artifacts_uses_recording_uuid_for_past_meeting_summary(
    monkeypatch,
):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    summary_ids = []
    monkeypatch.setattr(
        scheduler,
        "get_recording",
        lambda _meeting_id: {
            "meeting_id": "123",
            "meeting_uuid": "/abc+def==",
            "transcript_status": "ready",
            "transcript": "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nHello world.",
            "recording_files": [],
        },
    )
    monkeypatch.setattr(
        scheduler,
        "get_summary",
        lambda meeting_id: (
            summary_ids.append(meeting_id) or {"meeting_summary": "Decisions were made."}
        ),
    )

    result = scheduler.collect_post_meeting_artifacts("123")

    assert summary_ids == ["/abc+def=="]
    assert result["summary_text"] == "Decisions were made."


def test_collect_post_meeting_artifacts_is_ready_without_zoom_summary(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    monkeypatch.setattr(
        scheduler,
        "get_recording",
        lambda _meeting_id: {
            "transcript_status": "ready",
            "transcript": "WEBVTT\nHello world.",
            "recording_files": [],
        },
    )
    monkeypatch.setattr(scheduler, "get_summary", lambda _meeting_id: {})

    result = scheduler.collect_post_meeting_artifacts("123")

    assert result["ready"] is True
    assert result["summary_source"] == "unavailable"
    assert result["summary_text"] == ""
    assert result["processing_errors"] == []


def test_collect_post_meeting_artifacts_keeps_summary_processing_error_when_transcript_ready(
    monkeypatch,
):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    monkeypatch.setattr(
        scheduler,
        "get_recording",
        lambda _meeting_id: {"transcript_status": "ready", "transcript": "WEBVTT\n"},
    )

    def missing_summary(_meeting_id):
        raise client.MeetingSchedulerError("Zoom summary is still processing")

    monkeypatch.setattr(scheduler, "get_summary", missing_summary)

    result = scheduler.collect_post_meeting_artifacts("123")

    assert result["ready"] is True
    assert result["summary_source"] == "unavailable"
    assert result["processing_errors"] == ["Zoom summary is still processing"]


def test_collect_post_meeting_artifacts_retries_provider_processing(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()

    def pending(_meeting_id):
        raise client.MeetingSchedulerError("Zoom artifact is still processing")

    monkeypatch.setattr(scheduler, "get_recording", pending)
    monkeypatch.setattr(scheduler, "get_summary", pending)

    result = scheduler.collect_post_meeting_artifacts("123")

    assert result["ready"] is False
    assert result["transcript_status"] == "pending"
    assert result["summary_source"] == "unavailable"
    assert len(result["processing_errors"]) == 2


def test_terminal_zoom_event_candidate_accepts_booked_undelivered_early_end(
    monkeypatch,
):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    calls = []
    row = {
        "occurrence_key": "weekly-sync:2026-08-17",
        "status": "booked",
        "zoom_meeting_id": "123",
        "metadata": {"post_meeting_status": "processing"},
    }

    class Connection:
        async def fetchrow(self, query, *args):
            calls.append((query, args))
            return row

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)

    result = scheduler.post_meeting_candidate_for_terminal_zoom_event("123")

    assert result == row
    assert calls[0][1] == ("123",)
    assert "status = 'booked'" in calls[0][0]
    assert "zoom_meeting_id = $1" in calls[0][0]
    assert "make_interval" not in calls[0][0]
    assert "<= now()" not in calls[0][0]
    assert "post_meeting_status" in calls[0][0]


def test_post_meeting_candidate_by_zoom_id_keeps_end_gate(
    monkeypatch,
):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    calls = []

    class Connection:
        async def fetchrow(self, query, *args):
            calls.append((query, args))
            return None

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)

    assert scheduler.post_meeting_candidate_by_zoom_id("123") is None
    assert calls[0][1] == ("123",)
    assert "make_interval" in calls[0][0]
    assert "<= now()" in calls[0][0]


def test_post_meeting_candidates_keeps_scheduled_end_gate(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    calls = []

    class Connection:
        async def fetch(self, query, *args):
            calls.append((query, args))
            return []

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)

    assert scheduler.post_meeting_candidates("2026-08-31T20:15:00Z") == []
    assert "make_interval" in calls[0][0]
    assert "<= $1" in calls[0][0]


@pytest.mark.parametrize("meeting_id", ["", "   ", "x" * 129, "one two"])
def test_post_meeting_candidate_by_zoom_id_validates_bounded_id(monkeypatch, meeting_id):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")

    with pytest.raises(client.MeetingSchedulerError, match="meeting_id"):
        client.MeetingSchedulerClient().post_meeting_candidate_by_zoom_id(meeting_id)


def test_terminal_zoom_event_candidate_is_exposed_by_cli(monkeypatch, capsys):
    calls = []

    class FakeClient:
        def post_meeting_candidate_for_terminal_zoom_event(self, **kwargs):
            calls.append(kwargs)
            return {"occurrence_key": "occurrence:1"}

    monkeypatch.setattr(cli, "_client", lambda: FakeClient())

    cli.post_meeting_candidate_for_terminal_zoom_event('{"meeting_id":"123"}')

    assert calls == [{"meeting_id": "123"}]
    assert '"occurrence_key": "occurrence:1"' in capsys.readouterr().out


def test_record_post_meeting_processing_records_metadata_without_delivering(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    calls = []
    row = {
        "occurrence_key": "occurrence:1",
        "status": "booked",
        "metadata": {"post_meeting_status": "processing"},
    }

    class Transaction:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *_args):
            return None

    class Connection:
        def transaction(self):
            return Transaction()

        async def fetchrow(self, query, *args):
            calls.append((query, args))
            if query.lstrip().startswith("select"):
                return {
                    "occurrence_key": "occurrence:1",
                    "status": "booked",
                    "metadata": {
                        "post_meeting_attempt": 2,
                        "post_meeting_lease_token": "lease-owner",
                        "post_meeting_lease_until": "2999-01-01T00:00:00Z",
                    },
                }
            return row

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)

    result = scheduler.record_post_meeting_processing(
        "occurrence:1",
        state="pending_transcript",
        event="artifact_poll",
        error="Zoom summary is still processing",
        lease_token="lease-owner",
    )

    patch = json.loads(calls[1][1][1])
    assert result == row
    assert calls[0][1] == ("occurrence:1",)
    assert patch["post_meeting_status"] == "pending_transcript"
    assert patch["post_meeting_event"] == "artifact_poll"
    assert patch["post_meeting_error"] == "Zoom summary is still processing"
    assert patch["post_meeting_attempt"] == 2
    assert patch["post_meeting_attempted_at"].endswith("Z")
    assert patch["post_meeting_updated_at"].endswith("Z")
    assert patch["post_meeting_next_retry_at"] > patch["post_meeting_attempted_at"]
    assert patch["post_meeting_lease_until"] == ""
    assert "set status = 'completed'" not in calls[1][0]
    assert "post_meeting_status = 'delivered'" not in calls[1][0]


def test_record_post_meeting_processing_accepts_explicit_attempt_and_bounds_error(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    calls = []

    class Transaction:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *_args):
            return None

    class Connection:
        def transaction(self):
            return Transaction()

        async def fetchrow(self, query, *args):
            calls.append((query, args))
            return {
                "status": "booked",
                "metadata": {
                    "post_meeting_lease_token": "lease-owner",
                    "post_meeting_lease_until": "2999-01-01T00:00:00Z",
                },
            }

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)

    scheduler.record_post_meeting_processing(
        "occurrence:1",
        state="failed",
        event="artifact_poll",
        error="x" * 3000,
        attempt=7,
        lease_token="lease-owner",
    )

    patch = json.loads(calls[1][1][1])
    assert patch["post_meeting_attempt"] == 7
    assert len(patch["post_meeting_error"]) == 2000


def test_record_post_meeting_processing_rejects_delivery_state(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")

    with pytest.raises(client.MeetingSchedulerError, match="cannot mark"):
        client.MeetingSchedulerClient().record_post_meeting_processing(
            "occurrence:1",
            state="delivered",
            event="delivery_complete",
            lease_token="lease-owner",
        )


def test_record_post_meeting_processing_becomes_terminal_after_retry_budget(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    patches = []

    class Transaction:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *_args):
            return None

    class Connection:
        def transaction(self):
            return Transaction()

        async def fetchrow(self, query, *args):
            if query.lstrip().startswith("select"):
                return {
                    "occurrence_key": "occurrence:1",
                    "status": "booked",
                    "metadata": {
                        "post_meeting_attempt": 12,
                        "post_meeting_lease_token": "lease-owner",
                        "post_meeting_lease_until": "2999-01-01T00:00:00Z",
                    },
                }
            patches.append(json.loads(args[1]))
            return {"occurrence_key": "occurrence:1", "status": "booked", "metadata": patches[-1]}

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)

    scheduler.record_post_meeting_processing(
        "occurrence:1",
        state="pending_transcript",
        event="artifact_poll",
        lease_token="lease-owner",
    )

    assert patches[0]["post_meeting_status"] == "failed_terminal"
    assert patches[0]["post_meeting_next_retry_at"] == ""


def test_claim_post_meeting_processing_atomically_leases_occurrence(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    calls = []

    class Transaction:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *_args):
            return None

    class Connection:
        def transaction(self):
            return Transaction()

        async def fetchrow(self, query, *args):
            calls.append((query, args))
            if query.lstrip().startswith("select"):
                return {
                    "occurrence_key": "occurrence:1",
                    "status": "booked",
                    "metadata": {"post_meeting_attempt": 2},
                }
            return {
                "occurrence_key": "occurrence:1",
                "status": "booked",
                "metadata": json.loads(args[1]),
            }

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)

    result = scheduler.claim_post_meeting_processing(
        "occurrence:1", event="recording.transcript_completed", lease_seconds=600
    )

    assert result["claimed"] is True
    patch = json.loads(calls[1][1][1])
    assert result["lease_token"] == patch["post_meeting_lease_token"]
    assert patch["post_meeting_status"] == "processing"
    assert patch["post_meeting_attempt"] == 3
    assert patch["post_meeting_lease_until"] > patch["post_meeting_attempted_at"]


def test_claim_post_meeting_processing_persists_authenticated_webhook_uuid(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    calls = []

    class Transaction:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *_args):
            return None

    class Connection:
        def transaction(self):
            return Transaction()

        async def fetchrow(self, query, *args):
            calls.append((query, args))
            if query.lstrip().startswith("select"):
                return {
                    "occurrence_key": "occurrence:1",
                    "status": "booked",
                    "metadata": {},
                }
            return {
                "occurrence_key": "occurrence:1",
                "status": "booked",
                "metadata": json.loads(args[1]),
            }

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)

    scheduler.claim_post_meeting_processing(
        "occurrence:1",
        event="recording.completed",
        meeting_uuid="/abc+def==",
    )

    patch = json.loads(calls[1][1][1])
    assert patch["post_meeting_zoom_uuid"] == "/abc+def=="


@pytest.mark.parametrize(
    "active_state",
    ["processing", "summarizing", "publishing_notion", "notifying_participants"],
)
def test_claim_post_meeting_processing_rejects_active_lease(monkeypatch, active_state):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()

    class Transaction:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *_args):
            return None

    class Connection:
        def transaction(self):
            return Transaction()

        async def fetchrow(self, _query, *_args):
            return {
                "occurrence_key": "occurrence:1",
                "status": "booked",
                "metadata": {
                    "post_meeting_status": active_state,
                    "post_meeting_lease_until": "2999-01-01T00:00:00Z",
                },
            }

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)

    result = scheduler.claim_post_meeting_processing("occurrence:1", event="recording.completed")

    assert result == {"claimed": False, "reason": "active_lease"}


def test_claim_post_meeting_processing_resumes_same_durable_owner(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    owner_token = "a" * 64
    calls = []

    class Transaction:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *_args):
            return None

    class Connection:
        def transaction(self):
            return Transaction()

        async def fetchrow(self, query, *args):
            calls.append((query, args))
            return {
                "occurrence_key": "occurrence:1",
                "status": "booked",
                "metadata": {
                    "post_meeting_status": "publishing_notion",
                    "post_meeting_attempt": 3,
                    "post_meeting_lease_token": owner_token,
                    "post_meeting_lease_until": "2999-01-01T00:00:00Z",
                },
            }

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)
    result = scheduler.claim_post_meeting_processing(
        "occurrence:1",
        event="recording.transcript_completed",
        lease_seconds=3600,
        owner_token=owner_token,
    )

    assert result["claimed"] is True
    assert result["resumed"] is True
    assert result["lease_token"] == owner_token
    assert result["attempt"] == 3
    assert result["occurrence"]["metadata"]["post_meeting_status"] == "publishing_notion"
    assert len(calls) == 2


def test_record_post_meeting_processing_rejects_stale_lease_owner(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()

    class Transaction:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *_args):
            return None

    class Connection:
        def transaction(self):
            return Transaction()

        async def fetchrow(self, _query, *_args):
            return {
                "occurrence_key": "occurrence:1",
                "status": "booked",
                "metadata": {"post_meeting_lease_token": "new-owner"},
            }

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)

    with pytest.raises(client.MeetingSchedulerError, match="lease was lost"):
        scheduler.record_post_meeting_processing(
            "occurrence:1",
            state="publishing_notion",
            event="artifact_poll",
            lease_token="stale-owner",
        )


def test_record_post_meeting_processing_requires_live_lease(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()

    with pytest.raises(client.MeetingSchedulerError, match="lease_token is required"):
        scheduler.record_post_meeting_processing(
            "occurrence:1",
            state="processing",
            event="artifact_poll",
            lease_token=None,
        )

    class Transaction:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *_args):
            return None

    class Connection:
        def transaction(self):
            return Transaction()

        async def fetchrow(self, _query, *_args):
            return {
                "occurrence_key": "occurrence:1",
                "status": "booked",
                "metadata": {
                    "post_meeting_lease_token": "lease-owner",
                    "post_meeting_lease_until": "2000-01-01T00:00:00Z",
                },
            }

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)
    with pytest.raises(client.MeetingSchedulerError, match="lease expired"):
        scheduler.record_post_meeting_processing(
            "occurrence:1",
            state="processing",
            event="artifact_poll",
            lease_token="lease-owner",
        )


def test_mark_post_meeting_delivered_requires_lease(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")

    with pytest.raises(client.MeetingSchedulerError, match="lease_token is required"):
        client.MeetingSchedulerClient().mark_post_meeting_delivered(
            "occurrence:1", lease_token=None
        )


def test_mark_post_meeting_delivered_cannot_revive_cancelled_occurrence(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    queries = []

    class Connection:
        async def fetchrow(self, query, *_args):
            queries.append(query)
            return None

    async def with_connection(operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_connection", with_connection)

    with pytest.raises(client.MeetingSchedulerError, match="lease was lost"):
        client.MeetingSchedulerClient().mark_post_meeting_delivered(
            "occurrence:1", lease_token="lease-owner"
        )

    assert "status = 'booked'" in queries[0]


def test_find_availability_uses_freebusy_only_and_returns_slots(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", json.dumps({"default": "organizer@example.com"}))
    calls = []

    class FakeFreebusy:
        def query(self, **kwargs):
            calls.append(kwargs)
            return self

        def execute(self):
            return {
                "calendars": {
                    "organizer@example.com": {"busy": []},
                    "person@example.com": {
                        "busy": [{"start": "2026-08-17T09:00:00Z", "end": "2026-08-17T10:00:00Z"}]
                    },
                }
            }

    class FakeService:
        def freebusy(self):
            return FakeFreebusy()

    monkeypatch.setattr(client, "get_calendar_service", lambda: FakeService())
    result = client.MeetingSchedulerClient().find_availability(
        "default",
        ["person@example.com"],
        "2026-08-17T09:00:00Z",
        "2026-08-17T12:00:00Z",
        30,
    )

    assert calls[0]["body"]["items"] == [
        {"id": "organizer@example.com"},
        {"id": "person@example.com"},
    ]
    assert result["candidates"][0]["start"] == "2026-08-17T10:00:00Z"
    assert "summary" not in calls[0]


def test_email_organizer_requires_a_visible_writable_calendar(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", "{}")
    calls = []

    class FakeCalendarList:
        def list(self, **kwargs):
            calls.append(("list", kwargs))
            return self

        def execute(self):
            return {
                "items": [
                    {"id": "owner@example.com", "accessRole": "writer"},
                ]
            }

    class FakeFreebusy:
        def query(self, **kwargs):
            calls.append(("freebusy", kwargs))
            return self

        def execute(self):
            return {
                "calendars": {
                    "owner@example.com": {"busy": []},
                    "person@example.com": {"busy": []},
                }
            }

    class FakeService:
        def calendarList(self):
            return FakeCalendarList()

        def freebusy(self):
            return FakeFreebusy()

    monkeypatch.setattr(client, "get_calendar_service", lambda: FakeService())
    result = client.MeetingSchedulerClient().find_availability(
        "OWNER@example.com",
        ["person@example.com"],
        "2026-08-17T09:00:00Z",
        "2026-08-17T10:00:00Z",
        30,
    )

    assert result["candidates"]
    assert calls[0][0] == "list"
    assert calls[1][0] == "freebusy"


def test_ad_hoc_booking_requires_confirmation(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", '{"default":"organizer@example.com"}')

    with pytest.raises(client.MeetingSchedulerError, match="explicit confirmation"):
        client.MeetingSchedulerClient().book_meeting(
            "request:1",
            "Planning",
            "2026-08-17T10:00:00Z",
            30,
            "Europe/Prague",
            ["person@example.com"],
            "default",
            mode="ad_hoc",
        )


def test_ad_hoc_booking_rechecks_a_confirmed_slot_for_staleness(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", '{"default":"organizer@example.com"}')

    class FakeFreebusy:
        def query(self, **_kwargs):
            return self

        def execute(self):
            return {
                "calendars": {
                    "organizer@example.com": {
                        "busy": [
                            {
                                "start": "2099-08-17T10:00:00Z",
                                "end": "2099-08-17T10:30:00Z",
                            }
                        ]
                    },
                    "person@example.com": {"busy": []},
                }
            }

    class FakeService:
        def freebusy(self):
            return FakeFreebusy()

    monkeypatch.setattr(client, "get_calendar_service", lambda: FakeService())
    confirmation = client._slot_confirmation_token(
        start=client._parse_rfc3339("2099-08-17T10:00:00Z", field="start"),
        duration=30,
        time_zone="Europe/Prague",
        attendees=["person@example.com"],
        organizer_calendar_key="default",
    )
    with pytest.raises(client.MeetingSchedulerError, match="no longer free"):
        client.MeetingSchedulerClient().book_meeting(
            "request:stale",
            "Planning",
            "2099-08-17T10:00:00Z",
            30,
            "Europe/Prague",
            ["person@example.com"],
            "default",
            mode="ad_hoc",
            confirmation_token=confirmation,
        )


def test_slot_confirmation_binds_explicit_visibility_for_slack_bookings():
    common = {
        "start": client._parse_rfc3339("2099-08-17T10:00:00Z", field="start"),
        "duration": 30,
        "time_zone": "Europe/Prague",
        "attendees": ["person@example.com"],
        "organizer_calendar_key": "default",
    }

    public_token = client._slot_confirmation_token(**common, visibility="public")
    private_token = client._slot_confirmation_token(**common, visibility="private")

    assert public_token != private_token
    assert private_token == (
        "slot-v1:4ad3d989aa689c3eaa9fb2258889ced0b8af9b54e8e22726e12d4dd14396f294"
    )


def test_organizer_alias_is_allowlisted(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", '{"default":"organizer@example.com"}')

    with pytest.raises(client.MeetingSchedulerError, match="not allowlisted"):
        client.MeetingSchedulerClient().find_availability(
            "primary",
            ["person@example.com"],
            "2026-08-17T09:00:00Z",
            "2026-08-17T10:00:00Z",
            30,
        )


def test_find_availability_applies_working_window_in_requested_timezone(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", '{"default":"organizer@example.com"}')

    class FakeFreebusy:
        def query(self, **_kwargs):
            return self

        def execute(self):
            return {
                "calendars": {
                    "organizer@example.com": {"busy": []},
                    "person@example.com": {"busy": []},
                }
            }

    class FakeService:
        def freebusy(self):
            return FakeFreebusy()

    monkeypatch.setattr(client, "get_calendar_service", lambda: FakeService())
    result = client.MeetingSchedulerClient().find_availability(
        "default",
        ["person@example.com"],
        "2026-03-09T15:00:00Z",
        "2026-03-09T19:00:00Z",
        60,
        response_timezone="America/New_York",
    )

    assert result["candidates"][0] == {
        "start": "2026-03-09T11:00:00-04:00",
        "end": "2026-03-09T12:00:00-04:00",
        "timezone": "America/New_York",
        "confirmationToken": client._slot_confirmation_token(
            start=client._parse_rfc3339("2026-03-09T15:00:00Z", field="start"),
            duration=60,
            time_zone="America/New_York",
            attendees=["person@example.com"],
            organizer_calendar_key="default",
        ),
    }


def test_ad_hoc_reschedule_and_cancel_require_confirmation(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", '{"default":"organizer@example.com"}')
    scheduler = client.MeetingSchedulerClient()

    monkeypatch.setattr(
        scheduler,
        "_get_existing",
        lambda _key: None,
    )
    with pytest.raises(client.MeetingSchedulerError, match="explicit confirmation"):
        scheduler.reschedule_meeting("request:1", "2026-08-17T10:00:00Z", 1, "default", mode="ad_hoc")
    with pytest.raises(client.MeetingSchedulerError, match="explicit confirmation"):
        scheduler.cancel_meeting("request:1", "default")
    with pytest.raises(client.MeetingSchedulerError, match="explicit confirmation"):
        scheduler.end_meeting("request:1", "default")


def test_end_meeting_uses_centaur_zoom_owner_and_keeps_calendar_event(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    provider_calls = []
    database_updates = []

    class Connection:
        async def fetchrow(self, _query, _key):
            return {
                "occurrence_key": "request:1",
                "status": "booked",
                "organizer_calendar_key": "default",
                "zoom_meeting_id": "93648882154",
                "calendar_event_id": "event-must-remain",
                "cadence_id": None,
            }

        async def execute(self, query, *args):
            database_updates.append((query, args))

    async def lock(_key, operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_occurrence_lock", lock)
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda method, path, **kwargs: provider_calls.append((method, path, kwargs)) or {},
    )
    monkeypatch.setattr(
        client,
        "get_calendar_service",
        lambda: pytest.fail("ending a live meeting must not delete its Calendar event"),
    )

    result = scheduler.end_meeting(
        "request:1",
        "default",
        client._end_confirmation_token(occurrence_key="request:1", organizer_calendar_key="default"),
    )

    assert result == {
        "status": "ended",
        "occurrenceKey": "request:1",
        "cadence_id": None,
    }
    assert provider_calls == [
        (
            "PUT",
            "/meetings/93648882154/status",
            {"payload": {"action": "end"}, "occurrence_key": "request:1"},
        )
    ]
    assert len(database_updates) == 1
    assert "zoom_ended_by_centaur_at" in database_updates[0][0]


def test_cancel_meeting_locks_row_and_does_not_overwrite_completed(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    queries = []

    class Connection:
        async def fetchrow(self, query, *_args):
            queries.append(query)
            return {
                "occurrence_key": "request:1",
                "status": "completed",
                "organizer_calendar_key": "default",
                "cadence_id": "weekly-sync",
            }

    async def lock(_key, operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_occurrence_lock", lock)
    result = asyncio.run(
        scheduler._cancel_meeting_locked(key="request:1", organizer_calendar_key="default")
    )

    assert result == {
        "status": "completed",
        "occurrenceKey": "request:1",
        "cadence_id": "weekly-sync",
    }
    assert "for update" in queries[0].lower()


def test_reschedule_rejects_a_stale_expected_version(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    state = {
        "status": "booked",
        "version": 3,
        "organizer_calendar_key": "default",
        "actual_start": "2099-08-17T10:00:00+00:00",
        "duration_minutes": 30,
        "time_zone": "Europe/Prague",
        "zoom_meeting_id": "zoom-1",
        "calendar_event_id": "event-1",
        "organizer_calendar_id": "organizer@example.com",
    }

    class Connection:
        async def fetchrow(self, _query, _key):
            return state

    async def lock(_key, operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_occurrence_lock", lock)
    with pytest.raises(client.MeetingSchedulerError, match="version is stale"):
        scheduler.reschedule_meeting(
            "request:1",
            "2099-08-17T11:00:00Z",
            2,
            "default",
            mode="ad_hoc",
            confirmation_token="confirmed",
        )


def test_reschedule_compensates_zoom_when_calendar_update_fails(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", '{"default":"organizer@example.com"}')
    scheduler = client.MeetingSchedulerClient()
    state = {
        "status": "booked",
        "version": 2,
        "organizer_calendar_key": "default",
        "organizer_calendar_id": "organizer@example.com",
        "actual_start": "2099-08-17T10:00:00+00:00",
        "requested_start": "2099-08-17T10:00:00+00:00",
        "duration_minutes": 30,
        "time_zone": "Europe/Prague",
        "attendee_emails": ["person@example.com"],
        "zoom_meeting_id": "zoom-1",
        "zoom_join_url": "https://zoom/j/1",
        "calendar_event_id": "event-1",
    }
    zoom_calls = []

    class Request:
        def __init__(self, value=None, error=None):
            self.value = value
            self.error = error

        def execute(self):
            if self.error:
                raise self.error
            return self.value

    class Freebusy:
        def query(self, **_kwargs):
            return self

        def execute(self):
            return {
                "calendars": {
                    "organizer@example.com": {"busy": []},
                    "person@example.com": {"busy": []},
                }
            }

    class Events:
        def __init__(self):
            self.update_count = 0

        def get(self, **_kwargs):
            return Request({"id": "event-1", "start": {}, "end": {}})

        def update(self, **_kwargs):
            self.update_count += 1
            if self.update_count == 1:
                return Request(error=RuntimeError("calendar update failed"))
            return Request({"id": "event-1", "htmlLink": "https://calendar/event-1"})

    events = Events()

    class FakeService:
        def freebusy(self):
            return Freebusy()

        def events(self):
            return events

    monkeypatch.setattr(client, "get_calendar_service", lambda: FakeService())

    def zoom_request(method, path, **kwargs):
        zoom_calls.append((method, path, kwargs))
        return {}

    monkeypatch.setattr(scheduler, "_zoom_request", zoom_request)

    class Connection:
        async def fetchrow(self, query, *_args):
            if "select *" in query:
                return state
            raise AssertionError(query)

        async def execute(self, *_args):
            return None

    async def lock(_key, operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_occurrence_lock", lock)

    with pytest.raises(client.MeetingSchedulerError, match="provider rescheduling failed"):
        scheduler.reschedule_meeting(
            "occurrence:1",
            "2099-08-17T11:00:00Z",
            2,
            "default",
        )

    assert [call[1] for call in zoom_calls] == ["/meetings/zoom-1", "/meetings/zoom-1"]
    assert zoom_calls[0][2]["payload"]["start_time"] == "2099-08-17T11:00:00Z"
    assert zoom_calls[1][2]["payload"]["start_time"] == "2099-08-17T10:00:00Z"
    assert events.update_count == 2


def test_ad_hoc_retry_rejects_parameter_drift(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", '{"default":"organizer@example.com"}')
    scheduler = client.MeetingSchedulerClient()
    state = {
        "status": "pending",
        "title": "Original",
        "requested_start": "2099-08-17T10:00:00+00:00",
        "duration_minutes": 30,
        "time_zone": "Europe/Prague",
        "organizer_calendar_key": "default",
        "organizer_calendar_id": "organizer@example.com",
        "attendee_emails": ["person@example.com"],
        "calendar_event_id": "",
        "zoom_meeting_id": "",
    }

    class Freebusy:
        def query(self, **_kwargs):
            return self

        def execute(self):
            return {
                "calendars": {
                    "organizer@example.com": {"busy": []},
                    "person@example.com": {"busy": []},
                }
            }

    class FakeService:
        def freebusy(self):
            return Freebusy()

    monkeypatch.setattr(client, "get_calendar_service", lambda: FakeService())

    class Connection:
        async def execute(self, *_args):
            return None

        async def fetchrow(self, query, *_args):
            if "returning occurrence_key" in query:
                return None
            return state

    async def lock(_key, operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_occurrence_lock", lock)
    confirmation = client._slot_confirmation_token(
        start=client._parse_rfc3339("2099-08-17T10:00:00Z", field="start"),
        duration=30,
        time_zone="Europe/Prague",
        attendees=["person@example.com"],
        organizer_calendar_key="default",
    )

    with pytest.raises(client.MeetingSchedulerError, match="parameters cannot be changed"):
        scheduler.book_meeting(
            "request:1",
            "Changed",
            "2099-08-17T10:00:00Z",
            30,
            "Europe/Prague",
            ["person@example.com"],
            "default",
            mode="ad_hoc",
            confirmation_token=confirmation,
        )


def test_cadence_reconciliation_allows_provider_bound_parameter_updates(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    state = {
        "status": "booked",
        "title": "Original",
        "requested_start": "2099-08-17T10:00:00+00:00",
        "duration_minutes": 30,
        "time_zone": "Europe/Prague",
        "organizer_calendar_key": "default",
        "organizer_calendar_id": "organizer@example.com",
        "attendee_emails": ["person@example.com"],
        "calendar_event_id": "event-1",
        "zoom_meeting_id": "zoom-1",
    }

    class Connection:
        async def fetchrow(self, query, *_args):
            if "returning occurrence_key" in query:
                return None
            return state

        async def execute(self, *_args):
            raise AssertionError("a booked cadence should reconcile before rewriting state")

    async def claim():
        return await scheduler._claim_occurrence_row(
            Connection(),
            key="cadence:1",
            cadence_id="cadence-1",
            request_id="cadence:1",
            title="Updated",
            requested_start=client._parse_rfc3339("2099-08-17T10:00:00Z", field="requested_start"),
            duration=45,
            time_zone="Europe/Prague",
            organizer_key="default",
            organizer_id="organizer@example.com",
            attendees=["person@example.com", "new@example.com"],
            allow_parameter_update=True,
        )

    current, inserted = asyncio.run(claim())
    assert current == state
    assert inserted is False


def test_freebusy_errors_fail_closed_without_exposing_event_details(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", '{"default":"organizer@example.com"}')

    class FakeFreebusy:
        def query(self, **_kwargs):
            return self

        def execute(self):
            return {
                "calendars": {
                    "organizer@example.com": {"busy": []},
                    "person@example.com": {
                        "errors": [{"reason": "notFound"}],
                        "busy": [],
                    },
                }
            }

    class FakeService:
        def freebusy(self):
            return FakeFreebusy()

    monkeypatch.setattr(client, "get_calendar_service", lambda: FakeService())
    with pytest.raises(client.MeetingSchedulerError, match="free/busy access"):
        client.MeetingSchedulerClient().find_availability(
            "default",
            ["person@example.com"],
            "2026-08-17T09:00:00Z",
            "2026-08-17T10:00:00Z",
            30,
        )


def test_scheduler_lock_is_transaction_scoped(monkeypatch):
    calls = []

    class Transaction:
        async def __aenter__(self):
            calls.append("begin")

        async def __aexit__(self, *_args):
            calls.append("end")

    class Connection:
        def transaction(self):
            return Transaction()

        async def execute(self, query, *_args):
            calls.append(query)

        async def close(self):
            calls.append("close")

    connection = Connection()
    monkeypatch.setattr(client, "_database_url", lambda: "postgresql://scheduler")
    monkeypatch.setattr(client.asyncpg, "connect", lambda *_args, **_kwargs: _await(connection))

    async def operation(_connection):
        calls.append("operation")
        return "ok"

    async def run():
        return await client._with_occurrence_lock("cadence:2026-08-17", operation)

    async def _await(value):
        return value

    assert asyncio.run(run()) == "ok"
    assert calls == [
        "begin",
        "select pg_advisory_xact_lock(hashtextextended($1, 0))",
        "operation",
        "end",
        "close",
    ]


def test_book_meeting_reuses_deterministic_calendar_id_after_partial_insert(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", '{"default":"organizer@example.com"}')
    monkeypatch.setenv("MEETING_ZOOM_HOST_USER_ID", "host-1")
    scheduler = client.MeetingSchedulerClient()
    state = {
        "status": "pending",
        "title": "Planning",
        "duration_minutes": 30,
        "organizer_calendar_key": "default",
        "attendee_emails": ["person@example.com"],
        "zoom_meeting_id": "",
        "zoom_join_url": "",
        "calendar_event_id": "",
    }

    class FakeConnection:
        def transaction(self):
            raise AssertionError("test injects the lock boundary")

        async def fetchrow(self, query, *args):
            if "returning occurrence_key" in query:
                return {"occurrence_key": args[0]}
            if "set status = 'booked'" in query:
                return {**state, "status": "booked"}
            return state

        async def execute(self, *_args):
            return None

    class Request:
        def __init__(self, value=None, error=None):
            self.value = value
            self.error = error

        def execute(self):
            if self.error:
                raise self.error
            return self.value

    class Events:
        def __init__(self):
            self.insert_calls = []

        def insert(self, **kwargs):
            self.insert_calls.append(kwargs)
            return Request(error=RuntimeError("insert response lost"))

        def get(self, **_kwargs):
            return Request(
                value={
                    "id": client.MeetingSchedulerClient._calendar_event_id("cadence:1"),
                    "htmlLink": "https://calendar/event",
                }
            )

    events = Events()

    class FakeService:
        def events(self):
            return events

    monkeypatch.setattr(client, "get_calendar_service", lambda: FakeService())
    monkeypatch.setattr(
        scheduler,
        "_zoom_create",
        lambda **_kwargs: {"id": "zoom-1", "join_url": "https://zoom/j/1"},
    )
    monkeypatch.setattr(scheduler, "_zoom_find_by_occurrence", lambda _key: None)

    async def lock(_key, operation):
        return await operation(FakeConnection())

    monkeypatch.setattr(client, "_with_occurrence_lock", lock)
    result = scheduler.book_meeting(
        "cadence:1",
        "Planning",
        "2099-08-17T10:00:00Z",
        30,
        "Europe/Prague",
        ["person@example.com"],
        "default",
    )

    assert result["status"] == "booked"
    assert result["zoomJoinUrl"] == "https://zoom/j/1"
    assert "id" not in events.insert_calls[0]
    event_body = events.insert_calls[0]["body"]
    assert event_body["description"] == "Zoom: https://zoom/j/1"
    assert event_body["guestsCanModify"] is True
    assert event_body["guestsCanInviteOthers"] is True
    event_id = event_body["id"]
    assert event_id == client.MeetingSchedulerClient._calendar_event_id("cadence:1")
    assert "_" not in event_id


def test_reconcile_recovers_missing_zoom_join_url_before_marking_booked(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    state = {
        "status": "blocked",
        "organizer_calendar_id": "organizer@example.com",
        "calendar_event_id": "event-1",
        "calendar_html_link": "https://calendar/event-1",
        "zoom_meeting_id": "zoom-1",
        "zoom_join_url": "",
        "requested_start": "2026-08-17T10:00:00+00:00",
        "actual_start": None,
        "time_zone": "Europe/Prague",
        "version": 1,
    }
    updates = []
    marked = {}

    async def existing(_key):
        return dict(state)

    async def update(_key, **values):
        updates.append(values)
        return None

    async def mark(_key, **values):
        marked.update(values)
        return {
            **state,
            "status": "booked",
            "actual_start": values["actual_start"].isoformat(),
            "zoom_join_url": values["join_url"],
        }

    class Request:
        def execute(self):
            return {"id": "event-1"}

    class Events:
        def get(self, **_kwargs):
            return Request()

    class FakeService:
        def events(self):
            return Events()

    monkeypatch.setattr(scheduler, "_get_existing", existing)
    monkeypatch.setattr(scheduler, "_update_provider_state", update)
    monkeypatch.setattr(scheduler, "_mark_booked", mark)
    monkeypatch.setattr(client, "get_calendar_service", lambda: FakeService())

    class Connection:
        async def execute(self, *_args):
            return None

    async def lock(_key, operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_occurrence_lock", lock)

    def zoom_request(method, path, **_kwargs):
        if method == "GET" and path == "/meetings/zoom-1":
            return {"id": "zoom-1", "join_url": "https://zoom/j/1"}
        raise AssertionError((method, path))

    monkeypatch.setattr(scheduler, "_zoom_request", zoom_request)
    result = scheduler.get_or_reconcile_meeting("cadence:1")

    assert result["status"] == "booked"
    assert result["providerState"] == {"calendarPresent": True, "zoomPresent": True}
    assert marked["join_url"] == "https://zoom/j/1"
    assert updates == [{"join_url": "https://zoom/j/1"}]


def test_reconcile_marks_lost_provider_pair_retryable(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    state = {
        "status": "booked",
        "organizer_calendar_id": "organizer@example.com",
        "calendar_event_id": "event-1",
        "zoom_meeting_id": "zoom-1",
        "zoom_join_url": "https://zoom/j/1",
        "requested_start": "2099-08-17T10:00:00+00:00",
        "actual_start": "2099-08-17T10:00:00+00:00",
    }
    errors = []

    async def existing(_key):
        return dict(state)

    async def mark_error(_key, message):
        errors.append(message)

    class Request:
        def execute(self):
            raise RuntimeError("calendar event disappeared")

    class Events:
        def get(self, **_kwargs):
            return Request()

    class FakeService:
        def events(self):
            return Events()

    monkeypatch.setattr(scheduler, "_get_existing", existing)
    monkeypatch.setattr(scheduler, "_mark_error", mark_error)
    monkeypatch.setattr(client, "get_calendar_service", lambda: FakeService())

    async def lock(_key, operation):
        return await operation(object())

    monkeypatch.setattr(client, "_with_occurrence_lock", lock)
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(RuntimeError("zoom missing")),
    )

    result = scheduler.get_or_reconcile_meeting("cadence:2026-08-17")

    assert result["status"] == "blocked"
    assert result["providerState"] == {"calendarPresent": False, "zoomPresent": False}
    assert errors == ["provider state is incomplete and requires reconciliation"]


def test_booked_cadence_reconciles_existing_providers_and_preserves_attendee_removals(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", '{"default":"organizer@example.com"}')
    scheduler = client.MeetingSchedulerClient()
    state = {
        "status": "booked",
        "cadence_id": "weekly-sync",
        "request_id": "weekly-sync:2026-08-24",
        "title": "Old title",
        "duration_minutes": 30,
        "organizer_calendar_key": "default",
        "organizer_calendar_id": "organizer@example.com",
        "attendee_emails": ["removed@example.com"],
        "actual_start": "2099-08-24T08:00:00+00:00",
        "requested_start": "2099-08-24T08:00:00+00:00",
        "time_zone": "Europe/Prague",
        "calendar_event_id": "event-1",
        "calendar_html_link": "https://calendar/event-1",
        "zoom_meeting_id": "zoom-1",
        "zoom_join_url": "https://zoom/j/1",
        "version": 2,
    }
    zoom_calls = []
    calendar_updates = []

    monkeypatch.setattr(
        scheduler,
        "_claim_occurrence_row",
        lambda *_args, **_kwargs: _async_value((state, False)),
    )
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda method, path, **kwargs: zoom_calls.append((method, path, kwargs)) or {},
    )

    class Request:
        def __init__(self, value):
            self.value = value

        def execute(self):
            return self.value

    class Events:
        def get(self, **_kwargs):
            return Request({"id": "event-1"})

        def update(self, **kwargs):
            calendar_updates.append(kwargs)
            return Request({"id": "event-1", "htmlLink": "https://calendar/event-1"})

    class FakeService:
        def events(self):
            return Events()

    monkeypatch.setattr(client, "get_calendar_service", lambda: FakeService())
    reconciled = {}

    async def mark(_connection, **kwargs):
        reconciled.update(kwargs)
        return {**state, "title": kwargs["title"], "duration_minutes": kwargs["duration"]}

    monkeypatch.setattr(scheduler, "_reconcile_booked_row", mark)

    class Connection:
        async def execute(self, *_args):
            return None

    async def lock(_key, operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_occurrence_lock", lock)
    result = scheduler.book_meeting(
        "weekly-sync:2026-08-24",
        "New title",
        "2099-08-24T09:00:00Z",
        45,
        "Europe/Prague",
        ["new@example.com"],
        "default",
        cadence_id="weekly-sync",
        request_id="weekly-sync:2026-08-24",
    )

    assert result["reconciled"] is True
    assert zoom_calls[0][0:2] == ("PATCH", "/meetings/zoom-1")
    assert zoom_calls[0][2]["payload"] == {
        "topic": "New title",
        "start_time": "2099-08-24T09:00:00Z",
        "timezone": "Europe/Prague",
        "duration": 45,
    }
    assert calendar_updates[0]["sendUpdates"] == "all"
    assert [item["email"] for item in calendar_updates[0]["body"]["attendees"]] == [
        "removed@example.com",
        "new@example.com",
    ]
    assert reconciled["attendees"] == ["removed@example.com", "new@example.com"]


def test_cadence_retry_preserves_manual_reschedule_for_same_anchor(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ORGANIZER_CALENDARS", '{"default":"organizer@example.com"}')
    scheduler = client.MeetingSchedulerClient()
    state = {
        "status": "booked",
        "cadence_id": "weekly-sync",
        "request_id": "weekly-sync:2026-08-24",
        "title": "Weekly sync",
        "duration_minutes": 30,
        "organizer_calendar_key": "default",
        "organizer_calendar_id": "organizer@example.com",
        "attendee_emails": ["person@example.com"],
        "requested_start": "2099-08-24T10:00:00+00:00",
        "actual_start": "2099-08-24T11:00:00+00:00",
        "time_zone": "Europe/Prague",
        "calendar_event_id": "event-1",
        "zoom_meeting_id": "zoom-1",
        "zoom_join_url": "https://zoom/j/1",
        "version": 2,
    }
    provider_calls = []

    async def claim(*_args, **_kwargs):
        return state, False

    monkeypatch.setattr(scheduler, "_claim_occurrence_row", claim)
    monkeypatch.setattr(
        scheduler,
        "_zoom_request",
        lambda *args, **kwargs: provider_calls.append((args, kwargs)) or {},
    )

    class Connection:
        async def execute(self, *_args):
            return None

    async def lock(_key, operation):
        return await operation(Connection())

    monkeypatch.setattr(client, "_with_occurrence_lock", lock)
    result = scheduler.book_meeting(
        "weekly-sync:2026-08-24",
        "Weekly sync",
        "2099-08-24T10:00:00Z",
        30,
        "Europe/Prague",
        ["person@example.com"],
        "default",
        cadence_id="weekly-sync",
        request_id="weekly-sync:2026-08-24",
    )

    assert result["actualStart"] == "2099-08-24T11:00:00Z"
    assert result["zoomJoinUrl"] == "https://zoom/j/1"
    assert provider_calls == []


def test_public_scheduler_methods_have_explicit_tool_signatures():
    assert "**kwargs" not in str(inspect.signature(client.book_meeting))
    assert "**kwargs" not in str(inspect.signature(client.reschedule_meeting))
    assert "**kwargs" not in str(inspect.signature(client.cancel_meeting))
    assert "**kwargs" not in str(inspect.signature(client.end_meeting))


def test_public_book_meeting_forwards_alternative_host_email(monkeypatch):
    calls = []

    class FakeClient:
        def book_meeting(self, *args, **kwargs):
            calls.append((args, kwargs))
            return {"status": "booked"}

    monkeypatch.setattr(client, "_client", lambda: FakeClient())

    result = client.book_meeting(
        occurrence_key="request:1",
        title="Planning",
        start="2099-08-17T10:00:00Z",
        duration_minutes=30,
        time_zone="Europe/Prague",
        attendee_emails=["proposer@example.com"],
        organizer_calendar_key="default",
        mode="ad_hoc",
        confirmation_token="confirmed",
        alternative_host_email="proposer@example.com",
        visibility="private",
    )

    assert result == {"status": "booked"}
    assert "alternative_host_email" in inspect.signature(client.book_meeting).parameters
    assert calls[0][1]["alternative_host_email"] == "proposer@example.com"
    assert calls[0][1]["visibility"] == "private"


def _zoom_transport(monkeypatch, status_code, *, json_body=None, text=None, headers=None):
    """Route _zoom_request through a real httpx client with a canned response."""

    def handler(request):
        if json_body is not None:
            return httpx.Response(status_code, json=json_body, headers=headers)
        return httpx.Response(status_code, text=text or "", headers=headers)

    transport = httpx.MockTransport(handler)
    real_client = httpx.Client
    monkeypatch.setattr(
        client.httpx,
        "Client",
        lambda *args, **kwargs: real_client(*args, transport=transport, **kwargs),
    )


def test_zoom_request_error_keeps_bounded_code_and_message(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    _zoom_transport(
        monkeypatch,
        400,
        json_body={"code": 300, "message": "Invalid tracking field: centaur_occurrence_key."},
    )

    with pytest.raises(client.MeetingSchedulerError) as raised:
        scheduler._zoom_request("POST", "/users/me/meetings", payload={"topic": "x"})

    message = str(raised.value)
    assert message.startswith("Zoom request failed with HTTP 400")
    assert "zoom code 300" in message
    assert "Invalid tracking field: centaur_occurrence_key." in message


def test_zoom_request_error_includes_field_level_validation_errors(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    _zoom_transport(
        monkeypatch,
        400,
        json_body={
            "code": 300,
            "message": "Validation Failed.",
            "errors": [
                {"field": "settings.auto_recording", "message": "Invalid field."},
                {"field": "tracking_fields", "message": "Tracking field is not configured."},
            ],
        },
    )

    with pytest.raises(client.MeetingSchedulerError) as raised:
        scheduler._zoom_request("POST", "/users/me/meetings", payload={})

    message = str(raised.value)
    assert "settings.auto_recording: Invalid field." in message
    assert "tracking_fields: Tracking field is not configured." in message


def test_zoom_request_error_redacts_urls_tokens_emails_and_control_chars(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    secret_token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJvcmJpZSJ9.abcdefghijklmnopqrstuvwxyz0123456789"
    _zoom_transport(
        monkeypatch,
        401,
        json_body={
            "code": 124,
            "message": (
                "Invalid access token Bearer "
                + secret_token
                + " for host@example.com\r\nsee https://api.zoom.us/v2/users/me?token=abc"
            ),
        },
        headers={"x-zm-trackingid": "trace-secret", "www-authenticate": "Bearer realm=x"},
    )

    with pytest.raises(client.MeetingSchedulerError) as raised:
        scheduler._zoom_request("GET", "/users/me/meetings")

    message = str(raised.value)
    assert message.startswith("Zoom request failed with HTTP 401")
    assert "zoom code 124" in message
    assert "Invalid access token" in message
    assert secret_token not in message
    assert "eyJ" not in message
    assert "host@example.com" not in message
    assert "api.zoom.us" not in message
    assert "token=abc" not in message
    assert "trace-secret" not in message
    assert "realm=" not in message
    assert not any(ord(char) < 32 for char in message)


def test_zoom_request_error_bounds_message_length(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    _zoom_transport(
        monkeypatch,
        400,
        json_body={
            "code": 300,
            "message": "x" * 5000,
            "errors": [{"field": "f" * 500, "message": "m" * 500} for _ in range(50)],
        },
    )

    with pytest.raises(client.MeetingSchedulerError) as raised:
        scheduler._zoom_request("POST", "/users/me/meetings", payload={})

    assert len(str(raised.value)) <= client.MAX_ZOOM_ERROR_DETAIL_LENGTH + 64


@pytest.mark.parametrize(
    "kwargs",
    [
        {"text": "<html><body>Bad Gateway from https://api.zoom.us/secret</body></html>"},
        {"json_body": ["not", "a", "dict"]},
        {"json_body": {"unexpected": "shape", "token": "eyJabc"}},
        {"text": ""},
    ],
)
def test_zoom_request_error_without_recognized_body_leaks_nothing(monkeypatch, kwargs):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    _zoom_transport(monkeypatch, 400, **kwargs)

    with pytest.raises(client.MeetingSchedulerError) as raised:
        scheduler._zoom_request("POST", "/users/me/meetings", payload={})

    assert str(raised.value) == "Zoom request failed with HTTP 400"


def test_zoom_request_delete_404_still_returns_empty(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    scheduler = client.MeetingSchedulerClient()
    _zoom_transport(
        monkeypatch, 404, json_body={"code": 3001, "message": "Meeting does not exist."}
    )

    assert scheduler._zoom_request("DELETE", "/meetings/1") == {}


def test_booking_persists_sanitized_zoom_reason_in_last_error(monkeypatch):
    monkeypatch.setenv("MEETING_SCHEDULER_ENABLED", "true")
    monkeypatch.setenv("MEETING_ZOOM_HOST_USER_ID", "centaur@example.com")
    scheduler = client.MeetingSchedulerClient()
    executed = []

    class Connection:
        async def execute(self, query, *args):
            executed.append((query, args))

    async def claim(_connection, **_kwargs):
        return {"status": "pending", "organizer_calendar_key": "default"}, True

    async def lock(_key, operation):
        return await operation(Connection())

    monkeypatch.setattr(scheduler, "_claim_occurrence_row", claim)
    monkeypatch.setattr(scheduler, "_zoom_find_by_occurrence", lambda _key: None)
    monkeypatch.setattr(client, "_with_occurrence_lock", lock)
    monkeypatch.setattr(
        client, "get_calendar_service", lambda: pytest.fail("calendar must not be touched")
    )
    _zoom_transport(
        monkeypatch,
        400,
        json_body={"code": 300, "message": "Invalid tracking field: see https://zoom.us/x"},
    )

    result = asyncio.run(
        scheduler._book_meeting_locked(
            key="request:1",
            cadence_id=None,
            request_id="request:1",
            title="Planning",
            start_at=client._parse_rfc3339("2099-08-17T10:00:00Z", field="start"),
            duration=30,
            time_zone="UTC",
            organizer_calendar_key="default",
            organizer_id="organizer@example.com",
            attendees=["person@example.com"],
            allow_parameter_update=False,
            check_slot_free=False,
        )
    )

    assert isinstance(result, client._OperationFailure)
    blocked = [item for item in executed if "status = 'blocked'" in item[0]]
    assert len(blocked) == 1
    stored_error = blocked[0][1][1]
    assert stored_error.startswith("Zoom request failed with HTTP 400")
    assert "zoom code 300" in stored_error
    assert "Invalid tracking field" in stored_error
    assert "zoom.us" not in stored_error
