"""Permission-gated interview prep briefs from Ashby and Google Calendar."""

from __future__ import annotations

import os
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Any
from urllib.parse import urlsplit

import httplib2
import httpx
import socks
from centaur_sdk import secret
from googleapiclient.discovery import build

ASHBY_BASE_URL = "https://api.ashbyhq.com"
DEFAULT_INTERVIEWS_CALENDAR_ID = "c_5d7gf9ut9magpm8vta36608i40@group.calendar.google.com"

try:
    from api.integrations.gsuite.http import build_http as _shared_build_http
except ModuleNotFoundError:
    _shared_build_http = None


def _build_google_http() -> httplib2.Http:
    if _shared_build_http is not None:
        return _shared_build_http()

    proxy_url = os.environ.get("HTTPS_PROXY") or os.environ.get("https_proxy")
    proxy_info = None
    if proxy_url:
        parts = urlsplit(proxy_url)
        proxy_info = httplib2.ProxyInfo(
            proxy_type=socks.PROXY_TYPE_HTTP,
            proxy_host=parts.hostname,
            proxy_port=parts.port or 8080,
        )
    ca_certs = os.environ.get("SSL_CERT_FILE") or os.environ.get("REQUESTS_CA_BUNDLE")
    return httplib2.Http(proxy_info=proxy_info, ca_certs=ca_certs)


def _calendar_service():
    return build("calendar", "v3", http=_build_google_http())


def _parse_dt(value: str) -> datetime | None:
    if not value:
        return None
    normalized = value.replace("Z", "+00:00")
    try:
        parsed = datetime.fromisoformat(normalized)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=timezone.utc)
    return parsed


def _full_name(user: dict[str, Any] | None) -> str:
    if not user:
        return ""
    name = f"{user.get('firstName', '')} {user.get('lastName', '')}".strip()
    return name or user.get("name", "") or user.get("email", "")


def _lower(value: str | None) -> str:
    return (value or "").strip().lower()


@dataclass
class AccessDecision:
    granted: bool
    reason: str
    requester_email: str | None = None
    requester_ashby_user: dict[str, Any] | None = None


