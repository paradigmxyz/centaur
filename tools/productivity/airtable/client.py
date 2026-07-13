"""Airtable API client for bases, schemas, views, and record writes."""

from __future__ import annotations

from typing import Any
from urllib.parse import quote, urlparse

import httpx

from centaur_sdk import secret

BASE_URL = "https://api.airtable.com/v0"
META_URL = f"{BASE_URL}/meta"
AIRTABLE_API_KEY_MISSING_MESSAGE = (
    "AIRTABLE_API_KEY not set. Add the 1Password item 'Airtable API Key' "
    "or export AIRTABLE_API_KEY for local use."
)
AIRTABLE_URL_MESSAGE = (
    "Airtable URL must use airtable.com and include an app/base ID and table ID, e.g. "
    "https://airtable.com/app.../tbl.../viw..."
)


def _clean_secret(value: str | None) -> str:
    if not value:
        return ""
    return value.strip().splitlines()[0].strip()


def _simplify_cell(value: Any) -> Any:
    """Keep Airtable cell values readable without losing nested data."""
    if isinstance(value, list):
        return [_simplify_cell(item) for item in value]
    if isinstance(value, dict):
        if "url" in value and "filename" in value:
            return {
                "filename": value.get("filename"),
                "url": value.get("url"),
                "type": value.get("type"),
                "size": value.get("size"),
            }
        if "email" in value and "name" in value:
            return {"name": value.get("name"), "email": value.get("email")}
        return {key: _simplify_cell(nested) for key, nested in value.items()}
    return value


def _compact_record(record: dict[str, Any], fields: list[str] | None = None) -> dict[str, Any]:
    raw_fields = record.get("fields") if isinstance(record.get("fields"), dict) else {}
    selected = raw_fields
    if fields:
        selected = {field: raw_fields[field] for field in fields if field in raw_fields}
    return {
        "id": record.get("id"),
        "createdTime": record.get("createdTime"),
        "fields": {key: _simplify_cell(value) for key, value in selected.items()},
    }


def _compact_records(
    records: list[dict[str, Any]], fields: list[str] | None = None
) -> list[dict[str, Any]]:
    return [_compact_record(record, fields) for record in records]


def _match_text(value: Any, query: str) -> bool:
    if value is None:
        return False
    if isinstance(value, (str, int, float, bool)):
        return query in str(value).lower()
    if isinstance(value, list):
        return any(_match_text(item, query) for item in value)
    if isinstance(value, dict):
        return any(_match_text(item, query) for item in value.values())
    return query in str(value).lower()


def _path_part(value: str) -> str:
    return quote(value, safe="")


def _clamp_batch_size(value: int) -> int:
    return max(1, min(value, 10))


def _record_batch(
    records: list[dict[str, Any]], batch_size: int = 10
) -> list[list[dict[str, Any]]]:
    size = _clamp_batch_size(batch_size)
    return [records[index : index + size] for index in range(0, len(records), size)]


def _record_id_batch(record_ids: list[str], batch_size: int = 10) -> list[list[str]]:
    size = _clamp_batch_size(batch_size)
    return [record_ids[index : index + size] for index in range(0, len(record_ids), size)]


