"""CLI for Centaur meeting scheduling."""

from __future__ import annotations

import json
from typing import Any

import typer

from .client import _client

app = typer.Typer(name="meeting-scheduler", help="Centaur Calendar and Zoom scheduling")


def _print(value: Any) -> None:
    print(json.dumps(value, indent=2, ensure_ascii=False, default=str))


@app.command("availability")
def availability(payload: str = typer.Argument(..., help="JSON availability request")) -> None:
    _print(_client().find_availability(**json.loads(payload)))


@app.command("book")
def book(payload: str = typer.Argument(..., help="JSON booking request")) -> None:
    _print(_client().book_meeting(**json.loads(payload)))


@app.command("reschedule")
def reschedule(payload: str = typer.Argument(..., help="JSON reschedule request")) -> None:
    _print(_client().reschedule_meeting(**json.loads(payload)))


@app.command("cancel")
def cancel(payload: str = typer.Argument(..., help="JSON cancellation request")) -> None:
    _print(_client().cancel_meeting(**json.loads(payload)))


@app.command("end")
def end(payload: str = typer.Argument(..., help="JSON live meeting end request")) -> None:
    _print(_client().end_meeting(**json.loads(payload)))


@app.command("get")
def get(payload: str = typer.Argument(..., help="JSON reconciliation request")) -> None:
    _print(_client().get_or_reconcile_meeting(**json.loads(payload)))


@app.command("recording")
def recording(payload: str = typer.Argument(..., help="JSON recording request")) -> None:
    _print(_client().get_recording(**json.loads(payload)))


@app.command("summary")
def summary(payload: str = typer.Argument(..., help="JSON meeting summary request")) -> None:
    _print(_client().get_summary(**json.loads(payload)))


@app.command("post-meeting-candidates")
def post_meeting_candidates(
    payload: str = typer.Argument(..., help="JSON candidate request"),
) -> None:
    _print(_client().post_meeting_candidates(**json.loads(payload)))


@app.command("post-meeting-candidate-by-zoom-id")
def post_meeting_candidate_by_zoom_id(
    payload: str = typer.Argument(..., help="JSON Zoom meeting ID request"),
) -> None:
    _print(_client().post_meeting_candidate_by_zoom_id(**json.loads(payload)))


@app.command("post-meeting-candidate-for-terminal-zoom-event")
def post_meeting_candidate_for_terminal_zoom_event(
    payload: str = typer.Argument(..., help="Authenticated terminal Zoom event request"),
) -> None:
    _print(
        _client().post_meeting_candidate_for_terminal_zoom_event(**json.loads(payload))
    )


@app.command("collect-post-meeting-artifacts")
def collect_post_meeting_artifacts(
    payload: str = typer.Argument(..., help="JSON artifact request"),
) -> None:
    _print(_client().collect_post_meeting_artifacts(**json.loads(payload)))


@app.command("record-post-meeting-processing")
def record_post_meeting_processing(
    payload: str = typer.Argument(..., help="JSON processing state request"),
) -> None:
    _print(_client().record_post_meeting_processing(**json.loads(payload)))


@app.command("claim-post-meeting-processing")
def claim_post_meeting_processing(
    payload: str = typer.Argument(..., help="JSON processing lease request"),
) -> None:
    _print(_client().claim_post_meeting_processing(**json.loads(payload)))


@app.command("mark-post-meeting-delivered")
def mark_post_meeting_delivered(
    payload: str = typer.Argument(..., help="JSON delivery marker"),
) -> None:
    _print(_client().mark_post_meeting_delivered(**json.loads(payload)))
