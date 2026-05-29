"""Composio tool — execute actions from 1000+ services via Composio's cloud API."""

from __future__ import annotations

import logging

from centaur_sdk import secret

log = logging.getLogger(__name__)

# Composio user_id scopes connected accounts. Agents in a shared deployment
# share this default scope; pass a distinct user_id when connected accounts
# must be isolated per user or per thread.
_DEFAULT_USER_ID = "centaur"


def _extract_tools(raw: list) -> list[dict]:
    """Pull name, description, and required params from Composio tool dicts."""
    tools = []
    for t in raw:
        if not isinstance(t, dict):
            continue
        fn = t.get("function", {})
        params = fn.get("parameters", {})
        tools.append({
            "name": fn.get("name", ""),
            "description": fn.get("description", ""),
            "required_params": params.get("required", []),
        })
    return tools


class ComposioClient:
    """Bridge to Composio's tool execution platform."""

    def __init__(self, api_key: str | None = None):
        self._api_key = api_key or secret("COMPOSIO_API_KEY")
        self._composio = None

    def _get_client(self):
        if self._composio is None:
            from composio import Composio

            self._composio = Composio(api_key=self._api_key)
        return self._composio

    def list_tools(self, toolkit: str, user_id: str = _DEFAULT_USER_ID) -> dict:
        """List available tools for a toolkit (e.g. 'github', 'gmail', 'slack', 'notion')."""
        if not toolkit or not toolkit.strip():
            return {"error": "toolkit is required", "successful": False}
        try:
            c = self._get_client()
            raw = c.tools.get(user_id, toolkits=[toolkit.strip()])
            tools = _extract_tools(raw if isinstance(raw, list) else [])
            return {"toolkit": toolkit, "tools": tools, "count": len(tools)}
        except Exception as exc:
            log.warning("composio list_tools failed", exc_info=True)
            return {"error": str(exc), "successful": False}

    def search_tools(self, query: str, user_id: str = _DEFAULT_USER_ID) -> dict:
        """Search for tools across all toolkits by description."""
        if not query or not query.strip():
            return {"error": "query is required", "successful": False}
        try:
            c = self._get_client()
            raw = c.tools.get(user_id, search=query.strip())
            items = raw[:20] if isinstance(raw, list) else []
            tools = _extract_tools(items)
            return {"query": query, "tools": tools, "count": len(tools)}
        except Exception as exc:
            log.warning("composio search_tools failed", exc_info=True)
            return {"error": str(exc), "successful": False}

    def execute(
        self,
        tool_slug: str,
        arguments: dict | None = None,
        user_id: str = _DEFAULT_USER_ID,
    ) -> dict:
        """Execute a Composio tool action.

        tool_slug examples: GITHUB_LIST_REPOS_FOR_USER, HACKERNEWS_GET_TOP_STORIES.
        Use get_tool_schema() to discover required arguments.
        """
        if not tool_slug or not tool_slug.strip():
            return {"error": "tool_slug is required", "successful": False}
        try:
            c = self._get_client()
            # Version check requires per-toolkit version pinning which is
            # impractical when the caller doesn't know the toolkit in advance.
            result = c.tools.execute(
                tool_slug.strip(),
                user_id=user_id,
                arguments=arguments or {},
                dangerously_skip_version_check=True,
            )
            if isinstance(result, dict):
                return {
                    "successful": result.get("successful", False),
                    "error": result.get("error"),
                    "data": result.get("data", {}),
                }
            return {
                "successful": getattr(result, "successful", False),
                "error": getattr(result, "error", None),
                "data": getattr(result, "data", {}),
            }
        except Exception as exc:
            log.warning("composio execute failed", exc_info=True)
            return {"error": str(exc), "successful": False}

    def get_tool_schema(self, tool_slug: str, user_id: str = _DEFAULT_USER_ID) -> dict:
        """Get the input/output schema for a specific tool."""
        if not tool_slug or not tool_slug.strip():
            return {"error": "tool_slug is required", "successful": False}
        try:
            c = self._get_client()
            raw = c.tools.get(user_id, search=tool_slug.strip())
            for t in raw if isinstance(raw, list) else []:
                if not isinstance(t, dict):
                    continue
                fn = t.get("function", {})
                if fn.get("name") == tool_slug.strip():
                    return {
                        "successful": True,
                        "name": fn.get("name"),
                        "description": fn.get("description", ""),
                        "parameters": fn.get("parameters", {}),
                    }
            return {"error": f"Tool {tool_slug} not found", "successful": False}
        except Exception as exc:
            log.warning("composio get_tool_schema failed", exc_info=True)
            return {"error": str(exc), "successful": False}


def _client() -> ComposioClient:
    return ComposioClient()
