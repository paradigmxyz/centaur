"""Mercator MCP client with Centaur-hosted paid execution."""

from __future__ import annotations

import json
import os
import time
from typing import Any

import httpx

DEFAULT_MCP_URL = "https://mercator.tempoxyz.dev/mcp"
DEFAULT_CENTAUR_API_URL = "http://api:8000"
SUBMIT_PATH = "/api/mercator/jobs"


class MercatorClient:
    """Discover, quote, and execute Mercator services from any Centaur harness."""

    def __init__(
        self,
        mcp_url: str | None = None,
        centaur_api_url: str | None = None,
        bearer_token: str | None = None,
        timeout: float = 30.0,
        transport: httpx.BaseTransport | None = None,
    ) -> None:
        self.mcp_url = (
            mcp_url or os.getenv("MERCATOR_MCP_URL", DEFAULT_MCP_URL)  # noqa: TID251
        ).rstrip("/")
        self.centaur_api_url = (
            centaur_api_url
            or os.getenv("CENTAUR_API_URL", DEFAULT_CENTAUR_API_URL)  # noqa: TID251
        ).rstrip("/")
        self.bearer_token = bearer_token
        self.timeout = timeout
        self.transport = transport
        self._http: httpx.Client | None = None

    @property
    def http(self) -> httpx.Client:
        if self._http is None:
            self._http = httpx.Client(timeout=self.timeout, transport=self.transport)
        return self._http

    def close(self) -> None:
        if self._http is not None:
            self._http.close()
            self._http = None

    def __enter__(self) -> MercatorClient:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def search_services(
        self,
        query: str,
        limit: int = 8,
        resolution: str = "static",
        service_ids: list[str] | None = None,
        service_mode: str | None = None,
    ) -> dict[str, Any]:
        """Search Mercator for the user's complete external-data or API outcome. Free."""
        arguments: dict[str, Any] = {
            "query": query,
            "limit": limit,
            "resolution": resolution,
        }
        if service_ids is not None:
            arguments["service_ids"] = service_ids
        if service_mode is not None:
            arguments["service_mode"] = service_mode
        return self._mcp_call("search_services", arguments)

    def describe_service(
        self,
        service_id: str,
        method: str | None = None,
        path: str | None = None,
    ) -> dict[str, Any]:
        """Read a Mercator service or endpoint schema. Free."""
        arguments: dict[str, Any] = {"service_id": service_id}
        if method is not None:
            arguments["method"] = method
        if path is not None:
            arguments["path"] = path
        return self._mcp_call("describe_service", arguments)

    def quote_plan(self, plan: dict[str, Any]) -> dict[str, Any]:
        """Validate and price a complete Mercator plan without paying or creating a job."""
        return self._mcp_call("quote_plan", {"plan": plan})

    def create_job(
        self, plan: dict[str, Any], idempotency_key: str
    ) -> dict[str, Any]:
        """Create a Mercator payment handoff after quoting; this method does not pay."""
        return self._mcp_call(
            "create_job", {"plan": plan, "idempotency_key": idempotency_key}
        )

    def submit_job(
        self,
        handoff: dict[str, Any],
        approved: bool = False,
        wait: bool = True,
        poll_interval: float = 2.0,
        wait_timeout: float = 90.0,
    ) -> dict[str, Any]:
        """Submit through Centaur policy and, by default, return the terminal job."""
        if poll_interval < 0:
            raise ValueError("poll_interval must be non-negative")
        if wait_timeout < 0:
            raise ValueError("wait_timeout must be non-negative")
        response = self.http.post(
            f"{self.centaur_api_url}{SUBMIT_PATH}",
            headers=self._centaur_headers(),
            json={"approved": approved, "handoff": handoff},
        )
        self._raise_for_status(response, "Centaur Mercator payer")
        payload = response.json()
        if not isinstance(payload, dict):
            raise RuntimeError("Centaur Mercator payer returned invalid JSON")
        if not wait:
            return payload

        job = self._submission_job(payload)
        job_id = job.get("jobId")
        if not isinstance(job_id, str) or not job_id:
            raise RuntimeError("Centaur Mercator payer response is missing a job ID")
        if job.get("ready") is not True:
            deadline = time.monotonic() + wait_timeout
            while time.monotonic() <= deadline:
                polled = self.get_job(job_id).get("job")
                if not isinstance(polled, dict):
                    raise RuntimeError("Mercator get_job response is missing a job")
                job = polled
                if job.get("ready") is True:
                    break
                if poll_interval:
                    time.sleep(poll_interval)
            else:
                payload["job"] = job
                payload["polling"] = {
                    "status": "timed_out",
                    "nextAction": f"Retry get_job with job_id {job_id}",
                }
                return payload

        payload["job"] = job
        usage = job.get("usage")
        if isinstance(usage, dict):
            payload["receipt"] = {"jobId": job_id, "usage": usage}
        return payload

    def get_job(self, job_id: str) -> dict[str, Any]:
        """Read or poll a Mercator job. Free."""
        return self._mcp_call("get_job", {"job_id": job_id})

    def create_job_review(
        self, job_id: str, review: dict[str, Any]
    ) -> dict[str, Any]:
        """Store an explicitly requested review for a completed Mercator job."""
        return self._mcp_call(
            "create_job_review", {"job_id": job_id, "review": review}
        )

    def get_job_review(self, job_id: str) -> dict[str, Any]:
        """Read or advance the reward status for an existing Mercator job review."""
        return self._mcp_call("get_job_review", {"job_id": job_id})

    def send_product_feedback(self, report: dict[str, Any]) -> dict[str, Any]:
        """Send sanitized feedback only when the user explicitly asks to contact Mercator."""
        return self._mcp_call("send_product_feedback", report)

    def _mcp_call(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        response = self.http.post(
            self.mcp_url,
            headers={
                "Accept": "application/json, text/event-stream",
                "Content-Type": "application/json",
            },
            json={
                "id": 1,
                "jsonrpc": "2.0",
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            },
        )
        self._raise_for_status(response, "Mercator MCP")
        envelope = self._decode_mcp_envelope(response.text)
        error = envelope.get("error")
        if isinstance(error, dict):
            raise RuntimeError(str(error.get("message") or "Mercator MCP request failed"))
        result = envelope.get("result")
        if not isinstance(result, dict):
            raise RuntimeError("Mercator MCP response is missing a result")
        structured = result.get("structuredContent")
        if isinstance(structured, dict):
            return structured
        content = result.get("content")
        if isinstance(content, list):
            for item in content:
                if isinstance(item, dict) and isinstance(item.get("text"), str):
                    try:
                        parsed = json.loads(item["text"])
                    except ValueError:
                        continue
                    if isinstance(parsed, dict):
                        return parsed
        return result

    def _centaur_headers(self) -> dict[str, str]:
        headers = {"Accept": "application/json"}
        bearer = (
            self.bearer_token
            or os.getenv("CENTAUR_API_BEARER_TOKEN", "")  # noqa: TID251
        ).strip()
        if bearer:
            headers["Authorization"] = f"Bearer {bearer}"
        return headers

    @staticmethod
    def _submission_job(payload: dict[str, Any]) -> dict[str, Any]:
        result = payload.get("result")
        if isinstance(result, dict):
            if isinstance(result.get("jobId"), str):
                return result
            job = result.get("job")
            if isinstance(job, dict):
                if isinstance(job.get("jobId"), str):
                    return job
                if isinstance(job.get("id"), str):
                    return {**job, "jobId": job["id"]}
        raise RuntimeError("Centaur Mercator payer response is missing a job ID")

    @staticmethod
    def _decode_mcp_envelope(body: str) -> dict[str, Any]:
        payload = body
        for line in body.splitlines():
            if line.startswith("data: "):
                payload = line[6:]
                break
        try:
            envelope = json.loads(payload)
        except ValueError as exc:
            raise RuntimeError("Mercator MCP returned invalid JSON") from exc
        if not isinstance(envelope, dict):
            raise RuntimeError("Mercator MCP returned an invalid response envelope")
        return envelope

    @staticmethod
    def _raise_for_status(response: httpx.Response, service: str) -> None:
        try:
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            message = f"{service} returned HTTP {response.status_code}"
            try:
                payload = response.json()
            except ValueError:
                payload = None
            if isinstance(payload, dict) and isinstance(payload.get("error"), str):
                message = payload["error"]
            raise RuntimeError(message) from exc


_CLIENT: MercatorClient | None = None


def _client() -> MercatorClient:
    """Return the process-wide client used by Centaur's tool runtime."""
    global _CLIENT
    if _CLIENT is None:
        _CLIENT = MercatorClient()
    return _CLIENT
