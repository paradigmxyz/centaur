"""Notion REST API client."""

from typing import Any

import httpx

from centaur_sdk import secret

API_BASE = "https://api.notion.com/v1"
NOTION_VERSION = "2022-06-28"


class NotionClient:

    """Client for Notion's REST API."""

    def __init__(self, api_key: str | None = None):
        self.api_key = api_key or secret("NOTION_API_KEY", "")
        if not self.api_key:
            raise RuntimeError(
                "NOTION_API_KEY not set.\nGet one at https://www.notion.so/my-integrations"
            )
        self._http = httpx.Client(
            base_url=API_BASE,
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
                "Notion-Version": NOTION_VERSION,
            },
            timeout=30.0,
        )

    def _request(
        self,
        method: str,
        path: str,
        json: dict[str, Any] | None = None,
        params: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Execute an HTTP request."""
        resp = self._http.request(method, path, json=json, params=params)
        if resp.is_error:
            # Notion's error body names the offending property or value.
            try:
                message = resp.json().get("message")
            except (ValueError, AttributeError):
                message = None
            if message:
                raise httpx.HTTPStatusError(
                    f"Notion API error {resp.status_code}: {message}",
                    request=resp.request,
                    response=resp,
                )
        resp.raise_for_status()
        return resp.json()

    def _get(self, path: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        return self._request("GET", path, params=params)

    def _post(self, path: str, json: dict[str, Any] | None = None) -> dict[str, Any]:
        return self._request("POST", path, json=json)

    def _patch(self, path: str, json: dict[str, Any] | None = None) -> dict[str, Any]:
        return self._request("PATCH", path, json=json)

    def _delete(self, path: str) -> dict[str, Any]:
        return self._request("DELETE", path)

    # -------------------------------------------------------------------------
    # Search
    # -------------------------------------------------------------------------

    def search(
        self,
        query: str | None = None,
        filter_type: str | None = None,
        sort_direction: str = "descending",
        sort_timestamp: str = "last_edited_time",
        page_size: int = 100,
        start_cursor: str | None = None,
    ) -> dict[str, Any]:
        """Search pages and databases by title.

        Args:
            query: Text to search for in titles
            filter_type: 'page' or 'database' to filter results
            sort_direction: 'ascending' or 'descending'
            sort_timestamp: 'last_edited_time'
            page_size: Results per page (max 100)
            start_cursor: Pagination cursor
        """
        body: dict[str, Any] = {"page_size": page_size}
        if query:
            body["query"] = query
        if filter_type:
            body["filter"] = {"property": "object", "value": filter_type}
        body["sort"] = {"direction": sort_direction, "timestamp": sort_timestamp}
        if start_cursor:
            body["start_cursor"] = start_cursor
        return self._post("/search", json=body)

    # -------------------------------------------------------------------------
    # Users
    # -------------------------------------------------------------------------

    def me(self) -> dict[str, Any]:
        """Get the bot user."""
        return self._get("/users/me")

    def users(self, page_size: int = 100, start_cursor: str | None = None) -> dict[str, Any]:
        """List all users in the workspace."""
        params: dict[str, Any] = {"page_size": page_size}
        if start_cursor:
            params["start_cursor"] = start_cursor
        return self._get("/users", params=params)

    def user(self, user_id: str) -> dict[str, Any]:
        """Retrieve a user by ID."""
        return self._get(f"/users/{user_id}")

    # -------------------------------------------------------------------------
    # Databases
    # -------------------------------------------------------------------------

    def database(self, database_id: str) -> dict[str, Any]:
        """Retrieve a database."""
        return self._get(f"/databases/{database_id}")

    def query_database(
        self,
        database_id: str,
        filter: dict[str, Any] | None = None,
        sorts: list[dict[str, Any]] | None = None,
        page_size: int = 100,
        start_cursor: str | None = None,
    ) -> dict[str, Any]:
        """Query a database.

        Args:
            database_id: Database ID
            filter: Filter object (see Notion docs)
            sorts: Sort objects (see Notion docs)
            page_size: Results per page (max 100)
            start_cursor: Pagination cursor
        """
        body: dict[str, Any] = {"page_size": page_size}
        if filter:
            body["filter"] = filter
        if sorts:
            body["sorts"] = sorts
        if start_cursor:
            body["start_cursor"] = start_cursor
        return self._post(f"/databases/{database_id}/query", json=body)

    def create_database(
        self,
        parent_page_id: str,
        title: str,
        properties: dict[str, Any],
        is_inline: bool = False,
    ) -> dict[str, Any]:
        """Create a database.

        Args:
            parent_page_id: Parent page ID
            title: Database title
            properties: Property schema (see Notion docs)
            is_inline: Whether to create inline database
        """
        body: dict[str, Any] = {
            "parent": {"type": "page_id", "page_id": parent_page_id},
            "title": [{"type": "text", "text": {"content": title}}],
            "properties": properties,
            "is_inline": is_inline,
        }
        return self._post("/databases", json=body)

    def update_database(
        self,
        database_id: str,
        title: str | None = None,
        properties: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Update a database."""
        body: dict[str, Any] = {}
        if title:
            body["title"] = [{"type": "text", "text": {"content": title}}]
        if properties:
            body["properties"] = properties
        return self._patch(f"/databases/{database_id}", json=body)

    # -------------------------------------------------------------------------
    # Pages
    # -------------------------------------------------------------------------

    def page(self, page_id: str) -> dict[str, Any]:
        """Retrieve a page."""
        return self._get(f"/pages/{page_id}")

    def page_property(
        self,
        page_id: str,
        property_id: str,
        page_size: int = 100,
        start_cursor: str | None = None,
    ) -> dict[str, Any]:
        """Retrieve a page property item."""
        params: dict[str, Any] = {"page_size": page_size}
        if start_cursor:
            params["start_cursor"] = start_cursor
        return self._get(f"/pages/{page_id}/properties/{property_id}", params=params)

    def create_page(
        self,
        parent: dict[str, Any],
        properties: dict[str, Any],
        children: list[dict[str, Any]] | None = None,
        icon: dict[str, Any] | None = None,
        cover: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Create a page.

        Args:
            parent: Parent object (database_id or page_id)
            properties: Page properties
            children: Initial block children
            icon: Page icon
            cover: Page cover
        """
        body: dict[str, Any] = {"parent": parent, "properties": properties}
        if children:
            body["children"] = children
        if icon:
            body["icon"] = icon
        if cover:
            body["cover"] = cover
        return self._post("/pages", json=body)

    def update_page(
        self,
        page_id: str,
        properties: dict[str, Any] | None = None,
        archived: bool | None = None,
        icon: dict[str, Any] | None = None,
        cover: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Update a page."""
        body: dict[str, Any] = {}
        if properties:
            body["properties"] = properties
        if archived is not None:
            body["archived"] = archived
        if icon:
            body["icon"] = icon
        if cover:
            body["cover"] = cover
        return self._patch(f"/pages/{page_id}", json=body)

    def archive_page(self, page_id: str) -> dict[str, Any]:
        """Archive (trash) a page."""
        return self.update_page(page_id, archived=True)

    def restore_page(self, page_id: str) -> dict[str, Any]:
        """Restore a page from trash."""
        return self.update_page(page_id, archived=False)

    # -------------------------------------------------------------------------
    # Blocks
    # -------------------------------------------------------------------------

    def block(self, block_id: str) -> dict[str, Any]:
        """Retrieve a block."""
        return self._get(f"/blocks/{block_id}")

    def block_children(
        self,
        block_id: str,
        page_size: int = 100,
        start_cursor: str | None = None,
    ) -> dict[str, Any]:
        """Retrieve block children (page content)."""
        params: dict[str, Any] = {"page_size": page_size}
        if start_cursor:
            params["start_cursor"] = start_cursor
        return self._get(f"/blocks/{block_id}/children", params=params)

    def append_block_children(
        self,
        block_id: str,
        children: list[dict[str, Any]],
        after: str | None = None,
    ) -> dict[str, Any]:
        """Append blocks to a page or block.

        Args:
            block_id: Parent block/page ID
            children: Block objects to append
            after: Block ID to insert after
        """
        body: dict[str, Any] = {"children": children}
        if after:
            body["after"] = after
        return self._patch(f"/blocks/{block_id}/children", json=body)

    def update_block(
        self,
        block_id: str,
        block_data: dict[str, Any],
        archived: bool | None = None,
    ) -> dict[str, Any]:
        """Update a block."""
        body = block_data.copy()
        if archived is not None:
            body["archived"] = archived
        return self._patch(f"/blocks/{block_id}", json=body)

    def delete_block(self, block_id: str) -> dict[str, Any]:
        """Delete (archive) a block."""
        return self._delete(f"/blocks/{block_id}")

    # -------------------------------------------------------------------------
    # Comments
    # -------------------------------------------------------------------------

    def comments(
        self,
        block_id: str | None = None,
        page_size: int = 100,
        start_cursor: str | None = None,
    ) -> dict[str, Any]:
        """Retrieve comments on a block or page."""
        params: dict[str, Any] = {"page_size": page_size}
        if block_id:
            params["block_id"] = block_id
        if start_cursor:
            params["start_cursor"] = start_cursor
        return self._get("/comments", params=params)

    def create_comment(
        self,
        parent: dict[str, Any],
        rich_text: list[dict[str, Any]],
    ) -> dict[str, Any]:
        """Create a comment.

        Args:
            parent: {"page_id": "..."} or {"discussion_id": "..."}
            rich_text: Comment content as rich text
        """
        body = {"parent": parent, "rich_text": rich_text}
        return self._post("/comments", json=body)

    # -------------------------------------------------------------------------
    # Helpers
    # -------------------------------------------------------------------------

    def get_all_pages(
        self,
        database_id: str,
        filter: dict[str, Any] | None = None,
        sorts: list[dict[str, Any]] | None = None,
    ) -> list[dict[str, Any]]:
        """Fetch all pages from a database (handles pagination)."""
        results = []
        cursor = None
        while True:
            resp = self.query_database(database_id, filter=filter, sorts=sorts, start_cursor=cursor)
            results.extend(resp.get("results", []))
            if not resp.get("has_more"):
                break
            cursor = resp.get("next_cursor")
        return results

    def get_page_content(self, page_id: str) -> list[dict[str, Any]]:
        """Fetch all block children of a page (handles pagination)."""
        results = []
        cursor = None
        while True:
            resp = self.block_children(page_id, start_cursor=cursor)
            results.extend(resp.get("results", []))
            if not resp.get("has_more"):
                break
            cursor = resp.get("next_cursor")
        return results

    @staticmethod
    def extract_title(page_or_db: dict[str, Any]) -> str:
        """Extract plain text title from a page or database object."""
        props = page_or_db.get("properties", {})

        # Database title
        if page_or_db.get("object") == "database":
            title_arr = page_or_db.get("title", [])
            return "".join(t.get("plain_text", "") for t in title_arr)

        # Page title - find the title property
        for prop in props.values():
            if prop.get("type") == "title":
                title_arr = prop.get("title", [])
                return "".join(t.get("plain_text", "") for t in title_arr)

        return ""

    @staticmethod
    def extract_rich_text(rich_text: list[dict[str, Any]]) -> str:
        """Extract plain text from rich text array."""
        return "".join(t.get("plain_text", "") for t in rich_text)

    @staticmethod
    def make_rich_text(text: str) -> list[dict[str, Any]]:
        """Create a simple rich text array from plain text."""
        return [{"type": "text", "text": {"content": text}}]

    @staticmethod
    def make_paragraph_block(text: str) -> dict[str, Any]:
        """Create a paragraph block."""
        return {
            "object": "block",
            "type": "paragraph",
            "paragraph": {"rich_text": NotionClient.make_rich_text(text)},
        }

    @staticmethod
    def make_heading_block(text: str, level: int = 1) -> dict[str, Any]:
        """Create a heading block (level 1, 2, or 3)."""
        heading_type = f"heading_{level}"
        return {
            "object": "block",
            "type": heading_type,
            heading_type: {"rich_text": NotionClient.make_rich_text(text)},
        }

    @staticmethod
    def make_todo_block(text: str, checked: bool = False) -> dict[str, Any]:
        """Create a to-do block."""
        return {
            "object": "block",
            "type": "to_do",
            "to_do": {
                "rich_text": NotionClient.make_rich_text(text),
                "checked": checked,
            },
        }

    @staticmethod
    def make_bullet_block(text: str) -> dict[str, Any]:
        """Create a bulleted list item block."""
        return {
            "object": "block",
            "type": "bulleted_list_item",
            "bulleted_list_item": {"rich_text": NotionClient.make_rich_text(text)},
        }



def _client() -> NotionClient:
    return NotionClient(api_key=secret("NOTION_API_KEY", ""))