class InterviewPrepClient:
    """Build interview prep briefs with requester-level authorization."""

    def __init__(
        self,
        ashby_api_key: str | None = None,
        slack_bot_token: str | None = None,
        calendar_id: str | None = None,
        timeout: float = 30.0,
    ):
        self.ashby_api_key = ashby_api_key or secret("ASHBY_API_KEY", "")
        self.slack_bot_token = slack_bot_token or secret("SLACK_BOT_TOKEN", "")
        self.calendar_id = (
            calendar_id
            or os.environ.get("INTERVIEW_PREP_CALENDAR_ID", "").strip()
            or DEFAULT_INTERVIEWS_CALENDAR_ID
        )
        self.timeout = timeout
        self._http = httpx.Client(timeout=timeout)

    def _ashby_request(self, endpoint: str, data: dict[str, Any] | None = None) -> dict[str, Any]:
        if not self.ashby_api_key:
            raise RuntimeError("ASHBY_API_KEY not set")
        response = self._http.post(
            f"{ASHBY_BASE_URL}/{endpoint}",
            json=data or {},
            auth=(self.ashby_api_key, ""),
            headers={"Accept": "application/json; version=1", "Content-Type": "application/json"},
        )
        if response.status_code == 401:
            raise RuntimeError("Ashby API key is missing or invalid")
        if response.status_code == 403:
            raise RuntimeError("Ashby API key lacks required permissions")
        result = response.json()
        if not result.get("success", True):
            errors = result.get("errors", [])
            messages = [
                e.get("message", str(e)) if isinstance(e, dict) else str(e)
                for e in errors
            ]
            raise RuntimeError(f"Ashby API error: {'; '.join(messages)}")
        return result

    def _ashby_paginate(
        self, endpoint: str, data: dict[str, Any] | None = None, limit: int = 100
    ) -> list[dict[str, Any]]:
        payload = dict(data or {})
        payload["limit"] = min(limit, 100)
        results: list[dict[str, Any]] = []
        cursor = None
        while len(results) < limit:
            request = dict(payload)
            if cursor:
                request["cursor"] = cursor
            page = self._ashby_request(endpoint, request)
            results.extend(page.get("results", []))
            if not page.get("moreDataAvailable"):
                break
            cursor = page.get("nextCursor")
            if not cursor:
                break
        return results[:limit]

    def _slack_user_email(self, slack_user_id: str) -> str | None:
        if not self.slack_bot_token:
            return None
        response = self._http.get(
            "https://slack.com/api/users.info",
            params={"user": slack_user_id},
            headers={"Authorization": f"Bearer {self.slack_bot_token}"},
        )
        data = response.json()
        if not data.get("ok"):
            return None
        return data.get("user", {}).get("profile", {}).get("email")

    def _calendar_events(
        self,
        query: str,
        start: datetime,
        end: datetime,
        max_results: int = 50,
    ) -> list[dict[str, Any]]:
        service = _calendar_service()
        result = (
            service.events()
            .list(
                calendarId=self.calendar_id,
                q=query,
                timeMin=start.isoformat(),
                timeMax=end.isoformat(),
                maxResults=max_results,
                singleEvents=True,
                orderBy="startTime",
            )
            .execute()
        )
        events = []
        for event in result.get("items", []):
            events.append(
                {
                    "id": event.get("id", ""),
                    "summary": event.get("summary", ""),
                    "start": event.get("start", {}).get("dateTime")
                    or event.get("start", {}).get("date", ""),
                    "end": event.get("end", {}).get("dateTime")
                    or event.get("end", {}).get("date", ""),
                    "location": event.get("location", ""),
                    "description": event.get("description", ""),
                    "attendees": [a.get("email", "") for a in event.get("attendees", [])],
                    "html_link": event.get("htmlLink", ""),
                }
            )
        return events

    def _candidate_search(self, name: str) -> list[dict[str, Any]]:
        return self._ashby_request("candidate.search", {"name": name}).get("results", [])

    def _candidate(self, candidate_id: str) -> dict[str, Any] | None:
        return self._ashby_request("candidate.info", {"id": candidate_id}).get("results")

    def _application(self, application_id: str) -> dict[str, Any] | None:
        return self._ashby_request("application.info", {"applicationId": application_id}).get(
            "results"
        )

    def _users(self, limit: int = 500) -> list[dict[str, Any]]:
        return self._ashby_paginate("user.list", {"includeDeactivated": True}, limit=limit)

    def _feedback(self, application_id: str, limit: int = 100) -> list[dict[str, Any]]:
        return self._ashby_paginate(
            "applicationFeedback.list", {"applicationId": application_id}, limit=limit
        )

    def _interview_events(self, limit: int = 500) -> list[dict[str, Any]]:
        return self._ashby_paginate("interviewEvent.list", limit=limit)

    def _requester_email(
        self, slack_user_id: str | None = None, requester_email: str | None = None
    ) -> str | None:
        if requester_email:
            return requester_email.strip().lower()
        env_email = os.environ.get("SLACK_REQUESTER_EMAIL", "").strip().lower()
        if env_email:
            return env_email
        slack_id = slack_user_id or os.environ.get("SLACK_REQUESTER_ID", "").strip()
        if slack_id:
            return self._slack_user_email(slack_id)
        return None

    def _is_admin(self, user: dict[str, Any]) -> bool:
        role = " ".join(
            str(user.get(key, ""))
            for key in ("globalRole", "role", "accessRole", "roleName")
        ).lower()
        return "admin" in role

    def _authorize(
        self,
        requester_email: str | None,
        application: dict[str, Any],
        feedback: list[dict[str, Any]],
        schedule: list[dict[str, Any]],
    ) -> AccessDecision:
        if not requester_email:
            return AccessDecision(False, "Could not resolve the Slack requester email")

        users = self._users()
        requester = next((u for u in users if _lower(u.get("email")) == requester_email), None)
        if not requester:
            return AccessDecision(False, "Requester is not an Ashby user", requester_email)
        if self._is_admin(requester):
            return AccessDecision(True, "Requester is an Ashby admin", requester_email, requester)

        team_emails = {
            _lower(member.get("email"))
            for member in application.get("hiringTeam", [])
            if member.get("email")
        }
        if requester_email in team_emails:
            return AccessDecision(
                True, "Requester is on the candidate's Ashby hiring team", requester_email, requester
            )

        feedback_emails = {
            _lower(fb.get("submittedByUser", {}).get("email"))
            for fb in feedback
            if fb.get("submittedByUser", {}).get("email")
        }
        if requester_email in feedback_emails:
            return AccessDecision(
                True, "Requester submitted feedback for this candidate", requester_email, requester
            )

        attendee_emails = {
            _lower(email)
            for event in schedule
            for email in event.get("attendees", [])
            if email
        }
        if requester_email in attendee_emails:
            return AccessDecision(
                True, "Requester is an interviewer on the candidate calendar event", requester_email, requester
            )

        return AccessDecision(
            False,
            "Requester is not an Ashby admin, hiring-team member, feedback submitter, or scheduled interviewer for this candidate",
            requester_email,
            requester,
        )

    def _candidate_summary(self, candidate: dict[str, Any]) -> str:
        role = candidate.get("position") or "candidate"
        company = candidate.get("company")
        school = candidate.get("school")
        location = candidate.get("location", {}).get("locationSummary")
        sentence = f"{candidate.get('name')} is a {role}"
        if company:
            sentence += f" at {company}"
        if location:
            sentence += f" based in {location}"
        sentence += "."
        second = "Ashby"
        if school:
            second += f" lists {school} as his school"
        links = candidate.get("socialLinks", [])
        if links:
            second += " and includes a LinkedIn profile"
        if second == "Ashby":
            second += " has limited background detail beyond the current role"
        second += "."
        return f"{sentence} {second}"

    def _event_format(self, event: dict[str, Any]) -> dict[str, Any]:
        start = _parse_dt(event.get("start", ""))
        end = _parse_dt(event.get("end", ""))
        duration = None
        if start and end:
            duration = int((end - start).total_seconds() // 60)
        location = event.get("location", "") or ""
        text = "Zoom" if "zoom" in location.lower() else "in person" if location else "scheduled"
        if duration:
            text = f"{duration} minute {text} interview"
        if location and "zoom" not in location.lower():
            text += f" at {location}"
        return {
            "summary": event.get("summary", ""),
            "start": event.get("start", ""),
            "end": event.get("end", ""),
            "duration_minutes": duration,
            "medium": "Zoom" if "zoom" in location.lower() else "in person" if location else "unknown",
            "location": location,
            "format": text,
        }

    def _previous_interviews(
        self, candidate_name: str, feedback: list[dict[str, Any]], schedule: list[dict[str, Any]]
    ) -> list[str]:
        now = datetime.now(timezone.utc)
        previous: list[str] = []
        seen: set[str] = set()
        for fb in feedback:
            user = _full_name(fb.get("submittedByUser", {}))
            submitted = fb.get("submittedAt") or fb.get("createdAt") or ""
            date = submitted[:10] if submitted else ""
            label = f"met with {user} {date}" if user and date else user or date
            if label and label not in seen:
                seen.add(label)
                previous.append(label)
        for event in schedule:
            start = _parse_dt(event.get("start", ""))
            summary = event.get("summary", "")
            if not start or start >= now or candidate_name.lower() not in summary.lower():
                continue
            date = start.strftime("%-m/%-d") if hasattr(start, "strftime") else event["start"][:10]
            label = f"{summary} {date}"
            if label not in seen:
                seen.add(label)
                previous.append(label)
        return previous[:5]

    def _focus_areas(self, application: dict[str, Any], feedback: list[dict[str, Any]]) -> str:
        job_title = application.get("job", {}).get("title", "")
        stage_title = application.get("currentInterviewStage", {}).get("title", "")
        concerns: list[str] = []
        for fb in feedback:
            for key in ("overallSummary", "summary", "notes", "privateNotes"):
                value = fb.get(key)
                if isinstance(value, str) and value.strip():
                    concerns.append(value.strip())
        if concerns:
            return "Follow up on prior feedback themes: " + "; ".join(concerns[:2])
        if "government" in job_title.lower() or "policy" in stage_title.lower():
            return (
                "Democratic congressional relationships, crypto policy judgment, pace, "
                "horsepower, and ability to turn DC context into specific tactics."
            )
        return "Validate role-specific judgment, pace, horsepower, and any gaps not covered in prior interviews."

    def brief(
        self,
        candidate_name: str,
        slack_user_id: str | None = None,
        requester_email: str | None = None,
        days_ahead: int = 30,
    ) -> dict[str, Any]:
        """Generate a permission-gated interview prep brief."""
        matches = self._candidate_search(candidate_name)
        if not matches:
            raise RuntimeError(f"No Ashby candidate found for {candidate_name!r}")
        candidate = self._candidate(matches[0]["id"]) or matches[0]
        application_ids = candidate.get("applicationIds", [])
        if not application_ids:
            raise RuntimeError(f"Candidate {candidate.get('name')} has no Ashby applications")
        application = self._application(application_ids[0])
        if not application:
            raise RuntimeError(f"Application {application_ids[0]} was not found")

        now = datetime.now(timezone.utc)
        schedule = self._calendar_events(candidate.get("name", candidate_name), now - timedelta(days=45), now + timedelta(days=days_ahead))
        upcoming = [event for event in schedule if (_parse_dt(event.get("start", "")) or now) >= now]
        feedback = self._feedback(application["id"])
        email = self._requester_email(slack_user_id=slack_user_id, requester_email=requester_email)
        decision = self._authorize(email, application, feedback, schedule)
        if not decision.granted:
            return {
                "access_granted": False,
                "reason": decision.reason,
                "requester_email": decision.requester_email,
                "candidate_name": candidate.get("name"),
            }

        return {
            "access_granted": True,
            "access_reason": decision.reason,
            "requester_email": decision.requester_email,
            "candidate": {
                "id": candidate.get("id"),
                "name": candidate.get("name"),
                "profile_url": candidate.get("profileUrl"),
            },
            "application": {
                "id": application.get("id"),
                "job": application.get("job", {}).get("title"),
                "stage": application.get("currentInterviewStage", {}).get("title"),
            },
            "upcoming_interviews": [self._event_format(event) for event in upcoming],
            "background_summary": self._candidate_summary(candidate),
            "previous_interviews": self._previous_interviews(candidate.get("name", candidate_name), feedback, schedule),
            "focus": self._focus_areas(application, feedback),
        }

    def close(self):
        self._http.close()

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def _client() -> InterviewPrepClient:
    return InterviewPrepClient()
