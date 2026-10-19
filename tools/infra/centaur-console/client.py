"""Client for centaur-console sandbox-scoped permission introspection."""

from __future__ import annotations

import os
from typing import Any
from urllib.parse import quote

import httpx

SANDBOX_PERMISSIONS_PATH = "/api/v1/sandbox/permissions"
SANDBOX_OAUTH_APPS_PATH = "/api/v1/sandbox/oauth_apps"
SANDBOX_SCHEDULED_TASKS_PATH = "/api/v1/sandbox/scheduled_tasks"
SANDBOX_ADMIN_PRINCIPALS_PATH = "/api/v1/sandbox/admin/principals"
SANDBOX_ADMIN_ROLES_PATH = "/api/v1/sandbox/admin/roles"
ADMIN_PAGE_LIMIT = 200


class ConsoleClient:
    """Read the current sandbox's redacted permissions from centaur-console."""

    def __init__(
        self,
        url: str | None = None,
        bearer_token: str | None = None,
        timeout: float = 30.0,
        transport: httpx.BaseTransport | None = None,
    ):
        self._url = url
        self._bearer_token = bearer_token
        self.timeout = timeout
        self._transport = transport
        self._client: httpx.Client | None = None

    @property
    def base_url(self) -> str:
        # Non-secret endpoint config. Sandboxes receive this from api-rs.
        url = (self._url or os.getenv("CENTAUR_CONSOLE_URL", "http://centaur-console:3000")).strip().rstrip("/")  # noqa: TID251
        if url and not url.startswith(("http://", "https://")):
            url = f"http://{url}"
        return url

    def _headers(self) -> dict[str, str]:
        headers = {"Accept": "application/json"}
        # Optional local/debug override. In sandboxes, iron-proxy injects the
        # scoped Authorization header for this endpoint.
        bearer = (self._bearer_token or os.getenv("CENTAUR_CONSOLE_BEARER_TOKEN", "")).strip()  # noqa: TID251
        if bearer:
            headers["Authorization"] = f"Bearer {bearer}"
        return headers

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(
                base_url=self.base_url,
                headers=self._headers(),
                timeout=self.timeout,
                transport=self._transport,
            )
        return self._client

    def sandbox_permissions(self) -> dict[str, Any]:
        """Return the current sandbox's redacted permissions payload."""
        response = self.client.get(SANDBOX_PERMISSIONS_PATH)
        try:
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            detail = _response_error_detail(exc.response)
            raise RuntimeError(f"centaur-console permissions request failed: {detail}") from exc

        payload = response.json()
        data = payload.get("data")
        if not isinstance(data, dict):
            raise RuntimeError("centaur-console permissions response did not include a data object")
        return data

    def permissions(self) -> dict[str, Any]:
        """Alias for tool bridge calls."""
        return self.sandbox_permissions()

    def sandbox_oauth_apps(self) -> list[dict[str, Any]]:
        """Return enabled OAuth apps with user-facing consent start URLs."""
        response = self.client.get(SANDBOX_OAUTH_APPS_PATH)
        try:
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            detail = _response_error_detail(exc.response)
            raise RuntimeError(f"centaur-console OAuth apps request failed: {detail}") from exc

        payload = response.json()
        data = payload.get("data")
        if not isinstance(data, list):
            raise RuntimeError("centaur-console OAuth apps response did not include a data array")
        return data

    def oauth_apps(self) -> list[dict[str, Any]]:
        """Alias for tool bridge calls."""
        return self.sandbox_oauth_apps()

    def scheduled_tasks(self) -> list[dict[str, Any]]:
        """List scheduled tasks owned by the current Console user."""
        result = self._scheduled_task_request(SANDBOX_SCHEDULED_TASKS_PATH)
        if not isinstance(result, list):
            raise RuntimeError(
                "centaur-console scheduled tasks response did not include a data array"
            )
        return result

    def scheduled_task(self, task_id: str) -> dict[str, Any]:
        """Read one scheduled task owned by the current Console user."""
        result = self._scheduled_task_request(
            f"{SANDBOX_SCHEDULED_TASKS_PATH}/{quote(task_id, safe='')}"
        )
        if not isinstance(result, dict):
            raise RuntimeError(
                "centaur-console scheduled task response did not include a data object"
            )
        return result

    def create_scheduled_task(
        self,
        name: str,
        prompt: str,
        cron_expression: str | None = None,
        delivery_channel: str = "dm",
        enabled: bool = True,
    ) -> dict[str, Any]:
        """Create an ad-hoc or Pacific Time cron task."""
        result = self._scheduled_task_request(
            SANDBOX_SCHEDULED_TASKS_PATH,
            method="POST",
            json={
                "data": {
                    "name": name,
                    "prompt": prompt,
                    "cron_expression": cron_expression,
                    "delivery_channel": delivery_channel,
                    "enabled": enabled,
                }
            },
        )
        if not isinstance(result, dict):
            raise RuntimeError(
                "centaur-console scheduled task response did not include a data object"
            )
        return result

    def update_scheduled_task(
        self,
        task_id: str,
        name: str | None = None,
        prompt: str | None = None,
        cron_expression: str | None = None,
        delivery_channel: str | None = None,
        enabled: bool | None = None,
    ) -> dict[str, Any]:
        """Update selected fields on a scheduled task owned by the current Console user."""
        attributes: dict[str, str | bool | None] = {}
        for key, value in {
            "name": name,
            "prompt": prompt,
            "cron_expression": cron_expression,
            "delivery_channel": delivery_channel,
            "enabled": enabled,
        }.items():
            if value is not None:
                attributes[key] = value
        if not attributes:
            raise ValueError("at least one scheduled task field must be provided")

        result = self._scheduled_task_request(
            f"{SANDBOX_SCHEDULED_TASKS_PATH}/{quote(task_id, safe='')}",
            method="PATCH",
            json={"data": attributes},
        )
        if not isinstance(result, dict):
            raise RuntimeError(
                "centaur-console scheduled task response did not include a data object"
            )
        return result

    def delete_scheduled_task(self, task_id: str) -> None:
        """Delete a scheduled task owned by the current Console user."""
        self._scheduled_task_request(
            f"{SANDBOX_SCHEDULED_TASKS_PATH}/{quote(task_id, safe='')}",
            method="DELETE",
        )

    def run_scheduled_task(self, task_id: str) -> dict[str, Any]:
        """Queue an immediate run of a scheduled task owned by the current Console user."""
        result = self._scheduled_task_request(
            f"{SANDBOX_SCHEDULED_TASKS_PATH}/{quote(task_id, safe='')}/run",
            method="POST",
        )
        if not isinstance(result, dict):
            raise RuntimeError(
                "centaur-console scheduled task response did not include a data object"
            )
        return result

    def admin_principals(self, slack_channel_id: str | None = None) -> list[dict[str, Any]]:
        """List principals, or only those bound to a Slack channel. Console admins only."""
        if slack_channel_id:
            return self._admin_list(
                SANDBOX_ADMIN_PRINCIPALS_PATH, {"slack_channel_id": slack_channel_id}
            )
        return self._admin_list_all(SANDBOX_ADMIN_PRINCIPALS_PATH)

    def admin_principal(self, principal: str) -> dict[str, Any]:
        """Read a principal by OID or foreign ID, with its assigned roles."""
        result = self._admin_object(
            self._admin_ref_path(SANDBOX_ADMIN_PRINCIPALS_PATH, principal, "prn_")
        )
        result["roles"] = self._admin_list(f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/{result['id']}/roles")
        return result

    def admin_roles(self) -> list[dict[str, Any]]:
        """List every role. Console admins only."""
        return self._admin_list_all(SANDBOX_ADMIN_ROLES_PATH)

    def admin_assign_role(self, principal: str, role: str) -> dict[str, Any]:
        """Assign a role (OID or foreign ID) to a principal (OID or foreign ID)."""
        principal_id, role_id = self._admin_principal_and_role_ids(principal, role)
        return self._admin_object(
            f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/{principal_id}/roles",
            method="POST",
            json={"data": {"role_id": role_id}},
        )

    def admin_unassign_role(self, principal: str, role: str) -> None:
        """Remove a role (OID or foreign ID) from a principal (OID or foreign ID)."""
        principal_id, role_id = self._admin_principal_and_role_ids(principal, role)
        self._admin_request(
            f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/{principal_id}/roles/{role_id}", method="DELETE"
        )

    def _admin_principal_and_role_ids(self, principal: str, role: str) -> tuple[str, str]:
        principal_path = self._admin_ref_path(SANDBOX_ADMIN_PRINCIPALS_PATH, principal, "prn_")
        role_path = self._admin_ref_path(SANDBOX_ADMIN_ROLES_PATH, role, "role_")
        return self._admin_object(principal_path)["id"], self._admin_object(role_path)["id"]

    @staticmethod
    def _admin_ref_path(collection: str, ref: str, oid_prefix: str) -> str:
        if ref.startswith(oid_prefix):
            return f"{collection}/{quote(ref, safe='')}"
        return f"{collection}/lookup/{quote(ref, safe='')}"

    def _admin_list_all(self, path: str) -> list[dict[str, Any]]:
        records: list[dict[str, Any]] = []
        page = 1
        while True:
            payload = self._admin_request(path, params={"page": page, "limit": ADMIN_PAGE_LIMIT})
            records.extend(_admin_data(payload, list))
            if page >= (payload.get("meta") or {}).get("total_pages", 0):
                return records
            page += 1

    def _admin_list(self, path: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
        return _admin_data(self._admin_request(path, params=params), list)

    def _admin_object(
        self, path: str, method: str = "GET", json: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        return _admin_data(self._admin_request(path, method=method, json=json), dict)

    def _admin_request(
        self,
        path: str,
        method: str = "GET",
        params: dict[str, Any] | None = None,
        json: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        try:
            response = self.client.request(method, path, params=params, json=json)
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            detail = _response_error_detail(exc.response)
            raise RuntimeError(f"centaur-console admin request failed: {detail}") from exc
        except httpx.RequestError as exc:
            raise RuntimeError(f"centaur-console admin request failed: {exc}") from exc
        return response.json() if response.content else {}

    def health(self) -> dict[str, Any]:
        """Assert the sandbox permissions endpoint is reachable and authorized."""
        try:
            data = self.sandbox_permissions()
            return {
                "ok": True,
                "tool": "centaur-console",
                "error": None,
                "details": {
                    "sandbox_id": data.get("sandbox_id"),
                    "principal_id": data.get("principal_id"),
                    "proxy_id": data.get("proxy_id"),
                },
            }
        except Exception as exc:
            return {
                "ok": False,
                "tool": "centaur-console",
                "error": str(exc),
                "details": {},
            }

    def _scheduled_task_request(
        self,
        path: str,
        method: str = "GET",
        json: dict[str, Any] | None = None,
    ) -> dict[str, Any] | list[dict[str, Any]] | None:
        try:
            response = self.client.request(method, path, json=json)
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            detail = _response_error_detail(exc.response)
            raise RuntimeError(f"centaur-console scheduled task request failed: {detail}") from exc
        except httpx.RequestError as exc:
            raise RuntimeError(f"centaur-console scheduled task request failed: {exc}") from exc

        if not response.content:
            return None

        payload = response.json()
        data = payload.get("data")
        if not isinstance(data, (dict, list)):
            raise RuntimeError("centaur-console scheduled task response did not include data")
        return data

    def close(self) -> None:
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self) -> ConsoleClient:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()


def _response_error_detail(response: httpx.Response) -> str:
    try:
        body = response.json()
    except ValueError:
        body = response.text
    return f"HTTP {response.status_code}: {body}"


def _admin_data(payload: dict[str, Any], kind: type) -> Any:
    data = payload.get("data")
    if not isinstance(data, kind):
        raise RuntimeError("centaur-console admin response did not include the expected data")
    return data


def _client() -> ConsoleClient:
    return ConsoleClient()
