from datetime import datetime, timedelta, timezone

from centaur_tool_interview_prep.client import InterviewPrepClient


def test_admin_requester_is_authorized():
    client = InterviewPrepClient(ashby_api_key="x", slack_bot_token="")
    client._users = lambda limit=500: [  # type: ignore[method-assign]
        {"email": "admin@paradigm.xyz", "globalRole": "Organization Admin"}
    ]

    decision = client._authorize(
        "admin@paradigm.xyz",
        {"hiringTeam": []},
        [],
        [],
    )

    assert decision.granted is True
    assert "admin" in decision.reason.lower()


def test_unrelated_requester_is_denied():
    client = InterviewPrepClient(ashby_api_key="x", slack_bot_token="")
    client._users = lambda limit=500: [  # type: ignore[method-assign]
        {"email": "user@paradigm.xyz", "globalRole": "Limited Team Member"}
    ]

    decision = client._authorize(
        "user@paradigm.xyz",
        {"hiringTeam": []},
        [],
        [],
    )

    assert decision.granted is False
    assert "not an ashby admin" in decision.reason.lower()


def test_hiring_team_requester_is_authorized():
    client = InterviewPrepClient(ashby_api_key="x", slack_bot_token="")
    client._users = lambda limit=500: [  # type: ignore[method-assign]
        {"email": "interviewer@paradigm.xyz", "globalRole": "Limited Team Member"}
    ]

    decision = client._authorize(
        "interviewer@paradigm.xyz",
        {"hiringTeam": [{"email": "interviewer@paradigm.xyz"}]},
        [],
        [],
    )

    assert decision.granted is True
    assert "hiring team" in decision.reason.lower()


def test_event_format_includes_duration_and_zoom():
    client = InterviewPrepClient(ashby_api_key="x", slack_bot_token="")
    start = datetime.now(timezone.utc).replace(microsecond=0)
    end = start + timedelta(minutes=30)

    formatted = client._event_format(
        {
            "summary": "Interview with Candidate",
            "start": start.isoformat(),
            "end": end.isoformat(),
            "location": "https://paradigmxyz.zoom.us/j/123",
        }
    )

    assert formatted["duration_minutes"] == 30
    assert formatted["medium"] == "Zoom"
    assert formatted["format"] == "30 minute Zoom interview"
