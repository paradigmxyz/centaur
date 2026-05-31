"""Claap API client."""

from __future__ import annotations

import re
from typing import Any, Literal

import httpx

from centaur_sdk import secret

API_BASE = "https://api.claap.io/v1"

TranscriptFormat = Literal["json", "text"]


def recording_id_from_input(value: str) -> str:
    """Extract a Claap recording id from a raw id or common Claap URL shapes."""
    candidate = value.strip()
    if not candidate:
        raise ValueError("recording id or URL is required")

    match = re.search(r"recordings/([^/?#]+)", candidate)
    if match:
        return match.group(1)

    match = re.search(r"[?&]recordingId=([^&#]+)", candidate)
    if match:
        return match.group(1)

    match = re.search(r"/([A-Za-z0-9_-]{8,})(?:[/?#].*)?$", candidate)
    if match:
        return match.group(1)

    return candidate


def _normalize_date(value: str | None) -> str | None:
    if value is None:
        return None
    stripped = value.strip()
    if not stripped:
        return None
    match = re.match(r"^(\d{4}-\d{2}-\d{2})(?:$|T)", stripped)
    if not match:
        raise ValueError("Claap date filters must be YYYY-MM-DD or ISO timestamps")
    return match.group(1)


class ClaapClient:
    """Authenticated Claap API client."""

    def __init__(
        self,
        api_key: str | None = None,
        base_url: str = API_BASE,
        timeout: float = 30.0,
    ):
        self._api_key_override = api_key
        self.base_url = base_url.rstrip("/") + "/"
        self.timeout = timeout
        self._client: httpx.Client | None = None

    def _api_key(self) -> str:
        api_key = (self._api_key_override or secret("CLAAP_API_KEY", "")).strip()
        if not api_key:
            raise RuntimeError("CLAAP_API_KEY not set.")
        return api_key

    def _http(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(
                base_url=self.base_url,
                headers={"X-Claap-Key": self._api_key()},
                timeout=self.timeout,
            )
        return self._client

    def _request(
        self,
        path: str,
        params: dict[str, Any] | None = None,
        *,
        expect_json: bool = True,
    ) -> dict[str, Any] | str:
        response = self._http().get(path.lstrip("/"), params=params)
        if response.status_code >= 400:
            raise RuntimeError(
                f"Claap API error ({response.status_code}): {response.text}"
            )
        if expect_json:
            return response.json()
        return response.text

    def list_recordings(
        self,
        limit: int = 20,
        created_after: str | None = None,
        created_before: str | None = None,
        channel_id: str | None = None,
        recorder_email: str | None = None,
    ) -> dict[str, Any]:
        """List Claap recordings.

        Args:
            limit: Maximum recordings to return.
            created_after: Optional YYYY-MM-DD or ISO timestamp lower bound.
            created_before: Optional YYYY-MM-DD or ISO timestamp upper bound.
            channel_id: Optional Claap channel/folder id filter.
            recorder_email: Optional recorder email filter.
        """
        params: dict[str, Any] = {"limit": limit}
        normalized_after = _normalize_date(created_after)
        normalized_before = _normalize_date(created_before)
        if normalized_after:
            params["createdAfter"] = normalized_after
        if normalized_before:
            params["createdBefore"] = normalized_before
        if channel_id:
            params["channelId"] = channel_id
        if recorder_email:
            params["recorderEmail"] = recorder_email
        result = self._request("recordings", params=params)
        return result if isinstance(result, dict) else {"result": result}

    def get_recording(
        self,
        recording_id_or_url: str,
        return_ai_fields: bool = True,
    ) -> dict[str, Any]:
        """Get one Claap recording by id or URL."""
        recording_id = recording_id_from_input(recording_id_or_url)
        params = {"returnAiFields": "true"} if return_ai_fields else None
        result = self._request(f"recordings/{recording_id}", params=params)
        return result if isinstance(result, dict) else {"result": result}

    def get_transcript(
        self,
        recording_id_or_url: str,
        format: TranscriptFormat = "json",
        lang: str | None = None,
    ) -> dict[str, Any]:
        """Get a recording transcript as JSON utterances or plain text."""
        if format not in {"json", "text"}:
            raise ValueError("format must be 'json' or 'text'")

        recording_id = recording_id_from_input(recording_id_or_url)
        params: dict[str, Any] = {"format": format}
        if lang:
            params["lang"] = lang
        result = self._request(
            f"recordings/{recording_id}/transcript",
            params=params,
            expect_json=format == "json",
        )
        if isinstance(result, dict):
            return result
        return {"recording_id": recording_id, "format": format, "transcript": result}

    def get_recording_bundle(
        self,
        recording_id_or_url: str,
        transcript_format: TranscriptFormat = "json",
        lang: str | None = None,
    ) -> dict[str, Any]:
        """Get recording metadata plus transcript in one response."""
        recording_id = recording_id_from_input(recording_id_or_url)
        return {
            "recording_id": recording_id,
            "recording": self.get_recording(recording_id, return_ai_fields=True),
            "transcript": self.get_transcript(
                recording_id,
                format=transcript_format,
                lang=lang,
            ),
        }

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None

    def __enter__(self) -> "ClaapClient":
        return self

    def __exit__(self, *args: object) -> None:
        self.close()


def _client() -> ClaapClient:
    return ClaapClient()
