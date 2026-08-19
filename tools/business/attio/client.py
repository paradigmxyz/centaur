"""Attio API client."""

import mimetypes
import time
from datetime import UTC, datetime
from email.utils import parsedate_to_datetime
from pathlib import Path
from typing import Any

import httpx

from centaur_sdk import secret


class AttioClient:
    """Authenticated Attio CRM API client."""

    def __init__(self, api_key: str | None = None, max_rate_limit_retries: int = 2):
        self._api_key_override = api_key
        self._client: httpx.Client | None = None
        self.max_rate_limit_retries = max_rate_limit_retries

    def _http(self) -> httpx.Client:
        """Return the cached HTTP client, building it after secrets are injected."""
        if self._client is not None:
            return self._client

        api_key = self._api_key_override or secret("ATTIO_API_KEY", "")
        if not api_key:
            raise RuntimeError(
                "ATTIO_API_KEY not set.\nGenerate one at https://app.attio.com/settings/developers"
            )
        self._client = httpx.Client(
            base_url="https://api.attio.com/v2",
            headers={
                "Authorization": f"Bearer {api_key}",
            },
            timeout=30.0,
        )
        return self._client

    def _request(self, method: str, path: str, **kwargs) -> dict[str, Any]:
        """Make authenticated request to Attio API."""
        for attempt in range(self.max_rate_limit_retries + 1):
            response = self._http().request(method, path, **kwargs)
            if response.status_code != 429 or attempt == self.max_rate_limit_retries:
                break
            retry_after = response.headers.get("Retry-After", "1")
            time.sleep(self._retry_delay(retry_after))
        if response.status_code >= 400:
            try:
                error = response.json()
                msg = error.get("message", response.text)
            except Exception:
                msg = response.text
            raise RuntimeError(f"Attio API error ({response.status_code}): {msg}")
        return response.json()

    def _retry_delay(self, retry_after: str, now: datetime | None = None) -> float:
        """Parse Retry-After as delta seconds or an HTTP date, bounded to one minute."""
        try:
            delay = float(retry_after)
        except ValueError:
            try:
                retry_at = parsedate_to_datetime(retry_after)
                if retry_at.tzinfo is None:
                    retry_at = retry_at.replace(tzinfo=UTC)
                delay = (retry_at - (now or datetime.now(UTC))).total_seconds()
            except (TypeError, ValueError, OverflowError):
                delay = 1.0
        return min(max(delay, 0.0), 60.0)

    def _clean_params(self, params: dict[str, Any]) -> dict[str, Any]:
        """Remove unset values and encode list query params the way Attio expects."""
        cleaned: dict[str, Any] = {}
        for key, value in params.items():
            if value is None:
                continue
            if isinstance(value, list):
                cleaned[key] = ",".join(str(item) for item in value)
                continue
            cleaned[key] = value
        return cleaned

    def raw_request(
        self,
        method: str,
        endpoint: str,
        json: dict[str, Any] | None = None,
        params: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Make a raw request to the Attio REST API.

        The endpoint may be either a full `/v2/...` path or a path relative to `/v2`.
        """
        normalized_endpoint = endpoint if endpoint.startswith("/") else f"/{endpoint}"
        if normalized_endpoint.startswith("/v2"):
            normalized_endpoint = normalized_endpoint[len("/v2") :]

        kwargs: dict[str, Any] = {}
        if json is not None:
            kwargs["json"] = json
        if params is not None:
            kwargs["params"] = params
        return self._request(method.upper(), normalized_endpoint, **kwargs)

    def list_objects(self) -> list[dict]:
        """List all objects in workspace."""
        data = self._request("GET", "/objects")
        return data.get("data", [])

    def get_object(self, object_slug: str) -> dict:
        """Get object by slug or ID."""
        data = self._request("GET", f"/objects/{object_slug}")
        return data.get("data", {})

    def list_attributes(self, object_slug: str) -> list[dict]:
        """List attributes for an object."""
        data = self._request("GET", f"/objects/{object_slug}/attributes")
        return data.get("data", [])

    def query_records(
        self,
        object_slug: str,
        filter_obj: dict | None = None,
        sorts: list[dict] | None = None,
        limit: int = 25,
        offset: int = 0,
    ) -> list[dict]:
        """Query records for an object with optional filtering.

        IMPORTANT — filter_obj format:
        filter_obj is placed directly into the request body as ``{"filter": filter_obj}``,
        so do NOT nest an extra "filter" key inside it.

        Keys must be real Attio attribute slugs (e.g. "name", "domains",
        "email_addresses"). If unsure which slugs exist, call list_attributes()
        first.

        Each attribute value is an object whose keys depend on the attribute type:
          - text/domain/email: {"value": "exact match"}
          - number:            {"gt": 10} | {"lt": 5} | {"gte": 1, "lte": 100}
          - select:            {"value": "option-slug"}
          - record-reference:  {"target_object": "companies", "target_record_id": "..."}
          - checkbox:          {"value": true}

        Compound filters use "$and" / "$or" at the top level:
          {"$or": [{"name": {"value": "Acme"}}, {"domains": {"value": "acme.com"}}]}

        Examples:
          filter_obj={"name": {"value": "Acme Inc"}}
          filter_obj={"domains": {"domain": "acme.com"}}
          filter_obj={"email_addresses": {"email_address": "j@example.com"}}

        For multi-select attributes, the value must be a list:
          {"tags": [{"value": "partner"}, {"value": "active"}]}
        """
        body: dict[str, Any] = {"limit": limit, "offset": offset}
        if filter_obj:
            body["filter"] = filter_obj
        if sorts:
            body["sorts"] = sorts
        data = self._request("POST", f"/objects/{object_slug}/records/query", json=body)
        return data.get("data", [])

    def query_all_records(
        self,
        object_slug: str,
        filter_obj: dict | None = None,
        sorts: list[dict] | None = None,
        page_size: int = 500,
    ) -> list[dict]:
        """Query every matching record using Attio's limit/offset pagination."""
        if not 1 <= page_size <= 500:
            raise ValueError("page_size must be between 1 and 500")
        records: list[dict] = []
        offset = 0
        while True:
            page = self.query_records(
                object_slug,
                filter_obj=filter_obj,
                sorts=sorts,
                limit=page_size,
                offset=offset,
            )
            records.extend(page)
            if len(page) < page_size:
                return records
            offset += page_size

    def get_record(self, object_slug: str, record_id: str) -> dict:
        """Get a specific record by ID."""
        data = self._request("GET", f"/objects/{object_slug}/records/{record_id}")
        return data.get("data", {})

    def create_record(self, object_slug: str, values: dict) -> dict:
        """Create a new record.

        values format — keys are attribute slugs, values are lists of typed objects:
          {"name": [{"first_name": "Jane", "last_name": "Doe"}]}
          {"name": [{"value": "Acme Inc"}], "domains": [{"domain": "acme.com"}]}

        Multi-select attributes MUST be arrays:
          {"channel_source": [{"option": "inbound"}, {"option": "referral"}]}

        Call list_attributes(object_slug) first to discover required fields and
        attribute types so you don't omit required values or pass wrong types.
        """
        body = {"data": {"values": values}}
        data = self._request("POST", f"/objects/{object_slug}/records", json=body)
        return data.get("data", {})

    def update_record(self, object_slug: str, record_id: str, values: dict) -> dict:
        """Update a record, appending rather than replacing multiselect values.

        values format is the same as create_record — attribute slugs mapping to
        lists of typed value objects.
        """
        body = {"data": {"values": values}}
        data = self._request("PATCH", f"/objects/{object_slug}/records/{record_id}", json=body)
        return self._validated_record_response(data, record_id)

    def replace_record_values(self, object_slug: str, record_id: str, values: dict) -> dict:
        """Replace the supplied record attribute values instead of appending them."""
        body = {"data": {"values": values}}
        data = self._request("PUT", f"/objects/{object_slug}/records/{record_id}", json=body)
        return self._validated_record_response(data, record_id)

    def _validated_record_response(self, response: dict[str, Any], record_id: str) -> dict:
        """Require Attio to return the record that was mutated."""
        record = response.get("data", {})
        returned_id = record.get("id", {}).get("record_id")
        if returned_id != record_id:
            raise RuntimeError(
                f"Attio mutation response did not confirm record {record_id}; got {returned_id!r}"
            )
        return record

    def delete_record(self, object_slug: str, record_id: str) -> bool:
        """Delete a record."""
        self._request("DELETE", f"/objects/{object_slug}/records/{record_id}")
        return True

    def assert_record(self, object_slug: str, matching_attribute: str, values: dict) -> dict:
        """Create or update a record based on matching attribute."""
        body = {"data": {"values": values}}
        data = self._request(
            "PUT",
            f"/objects/{object_slug}/records",
            params={"matching_attribute": matching_attribute},
            json=body,
        )
        return data.get("data", {})

    def list_lists(self) -> list[dict]:
        """List all lists in workspace."""
        data = self._request("GET", "/lists")
        return data.get("data", [])

    def get_list(self, list_id: str) -> dict:
        """Get list by ID or slug."""
        data = self._request("GET", f"/lists/{list_id}")
        return data.get("data", {})

    def query_entries(
        self,
        list_id: str,
        filter_obj: dict | None = None,
        limit: int = 25,
        offset: int = 0,
    ) -> list[dict]:
        """Query entries in a list.

        filter_obj uses the same format as query_records — keys are attribute
        slugs (not "filter"), values are typed condition objects. Do NOT wrap
        filter_obj in an extra "filter" key; the method does that automatically.
        """
        body: dict[str, Any] = {"limit": limit, "offset": offset}
        if filter_obj:
            body["filter"] = filter_obj
        data = self._request("POST", f"/lists/{list_id}/entries/query", json=body)
        return data.get("data", [])

    def create_entry(self, list_id: str, parent_record_id: str, values: dict | None = None) -> dict:
        """Create a new entry in a list."""
        body: dict[str, Any] = {"data": {"parent_record_id": parent_record_id}}
        if values:
            body["data"]["entry_values"] = values
        data = self._request("POST", f"/lists/{list_id}/entries", json=body)
        return data.get("data", {})

    def list_notes(
        self,
        parent_object: str,
        parent_record_id: str,
        limit: int = 50,
        offset: int = 0,
    ) -> list[dict]:
        """List notes for a record."""
        data = self._request(
            "GET",
            "/notes",
            params={
                "parent_object": parent_object,
                "parent_record_id": parent_record_id,
                "limit": limit,
                "offset": offset,
            },
        )
        return data.get("data", [])

    def list_all_notes(
        self, parent_object: str, parent_record_id: str, page_size: int = 50
    ) -> list[dict]:
        """List every note for a record using Attio's limit/offset pagination."""
        if not 1 <= page_size <= 50:
            raise ValueError("page_size must be between 1 and 50")
        notes: list[dict] = []
        offset = 0
        while True:
            page = self.list_notes(
                parent_object,
                parent_record_id,
                limit=page_size,
                offset=offset,
            )
            notes.extend(page)
            if len(page) < page_size:
                return notes
            offset += page_size

    def upload_file(
        self,
        object_slug: str,
        record_id: str,
        file_path: str,
        parent_folder_id: str | None = None,
    ) -> dict:
        """Upload a local file to an Attio record's native file storage."""
        path = Path(file_path)
        if not path.is_file():
            raise ValueError(f"File does not exist or is not a regular file: {file_path}")

        max_size = 50 * 1024 * 1024
        if path.stat().st_size > max_size:
            raise ValueError("Attio files must be 50 MB or smaller")

        form = {"object": object_slug, "record_id": record_id}
        if parent_folder_id:
            form["parent_folder_id"] = parent_folder_id

        content_type = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
        with path.open("rb") as file_handle:
            data = self._request(
                "POST",
                "/files/upload",
                data=form,
                files={"file": (path.name, file_handle, content_type)},
            )
        return data.get("data", {})

    def list_threads(
        self,
        object_slug: str | None = None,
        record_id: str | None = None,
        list_id: str | None = None,
        entry_id: str | None = None,
        limit: int = 10,
        offset: int = 0,
    ) -> list[dict]:
        """List comment threads for a record or list entry."""
        params = self._clean_params(
            {
                "object": object_slug,
                "record_id": record_id,
                "list": list_id,
                "entry_id": entry_id,
                "limit": limit,
                "offset": offset,
            }
        )
        data = self._request("GET", "/threads", params=params)
        return data.get("data", [])

    def get_thread(self, thread_id: str) -> dict:
        """Get a thread and its comments by thread ID."""
        data = self._request("GET", f"/threads/{thread_id}")
        return data.get("data", {})

    def list_meetings(
        self,
        limit: int = 50,
        cursor: str | None = None,
        linked_object: str | None = None,
        linked_record_id: str | None = None,
        participants: list[str] | str | None = None,
        sort: str | None = None,
        ends_from: str | None = None,
        starts_before: str | None = None,
        timezone: str | None = None,
    ) -> dict[str, Any]:
        """List meetings with optional filtering and cursor pagination."""
        params = self._clean_params(
            {
                "limit": limit,
                "cursor": cursor,
                "linked_object": linked_object,
                "linked_record_id": linked_record_id,
                "participants": participants,
                "sort": sort,
                "ends_from": ends_from,
                "starts_before": starts_before,
                "timezone": timezone,
            }
        )
        return self._request("GET", "/meetings", params=params)

    def get_meeting(self, meeting_id: str) -> dict:
        """Get a single meeting by ID."""
        data = self._request("GET", f"/meetings/{meeting_id}")
        return data.get("data", {})

    def list_call_recordings(
        self,
        meeting_id: str,
        limit: int = 50,
        cursor: str | None = None,
    ) -> dict[str, Any]:
        """List call recordings for a meeting."""
        params = self._clean_params({"limit": limit, "cursor": cursor})
        return self._request("GET", f"/meetings/{meeting_id}/call_recordings", params=params)

    def get_call_transcript(
        self,
        meeting_id: str,
        call_recording_id: str,
        cursor: str | None = None,
    ) -> dict[str, Any]:
        """Get the transcript for a call recording."""
        params = self._clean_params({"cursor": cursor})
        return self._request(
            "GET",
            f"/meetings/{meeting_id}/call_recordings/{call_recording_id}/transcript",
            params=params,
        )

    def create_note(
        self, parent_object: str, parent_record_id: str, title: str, content: str
    ) -> dict:
        """Create a note for a record."""
        body = {
            "data": {
                "parent_object": parent_object,
                "parent_record_id": parent_record_id,
                "title": title,
                "format": "plaintext",
                "content": content,
            }
        }
        data = self._request("POST", "/notes", json=body)
        return data.get("data", {})

    def list_tasks(
        self,
        linked_object: str | None = None,
        linked_record_id: str | None = None,
        is_completed: bool | None = None,
        limit: int = 25,
    ) -> list[dict]:
        """List tasks with optional filters."""
        params: dict[str, Any] = {"limit": limit}
        if linked_object:
            params["linked_object"] = linked_object
        if linked_record_id:
            params["linked_record_id"] = linked_record_id
        if is_completed is not None:
            params["is_completed"] = str(is_completed).lower()
        data = self._request("GET", "/tasks", params=params)
        return data.get("data", [])

    def create_task(
        self,
        content: str,
        deadline: str | None = None,
        assignees: list[str] | None = None,
        linked_records: list[dict] | None = None,
    ) -> dict:
        """Create a new task."""
        body: dict[str, Any] = {
            "data": {
                "content": content,
                "format": "plaintext",
            }
        }
        if deadline:
            body["data"]["deadline_at"] = deadline
        if assignees:
            body["data"]["assignees"] = [{"workspace_member_id": a} for a in assignees]
        if linked_records:
            body["data"]["linked_records"] = linked_records
        data = self._request("POST", "/tasks", json=body)
        return data.get("data", {})

    def list_workspace_members(self) -> list[dict]:
        """List workspace members."""
        data = self._request("GET", "/workspace_members")
        return data.get("data", [])

    def get_self(self) -> dict:
        """Get info about the current API token."""
        data = self._request("GET", "/self")
        return data.get("data", {})

    def close(self):
        """Close the underlying HTTP client."""
        if self._client is not None:
            self._client.close()


def _client() -> AttioClient:
    """Factory for tool SDK integration."""
    return AttioClient()
