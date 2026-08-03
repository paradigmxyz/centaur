"""Unit tests for the Microsoft Graph client without network access."""

from __future__ import annotations

import importlib.util
import sys
import types
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch


sys.modules.setdefault("centaur_sdk", types.SimpleNamespace(secret=lambda _name, default: default))
sys.modules.setdefault("httpx", types.SimpleNamespace(Client=MagicMock))
MODULE_PATH = Path(__file__).parents[1] / "client.py"
SPEC = importlib.util.spec_from_file_location("microsoft_graph_client_under_test", MODULE_PATH)
assert SPEC and SPEC.loader
client_module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(client_module)
MicrosoftGraphClient = client_module.MicrosoftGraphClient


class MicrosoftGraphClientTest(unittest.TestCase):
    def response(self, status_code: int, payload: object | None = None) -> MagicMock:
        response = MagicMock(status_code=status_code, text="request failed")
        response.content = b"" if payload is None else b"{}"
        response.json.return_value = payload
        return response

    @patch.object(client_module.httpx, "Client")
    def test_initializes_http_client_and_lists_search_results(self, http_client: MagicMock) -> None:
        transport = http_client.return_value
        transport.request.return_value = self.response(200, {"value": [{"id": "mail-1"}]})
        client = MicrosoftGraphClient(access_token="token")

        result = client.list_messages(top=99, search="quarterly report", select=["id", "subject"])

        self.assertEqual({"value": [{"id": "mail-1"}]}, result)
        http_client.assert_called_once_with(
            base_url="https://graph.microsoft.com/v1.0",
            headers={"Authorization": "Bearer token", "Content-Type": "application/json"},
            timeout=30.0,
        )
        transport.request.assert_called_once_with(
            "GET",
            "/me/messages",
            params={"$top": 50, "$search": '"quarterly report"', "$select": "id,subject"},
            headers={"ConsistencyLevel": "eventual"},
        )

    def test_list_events_and_raw_request_normalize_paths(self) -> None:
        transport = MagicMock()
        transport.request.side_effect = [
            self.response(200, {"value": []}),
            self.response(200, {"id": "user-1"}),
        ]
        client = MicrosoftGraphClient(access_token="token")
        client._client = transport

        self.assertEqual({"value": []}, client.list_events(top=0))
        self.assertEqual({"id": "user-1"}, client.raw_request("get", "/v1.0/me"))
        self.assertEqual(
            [
                unittest.mock.call(
                    "GET",
                    "/me/events",
                    params={"$top": 1, "$select": "id,subject,start,end,organizer"},
                ),
                unittest.mock.call("GET", "/me"),
            ],
            transport.request.call_args_list,
        )

    def test_api_error_includes_graph_error_message(self) -> None:
        transport = MagicMock()
        transport.request.return_value = self.response(403, {"error": {"message": "Access denied"}})
        client = MicrosoftGraphClient(access_token="token")
        client._client = transport

        with self.assertRaisesRegex(RuntimeError, r"Microsoft Graph error \(403\): Access denied"):
            client.me()


if __name__ == "__main__":
    unittest.main()