def _normalize_create_records(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    normalized: list[dict[str, Any]] = []
    for record in records:
        fields = record.get("fields") if "fields" in record else record
        if not isinstance(fields, dict):
            raise ValueError(
                "Each record must be a field mapping or an object with a 'fields' mapping."
            )
        normalized.append({"fields": fields})
    return normalized


def _normalize_update_records(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    normalized: list[dict[str, Any]] = []
    for record in records:
        record_id = record.get("id")
        fields = record.get("fields")
        if not isinstance(record_id, str) or not record_id:
            raise ValueError("Each record update must include a non-empty 'id'.")
        if not isinstance(fields, dict):
            raise ValueError("Each record update must include a 'fields' mapping.")
        normalized.append({"id": record_id, "fields": fields})
    return normalized


def _airtable_host(host: str | None) -> str:
    return (host or "").split(":", 1)[0].lower()


def _is_airtable_url(host: str | None) -> bool:
    return _airtable_host(host) in {"airtable.com", "www.airtable.com"}


def _minimal_identity(whoami: dict[str, Any]) -> dict[str, Any]:
    """Return a privacy-minimized view of Airtable's whoami payload.

    Diagnostics should not echo the full identity (notably email) into Slack
    output by default. We only confirm presence and surface the user id plus
    a scopes count so the agent can reason about token capability.
    """
    if not isinstance(whoami, dict):
        return {"identity_present": False}
    scopes = whoami.get("scopes")
    return {
        "identity_present": True,
        "id": whoami.get("id"),
        "scopes_count": len(scopes) if isinstance(scopes, list) else None,
    }


def _error_payload(response: httpx.Response) -> tuple[str | None, str | None]:
    try:
        payload = response.json()
    except ValueError:
        return None, None
    error = payload.get("error")
    if not isinstance(error, dict):
        return None, None
    error_type = str(error.get("type") or "").strip() or None
    error_message = str(error.get("message") or "").strip() or None
    return error_type, error_message


class AirtableClient:
    """Client for Airtable's REST API."""

    def __init__(
        self,
        api_key: str | None = None,
        timeout: float = 30.0,
        *,
        allow_missing_api_key: bool = False,
    ):
        self._explicit_api_key = _clean_secret(api_key)
        self.api_key = ""
        self._client = httpx.Client(
            timeout=timeout,
            headers={"Content-Type": "application/json"},
        )
        self._set_api_key(self._explicit_api_key or _clean_secret(secret("AIRTABLE_API_KEY", "")))
        if not self.api_key and not allow_missing_api_key:
            raise RuntimeError(AIRTABLE_API_KEY_MISSING_MESSAGE)

    def _set_api_key(self, api_key: str | None) -> None:
        self.api_key = _clean_secret(api_key)
        if self.api_key:
            self._client.headers["Authorization"] = f"Bearer {self.api_key}"
        else:
            self._client.headers.pop("Authorization", None)

    def _refresh_api_key(self) -> str:
        if self.api_key:
            return self.api_key
        self._set_api_key(self._explicit_api_key or secret("AIRTABLE_API_KEY", ""))
        return self.api_key

    def _require_api_key(self) -> None:
        if self._refresh_api_key():
            return
        raise RuntimeError(AIRTABLE_API_KEY_MISSING_MESSAGE)

    def _send(
        self,
        method: str,
        url: str,
        *,
        params: dict[str, Any] | list[tuple[str, Any]] | None = None,
        json: dict[str, Any] | None = None,
    ) -> httpx.Response:
        self._require_api_key()
        return self._client.request(method, url, params=params, json=json)

    def _raise_for_error(self, response: httpx.Response) -> None:
        _, error_message = _error_payload(response)
        if response.status_code == 401:
            raise RuntimeError("Airtable API error: AIRTABLE_API_KEY is missing or invalid")
        if response.status_code == 403:
            raise RuntimeError(
                "Airtable API error: AIRTABLE_API_KEY lacks access to this base, table, or scope"
            )
        if response.status_code == 404:
            raise RuntimeError("Airtable API error: base, table, view, or record not found")
        try:
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            detail = error_message or exc.response.text
            raise RuntimeError(
                f"Airtable API error: {exc.response.status_code} - {detail}"
            ) from exc

    def _request(
        self,
        method: str,
        url: str,
        *,
        params: dict[str, Any] | list[tuple[str, Any]] | None = None,
        json: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        response = self._send(method, url, params=params, json=json)
        self._raise_for_error(response)
        return response.json()

    def _resolve_url_target(self, url: str) -> dict[str, str | None]:
        parsed = self.parse_url(url)
        if not _is_airtable_url(parsed.get("host")):
            raise RuntimeError(AIRTABLE_URL_MESSAGE)
        if not parsed.get("base_id") or not parsed.get("table_id"):
            raise RuntimeError(AIRTABLE_URL_MESSAGE)
        return parsed

    def _preflight_probe(
        self,
        base_id: str,
        *,
        table: str | None = None,
        view: str | None = None,
    ) -> tuple[str, httpx.Response]:
        if not table:
            return (
                "schema",
                self._send("GET", f"{META_URL}/bases/{_path_part(base_id)}/tables"),
            )
        params: list[tuple[str, Any]] = [("pageSize", 1)]
        if view:
            params.append(("view", view))
        return (
            "records",
            self._send(
                "GET",
                f"{BASE_URL}/{_path_part(base_id)}/{_path_part(table)}",
                params=params,
            ),
        )

    def parse_url(self, url: str) -> dict[str, str | None]:
        """Parse an Airtable URL into app/base, table, view, page, and record IDs."""
        parsed = urlparse(url)
        parts = [part for part in parsed.path.split("/") if part]
        ids: dict[str, str | None] = {
            "base_id": None,
            "table_id": None,
            "view_id": None,
            "page_id": None,
            "record_id": None,
        }
        for part in parts:
            if part.startswith("app"):
                ids["base_id"] = part
            elif part.startswith("tbl"):
                ids["table_id"] = part
            elif part.startswith("viw"):
                ids["view_id"] = part
            elif part.startswith("pag"):
                ids["page_id"] = part
            elif part.startswith("rec"):
                ids["record_id"] = part
        return {
            "url": url,
            "host": parsed.netloc,
            **ids,
        }

    def whoami(self) -> dict[str, Any]:
        """Return the Airtable user/workspace identity for this API key."""
        return self._request("GET", f"{META_URL}/whoami")

    def preflight_access(
        self,
        url: str | None = None,
        base_id: str | None = None,
        table: str | None = None,
        view: str | None = None,
    ) -> dict[str, Any]:
        """Diagnose Airtable auth and read access before doing a larger fetch."""
        if url and any(value is not None for value in (base_id, table, view)):
            return {
                "ok": False,
                "status": "bad_target",
                "message": "Provide either an Airtable URL or base/table arguments, not both.",
                "target": {"url": url, "base_id": base_id, "table": table, "view": view},
                "auth": {"attempted": False, "ok": False, "status": "not_run"},
                "probe": {"attempted": False, "ok": False, "status": "not_run"},
            }

        parsed: dict[str, str | None] | None = None
        if url:
            try:
                parsed = self._resolve_url_target(url)
            except RuntimeError as exc:
                return {
                    "ok": False,
                    "status": "bad_url",
                    "message": str(exc),
                    "target": {
                        "mode": "url",
                        "url": url,
                        "parsed": self.parse_url(url),
                    },
                    "auth": {"attempted": False, "ok": False, "status": "not_run"},
                    "probe": {"attempted": False, "ok": False, "status": "not_run"},
                }
            base_id = parsed["base_id"]
            table = parsed["table_id"]
            view = parsed["view_id"]
        elif not base_id:
            return {
                "ok": False,
                "status": "bad_target",
                "message": "Provide an Airtable URL or at least a base_id for preflight access checks.",
                "target": {"url": url, "base_id": base_id, "table": table, "view": view},
                "auth": {"attempted": False, "ok": False, "status": "not_run"},
                "probe": {"attempted": False, "ok": False, "status": "not_run"},
            }

        target = {
            "mode": "url" if url else "ids",
            "url": url,
            "base_id": base_id,
            "table": table,
            "view": view,
            "parsed": parsed,
        }

        if not self._refresh_api_key():
            return {
                "ok": False,
                "status": "missing_secret",
                "message": AIRTABLE_API_KEY_MISSING_MESSAGE,
                "target": target,
                "auth": {"attempted": False, "ok": False, "status": "missing_secret"},
                "probe": {"attempted": False, "ok": False, "status": "not_run"},
            }

        whoami_response = self._send("GET", f"{META_URL}/whoami")
        auth_error_type, auth_error_message = _error_payload(whoami_response)
        if whoami_response.status_code == 401:
            return {
                "ok": False,
                "status": "invalid_token",
                "message": "Airtable rejected AIRTABLE_API_KEY during the identity check.",
                "target": target,
                "auth": {
                    "attempted": True,
                    "ok": False,
                    "status": "invalid_token",
                    "error_type": auth_error_type,
                    "error_message": auth_error_message,
                },
                "probe": {"attempted": False, "ok": False, "status": "not_run"},
            }
        if whoami_response.is_error:
            return {
                "ok": False,
                "status": "auth_error",
                "message": auth_error_message or "Airtable identity check failed.",
                "target": target,
                "auth": {
                    "attempted": True,
                    "ok": False,
                    "status": "auth_error",
                    "http_status": whoami_response.status_code,
                    "error_type": auth_error_type,
                    "error_message": auth_error_message,
                },
                "probe": {"attempted": False, "ok": False, "status": "not_run"},
            }

        whoami_data = whoami_response.json()
        probe_type, probe_response = self._preflight_probe(base_id, table=table, view=view)
        probe_error_type, probe_error_message = _error_payload(probe_response)
        if probe_response.status_code == 403:
            return {
                "ok": False,
                "status": "missing_base_scope",
                "message": "Airtable auth succeeded, but the token cannot read the requested base or table.",
                "target": target,
                "auth": {
                    "attempted": True,
                    "ok": True,
                    "status": "ok",
                    "identity": _minimal_identity(whoami_data),
                },
                "probe": {
                    "attempted": True,
                    "ok": False,
                    "status": "missing_base_scope",
                    "type": probe_type,
                    "http_status": probe_response.status_code,
                    "error_type": probe_error_type,
                    "error_message": probe_error_message,
                },
            }
        if probe_response.status_code in {400, 404, 422}:
            return {
                "ok": False,
                "status": "bad_url" if url else "bad_target",
                "message": probe_error_message
                or "Airtable could not resolve the requested base, table, or view.",
                "target": target,
                "auth": {
                    "attempted": True,
                    "ok": True,
                    "status": "ok",
                    "identity": _minimal_identity(whoami_data),
                },
                "probe": {
                    "attempted": True,
                    "ok": False,
                    "status": "bad_url" if url else "bad_target",
                    "type": probe_type,
                    "http_status": probe_response.status_code,
                    "error_type": probe_error_type,
                    "error_message": probe_error_message,
                },
            }
        if probe_response.status_code == 401:
            return {
                "ok": False,
                "status": "invalid_token",
                "message": "Airtable rejected AIRTABLE_API_KEY during the read probe.",
                "target": target,
                "auth": {
                    "attempted": True,
                    "ok": True,
                    "status": "ok",
                    "identity": _minimal_identity(whoami_data),
                },
                "probe": {
                    "attempted": True,
                    "ok": False,
                    "status": "invalid_token",
                    "type": probe_type,
                    "http_status": probe_response.status_code,
                    "error_type": probe_error_type,
                    "error_message": probe_error_message,
                },
            }
        if probe_response.is_error:
            return {
                "ok": False,
                "status": "probe_error",
                "message": probe_error_message or "Airtable read probe failed unexpectedly.",
                "target": target,
                "auth": {
                    "attempted": True,
                    "ok": True,
                    "status": "ok",
                    "identity": _minimal_identity(whoami_data),
                },
                "probe": {
                    "attempted": True,
                    "ok": False,
                    "status": "probe_error",
                    "type": probe_type,
                    "http_status": probe_response.status_code,
                    "error_type": probe_error_type,
                    "error_message": probe_error_message,
                },
            }

        probe_data = probe_response.json()
        details: dict[str, Any]
        if probe_type == "schema":
            details = {"table_count": len(probe_data.get("tables", []))}
        else:
            details = {
                "record_count": len(probe_data.get("records", [])),
                "has_more": bool(probe_data.get("offset")),
            }
        return {
            "ok": True,
            "status": "ok",
            "message": "Airtable authentication and read access succeeded.",
            "target": target,
            "auth": {
                "attempted": True,
                "ok": True,
                "status": "ok",
                "identity": _minimal_identity(whoami_data),
            },
            "probe": {
                "attempted": True,
                "ok": True,
                "status": "ok",
                "type": probe_type,
                "details": details,
            },
        }

    def list_bases(self, limit: int = 100) -> list[dict[str, Any]]:
        """List bases visible to AIRTABLE_API_KEY."""
        data = self._request("GET", f"{META_URL}/bases")
        bases = data.get("bases", [])
        return bases[: max(1, min(limit, 1000))]

    def find_bases(self, query: str, limit: int = 20) -> list[dict[str, Any]]:
        """Find visible bases by name or base ID."""
        needle = query.lower()
        matches = [
            base
            for base in self.list_bases(limit=1000)
            if needle in str(base.get("name", "")).lower()
            or needle in str(base.get("id", "")).lower()
        ]
        return matches[: max(1, min(limit, 100))]

    def schema(self, base_id: str) -> dict[str, Any]:
        """Get tables, fields, and views for a base."""
        return self._request("GET", f"{META_URL}/bases/{_path_part(base_id)}/tables")

    def list_tables(self, base_id: str) -> list[dict[str, Any]]:
        """List tables, fields, and views in a base."""
        tables = self.schema(base_id).get("tables", [])
        return [
            {
                "id": table.get("id"),
                "name": table.get("name"),
                "description": table.get("description"),
                "fields": table.get("fields", []),
                "views": table.get("views", []),
            }
            for table in tables
        ]

    def find_tables(self, base_id: str, query: str, limit: int = 20) -> list[dict[str, Any]]:
        """Find tables or views in a base by name or ID."""
        needle = query.lower()
        matches: list[dict[str, Any]] = []
        for table in self.list_tables(base_id):
            table_hit = (
                needle in str(table.get("name", "")).lower()
                or needle in str(table.get("id", "")).lower()
            )
            view_hits = [
                view
                for view in table.get("views", [])
                if needle in str(view.get("name", "")).lower()
                or needle in str(view.get("id", "")).lower()
            ]
            if table_hit or view_hits:
                matches.append({**table, "matching_views": view_hits})
        return matches[: max(1, min(limit, 100))]

    def list_records(
        self,
        base_id: str,
        table: str,
        view: str | None = None,
        max_records: int = 100,
        fields: list[str] | None = None,
        filter_by_formula: str | None = None,
    ) -> dict[str, Any]:
        """List records from a table or view.

        `table` may be a table ID or table name. `view` may be a view ID or view name.
        """
        max_records = max(1, min(max_records, 1000))
        page_size = min(max_records, 100)
        params: list[tuple[str, Any]] = [("pageSize", page_size)]
        if view:
            params.append(("view", view))
        if filter_by_formula:
            params.append(("filterByFormula", filter_by_formula))
        for field in fields or []:
            params.append(("fields[]", field))

        records: list[dict[str, Any]] = []
        offset: str | None = None
        while len(records) < max_records:
            request_params = list(params)
            if offset:
                request_params.append(("offset", offset))
            data = self._request(
                "GET",
                f"{BASE_URL}/{_path_part(base_id)}/{_path_part(table)}",
                params=request_params,
            )
            records.extend(data.get("records", []))
            offset = data.get("offset")
            if not offset:
                break

        return {
            "base_id": base_id,
            "table": table,
            "view": view,
            "count": min(len(records), max_records),
            "records": [_compact_record(record, fields) for record in records[:max_records]],
            "has_more": bool(offset),
        }

    def get_record(self, base_id: str, table: str, record_id: str) -> dict[str, Any]:
        """Get one Airtable record by ID."""
        record = self._request(
            "GET",
            f"{BASE_URL}/{_path_part(base_id)}/{_path_part(table)}/{_path_part(record_id)}",
        )
        return _compact_record(record)

    def records_from_url(
        self,
        url: str,
        max_records: int = 100,
        fields: list[str] | None = None,
    ) -> dict[str, Any]:
        """Read records from an Airtable table/view URL."""
        parsed = self._resolve_url_target(url)
        base_id = parsed.get("base_id")
        table_id = parsed.get("table_id")
        view_id = parsed.get("view_id")
        result = self.list_records(
            base_id=base_id,
            table=table_id,
            view=view_id,
            max_records=max_records,
            fields=fields,
        )
        return {"parsed": parsed, **result}

    def search_records(
        self,
        base_id: str,
        table: str,
        query: str,
        view: str | None = None,
        max_records: int = 200,
    ) -> dict[str, Any]:
        """Search visible record fields by text after fetching records."""
        data = self.list_records(base_id, table, view=view, max_records=max_records)
        needle = query.lower()
        matches = [
            record
            for record in data["records"]
            if any(_match_text(value, needle) for value in record.get("fields", {}).values())
        ]
        return {
            "base_id": base_id,
            "table": table,
            "view": view,
            "query": query,
            "searched": data["count"],
            "count": len(matches),
            "records": matches,
        }

    def create_record(
        self,
        base_id: str,
        table: str,
        fields: dict[str, Any],
        typecast: bool = False,
    ) -> dict[str, Any]:
        """Create one Airtable record.

        `table` may be a table ID or table name. `fields` maps Airtable field names to values.
        The API key needs Airtable's `data.records:write` scope for the target base.
        """
        data = self._request(
            "POST",
            f"{BASE_URL}/{_path_part(base_id)}/{_path_part(table)}",
            json={"fields": fields, "typecast": typecast},
        )
        return _compact_record(data)

    def create_records(
        self,
        base_id: str,
        table: str,
        records: list[dict[str, Any]],
        typecast: bool = False,
    ) -> dict[str, Any]:
        """Create Airtable records in batches of up to 10 records per request."""
        normalized = _normalize_create_records(records)
        created: list[dict[str, Any]] = []
        for batch in _record_batch(normalized):
            data = self._request(
                "POST",
                f"{BASE_URL}/{_path_part(base_id)}/{_path_part(table)}",
                json={"records": batch, "typecast": typecast},
            )
            created.extend(data.get("records", []))
        return {
            "base_id": base_id,
            "table": table,
            "count": len(created),
            "records": _compact_records(created),
        }

    def update_record(
        self,
        base_id: str,
        table: str,
        record_id: str,
        fields: dict[str, Any],
        typecast: bool = False,
        replace: bool = False,
    ) -> dict[str, Any]:
        """Update one Airtable record.

        By default this partially updates fields with PATCH. Set `replace=True` to use PUT.
        """
        method = "PUT" if replace else "PATCH"
        data = self._request(
            method,
            f"{BASE_URL}/{_path_part(base_id)}/{_path_part(table)}/{_path_part(record_id)}",
            json={"fields": fields, "typecast": typecast},
        )
        return _compact_record(data)

    def update_records(
        self,
        base_id: str,
        table: str,
        records: list[dict[str, Any]],
        typecast: bool = False,
        replace: bool = False,
    ) -> dict[str, Any]:
        """Update Airtable records in batches of up to 10 records per request."""
        normalized = _normalize_update_records(records)
        method = "PUT" if replace else "PATCH"
        updated: list[dict[str, Any]] = []
        for batch in _record_batch(normalized):
            data = self._request(
                method,
                f"{BASE_URL}/{_path_part(base_id)}/{_path_part(table)}",
                json={"records": batch, "typecast": typecast},
            )
            updated.extend(data.get("records", []))
        return {
            "base_id": base_id,
            "table": table,
            "count": len(updated),
            "records": _compact_records(updated),
        }

    def upsert_records(
        self,
        base_id: str,
        table: str,
        records: list[dict[str, Any]],
        fields_to_merge_on: list[str],
        typecast: bool = False,
    ) -> dict[str, Any]:
        """Create or update records using Airtable's `performUpsert` merge fields."""
        if not fields_to_merge_on:
            raise ValueError("fields_to_merge_on must include at least one field name.")
        normalized = _normalize_create_records(records)
        upserted: list[dict[str, Any]] = []
        created_record_ids: list[str] = []
        updated_record_ids: list[str] = []
        for batch in _record_batch(normalized):
            data = self._request(
                "PATCH",
                f"{BASE_URL}/{_path_part(base_id)}/{_path_part(table)}",
                json={
                    "records": batch,
                    "typecast": typecast,
                    "performUpsert": {"fieldsToMergeOn": fields_to_merge_on},
                },
            )
            upserted.extend(data.get("records", []))
            created_record_ids.extend(data.get("createdRecords", []))
            updated_record_ids.extend(data.get("updatedRecords", []))
        return {
            "base_id": base_id,
            "table": table,
            "count": len(upserted),
            "records": _compact_records(upserted),
            "createdRecords": created_record_ids,
            "updatedRecords": updated_record_ids,
        }

    def delete_record(self, base_id: str, table: str, record_id: str) -> dict[str, Any]:
        """Delete one Airtable record."""
        return self._request(
            "DELETE",
            f"{BASE_URL}/{_path_part(base_id)}/{_path_part(table)}/{_path_part(record_id)}",
        )

    def delete_records(
        self,
        base_id: str,
        table: str,
        record_ids: list[str],
    ) -> dict[str, Any]:
        """Delete Airtable records in batches of up to 10 record IDs per request."""
        deleted: list[dict[str, Any]] = []
        for batch in _record_id_batch(record_ids):
            params = [("records[]", record_id) for record_id in batch]
            data = self._request(
                "DELETE",
                f"{BASE_URL}/{_path_part(base_id)}/{_path_part(table)}",
                params=params,
            )
            deleted.extend(data.get("records", []))
        return {
            "base_id": base_id,
            "table": table,
            "count": len(deleted),
            "records": deleted,
        }

    def snapshot_from_url(self, url: str, max_records: int = 50) -> dict[str, Any]:
        """Return a compact table-shaped snapshot for an Airtable table/view URL."""
        data = self.records_from_url(url, max_records=max_records)
        records = data["records"]
        field_order: list[str] = []
        for record in records:
            for field in record.get("fields", {}):
                if field not in field_order:
                    field_order.append(field)
        rows = [
            {
                "id": record.get("id"),
                **{field: record.get("fields", {}).get(field) for field in field_order},
            }
            for record in records
        ]
        return {
            "parsed": data["parsed"],
            "base_id": data["base_id"],
            "table": data["table"],
            "view": data["view"],
            "columns": field_order,
            "count": data["count"],
            "rows": rows,
            "has_more": data["has_more"],
        }

    def close(self) -> None:
        self._client.close()


def _client() -> AirtableClient:
    return AirtableClient(allow_missing_api_key=True)
