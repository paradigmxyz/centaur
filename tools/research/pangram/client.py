"""Client for Pangram's AI and plagiarism detection APIs.

API reference: https://docs.pangram.com/api-reference/introduction
"""

from __future__ import annotations

import time
from typing import Any
from urllib.parse import quote

import httpx

from centaur_sdk import secret

TEXT_BASE_URL = "https://text.external-api.pangram.com"
PLAGIARISM_BASE_URL = "https://plagiarism.api.pangram.com"
DEFAULT_MODEL = "default"
DEFAULT_POLL_INTERVAL_SECONDS = 1.0
DEFAULT_TASK_TIMEOUT_SECONDS = 300.0
TERMINAL_STAGES = {"STAGE_SUCCESS", "STAGE_FAILED"}


class PangramClient:
    """Client for Pangram's text APIs."""

    def __init__(self, api_key: str | None = None, timeout: float = 30.0) -> None:
        self._api_key = api_key
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    @property
    def headers(self) -> dict[str, str]:
        return {"x-api-key": self._api_key or secret("PANGRAM_API_KEY")}

    def _request(
        self,
        method: str,
        url: str,
        *,
        json: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        response = self.client.request(method, url, headers=self.headers, json=json)
        response.raise_for_status()
        result = response.json()
        if not isinstance(result, dict):
            raise RuntimeError("Pangram returned an unexpected response.")
        return result

    def list_models(self) -> dict[str, Any]:
        """Return the ordered model selectors available to this API key."""
        return self._request("GET", f"{TEXT_BASE_URL}/models")

    def create_task(
        self,
        text: str,
        *,
        model: str = DEFAULT_MODEL,
        public_dashboard_link: bool = False,
    ) -> dict[str, Any]:
        """Create an asynchronous AI-detection task."""
        return self._request(
            "POST",
            f"{TEXT_BASE_URL}/task",
            json={
                "text": text,
                "model": model,
                "public_dashboard_link": public_dashboard_link,
            },
        )

    def get_task(self, task_id: str) -> dict[str, Any]:
        """Return the current status or result of an AI-detection task."""
        task_path = quote(task_id, safe="")
        return self._request("GET", f"{TEXT_BASE_URL}/task/{task_path}")

    def detect(
        self,
        text: str,
        *,
        model: str = DEFAULT_MODEL,
        public_dashboard_link: bool = False,
        task_timeout: float = DEFAULT_TASK_TIMEOUT_SECONDS,
        poll_interval: float = DEFAULT_POLL_INTERVAL_SECONDS,
    ) -> dict[str, Any]:
        """Create an AI-detection task and wait for its terminal result."""
        if task_timeout <= 0:
            raise ValueError("task_timeout must be greater than zero")
        if poll_interval <= 0:
            raise ValueError("poll_interval must be greater than zero")

        submitted = self.create_task(
            text,
            model=model,
            public_dashboard_link=public_dashboard_link,
        )
        task_id = submitted.get("task_id")
        if not isinstance(task_id, str) or not task_id:
            raise RuntimeError("Pangram did not return a task_id.")

        deadline = time.monotonic() + task_timeout
        while True:
            result = self.get_task(task_id)
            if result.get("stage") in TERMINAL_STAGES:
                return result

            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(
                    f"Pangram task {task_id} did not finish within {task_timeout:g}s"
                )
            time.sleep(min(poll_interval, remaining))

    def check_plagiarism(self, text: str) -> dict[str, Any]:
        """Check text for potential plagiarism against online content."""
        return self._request("POST", PLAGIARISM_BASE_URL, json={"text": text})

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None

    def __enter__(self) -> PangramClient:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()


def _client() -> PangramClient:
    return PangramClient()
