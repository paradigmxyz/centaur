"""Unit tests for the HubSpot client without network access."""

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
SPEC = importlib.util.spec_from_file_location("hubspot_client_under_test", MODULE_PATH)
assert SPEC and SPEC.loader
client_module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(client_module)
HubspotClient = client_module.HubspotClient


class HubspotClientTest(unittest.TestCase):
    def response(self, status_code: int, payload: object | None = None) -> MagicMock:
        response = MagicMock(status_code=status_code, text="request failed")
        response.content = b"" if payload is None else b"{}"
        response.json.return_value = payload
        return response

    @patch.object(client_module.httpx, "Client")
    def test_search_contacts_clamps_limit_and_uses_bearer_token(self, http_client: MagicMock) -> None:
        transport = http_client.return_value
        transport.request.return_value = self.response(200, {"results": [{"id": "42"}]})
        client = HubspotClient(access_token="token")

        result = client.search_contacts("Ronaldo", limit=200)

        self.assertEqual({"results": [{"id": "42"}]}, result)
        http_client.assert_called_once_with(
            base_url="https://api.hubapi.com",
            headers={"Authorization": "Bearer token", "Content-Type": "application/json"},
            timeout=30.0,
        )
        transport.request.assert_called_once_with(
            "POST",
            "/crm/v3/objects/contacts/search",
            json={
                "query": "Ronaldo",
                "limit": 100,
                "properties": ["email", "firstname", "lastname", "company"],
            },
        )

    def test_get_contact_and_raw_request_delegate_to_http(self) -> None:
        transport = MagicMock()
        transport.request.side_effect = [
            self.response(200, {"id": "42"}),
            self.response(200, {"ok": True}),
        ]
        client = HubspotClient(access_token="token")
        client._client = transport

        self.assertEqual({"id": "42"}, client.get_contact("42", properties=["email", "company"]))
        self.assertEqual(
            {"ok": True},
            client.raw_request("post", "crm/v3/example", json={"name": "example"}),
        )
        self.assertEqual(
            [
                unittest.mock.call(
                    "GET", "/crm/v3/objects/contacts/42", params={"properties": "email,company"}
                ),
                unittest.mock.call("POST", "/crm/v3/example", json={"name": "example"}),
            ],
            transport.request.call_args_list,
        )

    def test_api_error_includes_hubspot_message(self) -> None:
        transport = MagicMock()
        transport.request.return_value = self.response(401, {"message": "expired token"})
        client = HubspotClient(access_token="token")
        client._client = transport

        with self.assertRaisesRegex(RuntimeError, r"HubSpot API error \(401\): expired token"):
            client.account_info()


if __name__ == "__main__":
    unittest.main()
