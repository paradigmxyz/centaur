"""Crew management API. Slack configuration and bot tokens remain server-side."""

from typing import Any

import httpx

from centaur_sdk import secret


class SlackCrewClient:
    def __init__(self, base_url: str | None = None, token: str | None = None):
        self.base_url = (base_url or secret("SLACK_CREW_URL", "http://centaur-slackbotv2:3001")).rstrip("/")
        self.token = token

    def _request(self, method: str, body: dict | None = None) -> dict[str, Any]:
        token = self.token or secret("SLACK_CREW_ADMIN_TOKEN")
        if not token:
            raise RuntimeError("SLACK_CREW_ADMIN_TOKEN is not granted")
        # No retries: an ambiguous app-creation response must be reconciled by ID.
        with httpx.Client(timeout=30, follow_redirects=False) as client:
            response = client.request(
                method, f"{self.base_url}/api/slack/crew", json=body,
                headers={"Authorization": f"Bearer {token}"},
            )
        if response.status_code not in (200, 201):
            raise RuntimeError(f"Crew API returned HTTP {response.status_code}; inspect status before retrying")
        return response.json()

    def list_bots(self) -> dict[str, Any]:
        """List Crew installation states; never returns Slack credentials."""
        return self._request("GET")

    def create_bot(self, id: str, name: str, crew_id: str) -> dict[str, Any]:
        """Create an app for an allowed profile; stable id prevents duplicate creation.

        Return its installation link. Slack consent is required before it can respond.
        """
        return self._request("POST", {"id": id, "name": name, "crew_id": crew_id})

    def health(self) -> dict[str, Any]:
        """Read-only management-auth check without returning installation links."""
        return {"ok": True, "bot_count": len(self.list_bots()["crew"])}


def _client() -> SlackCrewClient:
    return SlackCrewClient()
