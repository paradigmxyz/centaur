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
SANDBOX_ADMIN_GRANTS_PATH = "/api/v1/sandbox/admin/grants"
SANDBOX_ADMIN_SECRETS_PATH = "/api/v1/sandbox/admin/secrets"
SANDBOX_ADMIN_SLACK_EXTERNAL_TEAMS_PATH = "/api/v1/sandbox/admin/slack_external_teams"
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

    def admin_create_principal(
        self,
        slack_channel_id: str | None = None,
        slack_user_id: str | None = None,
    ) -> dict[str, Any]:
        """Create a Slack channel or Slack user principal; the Console derives its foreign ID."""
        if (slack_channel_id is None) == (slack_user_id is None):
            raise ValueError("pass exactly one of slack_channel_id or slack_user_id")
        data = (
            {"kind": "slack_channel", "slack_channel_id": slack_channel_id}
            if slack_channel_id is not None
            else {"kind": "slack_dm", "slack_user_id": slack_user_id}
        )
        return self._admin_object(SANDBOX_ADMIN_PRINCIPALS_PATH, method="POST", json={"data": data})

    def admin_principal(self, principal: str) -> dict[str, Any]:
        """Read a principal by OID or foreign ID, with its roles and direct grants."""
        result = self._admin_object(
            self._admin_ref_path(SANDBOX_ADMIN_PRINCIPALS_PATH, principal, "prn_")
        )
        path = f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/{result['id']}"
        result["roles"] = self._admin_list(f"{path}/roles")
        result["grants"] = self._admin_list_all(f"{path}/grants")
        return result

    def admin_effective_grants(self, principal: str) -> list[dict[str, Any]]:
        """List secrets a principal holds directly or through roles, with their sources."""
        principal_id = self._admin_principal_id(principal)
        return self._admin_list_all(
            f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/{principal_id}/effective_grants"
        )

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

    def admin_secrets(self, query: str | None = None) -> list[dict[str, Any]]:
        """List grantable secrets, optionally filtered by name or foreign ID."""
        return self._admin_list(SANDBOX_ADMIN_SECRETS_PATH, {"q": query} if query else None)

    def admin_grant_secret(self, principal: str, secret: str) -> dict[str, Any]:
        """Grant a secret (OID, foreign ID, or name) directly to a principal."""
        principal_id = self._admin_principal_id(principal)
        match = self._admin_secret(secret)
        return self._admin_object(
            SANDBOX_ADMIN_GRANTS_PATH,
            method="POST",
            json={"data": {"principal_id": principal_id, match["grant_key"]: match["id"]}},
        )

    def admin_revoke_secret(self, principal: str, secret: str) -> None:
        """Revoke a secret granted directly to a principal."""
        principal_id = self._admin_principal_id(principal)
        match = self._admin_secret(secret)
        grants = self._admin_list_all(f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/{principal_id}/grants")
        grant = next((g for g in grants if g.get(match["grant_key"]) == match["id"]), None)
        if grant is None:
            raise RuntimeError(f"{secret} is not granted directly to {principal}")
        self._admin_request(f"{SANDBOX_ADMIN_GRANTS_PATH}/{grant['id']}", method="DELETE")

    def admin_set_slack_permission(
        self,
        principal: str,
        channel_id: str,
        upload: bool | None = None,
        download: bool | None = None,
        history: bool | None = None,
    ) -> dict[str, Any]:
        """Create or update a principal's Slack channel permission.

        New permissions default every unspecified flag to enabled.
        """
        data: dict[str, Any] = {"channel_id": channel_id}
        for key, value in {
            "upload_enabled": upload,
            "download_enabled": download,
            "history_enabled": history,
        }.items():
            if value is not None:
                data[key] = value
        principal_id = self._admin_principal_id(principal)
        return self._admin_object(
            f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/{principal_id}/slack_channel_permissions",
            method="POST",
            json={"data": data},
        )

    def admin_remove_slack_permission(self, principal: str, channel_id: str) -> None:
        """Remove a principal's own permission for a Slack channel."""
        principal_id = self._admin_principal_id(principal)
        self._admin_request(
            f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/{principal_id}/slack_channel_permissions/"
            f"{quote(channel_id, safe='')}",
            method="DELETE",
        )

    def admin_set_sandbox_access(self, principal: str, **capabilities: Any) -> dict[str, Any]:
        """Update selected sandbox capabilities, such as sandbox_sessions_read_enabled."""
        data = {key: value for key, value in capabilities.items() if value is not None}
        if not data:
            raise ValueError("at least one sandbox capability must be provided")
        principal_id = self._admin_principal_id(principal)
        return self._admin_object(
            f"{SANDBOX_ADMIN_PRINCIPALS_PATH}/{principal_id}/sandbox_access",
            method="PATCH",
            json={"data": data},
        )

    def admin_slack_external_teams(self) -> list[str]:
        """List Slack Connect workspace and org ids Centaur accepts messages from."""
        return self._admin_list(SANDBOX_ADMIN_SLACK_EXTERNAL_TEAMS_PATH)

    def admin_add_slack_external_team(self, team_id: str) -> list[str]:
        """Allow a Slack Connect workspace (T…) or Enterprise Grid org (E…)."""
        return self._admin_list(
            SANDBOX_ADMIN_SLACK_EXTERNAL_TEAMS_PATH,
            method="POST",
            json={"data": {"team_id": team_id}},
        )

    def admin_remove_slack_external_team(self, team_id: str) -> list[str]:
        """Stop accepting messages from a Slack Connect workspace or org."""
        return self._admin_list(
            f"{SANDBOX_ADMIN_SLACK_EXTERNAL_TEAMS_PATH}/{quote(team_id, safe='')}",
            method="DELETE",
        )

    def _admin_principal_id(self, principal: str) -> str:
        path = self._admin_ref_path(SANDBOX_ADMIN_PRINCIPALS_PATH, principal, "prn_")
        return self._admin_object(path)["id"]

    def _admin_secret(self, secret: str) -> dict[str, Any]:
        matches = [
            row
            for row in self.admin_secrets()
            if secret in (row.get("id"), row.get("foreign_id"), row.get("name"))
        ]
        if len(matches) != 1:
            found = "no secret" if not matches else "several secrets"
            raise RuntimeError(f"{found} matched {secret}; pass the secret OID")
        return matches[0]

    def _admin_principal_and_role_ids(self, principal: str, role: str) -> tuple[str, str]:
        role_path = self._admin_ref_path(SANDBOX_ADMIN_ROLES_PATH, role, "role_")
        return self._admin_principal_id(principal), self._admin_object(role_path)["id"]

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

    def _admin_list(
        self,
        path: str,
        params: dict[str, Any] | None = None,
        method: str = "GET",
        json: dict[str, Any] | None = None,
    ) -> list[Any]:
        return _admin_data(self._admin_request(path, method=method, params=params, json=json), list)

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
