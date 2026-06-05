from datetime import date
from pathlib import Path
import sys

import httpx

sys.path.insert(0, str(Path(__file__).parent))

from client import SimilarWebClient, default_app_download_window


def test_default_app_download_window_lags_two_months_for_six_months():
    start, end = default_app_download_window(date(2026, 6, 5))

    assert start == date(2025, 11, 1)
    assert end == date(2026, 4, 1)


def test_app_downloads_uses_v5_endpoint_and_default_window(monkeypatch):
    client = SimilarWebClient(api_key="key")
    seen = {}

    def fake_request(endpoint, params=None, **_kwargs):
        seen["endpoint"] = endpoint
        seen["params"] = params
        return {"downloads": []}

    fixed_date = type(
        "FixedDate",
        (date,),
        {"today": classmethod(lambda cls: date(2026, 6, 5))},
    )
    monkeypatch.setattr("client.date", fixed_date)
    monkeypatch.setattr(client, "_request", fake_request)

    result = client.get_app_downloads("6749636760", store="apple")

    assert result == {"downloads": []}
    assert seen == {
        "endpoint": "/v5/apps/apple/downloads",
        "params": {
            "app_id": "6749636760",
            "country": "world",
            "granularity": "monthly",
            "start_date": "2025-11",
            "end_date": "2026-04",
        },
    }


def test_http_error_uses_response_error_message():
    response = httpx.Response(
        400,
        json={"error_message": "Dates not in range"},
        request=httpx.Request("GET", "https://api.similarweb.com/test"),
    )

    assert SimilarWebClient._error_message(response) == "Dates not in range"
