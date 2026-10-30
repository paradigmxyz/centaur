import httpx
import pytest
from centaur_tool_elfa.client import ElfaClient


@pytest.mark.parametrize(
    ("options", "expected_window"),
    [({}, "24h"), ({"time_window": "7d"}, "7d")],
)
def test_event_summary_request(options, expected_window):
    payload = {
        "success": True,
        "data": [
            {
                "summary": "ETF inflows rose.",
                "tweetIds": ["123"],
                "sourceLinks": ["https://x.com/i/status/123"],
            }
        ],
    }

    def handler(request):
        assert request.method == "GET"
        assert str(request.url).split("?")[0] == "https://api.elfa.ai/v2/data/event-summary"
        assert request.headers["x-elfa-api-key"] == "test-key"
        assert dict(request.url.params) == {"keywords": "ETH", "timeWindow": expected_window}
        return httpx.Response(200, json=payload)

    with ElfaClient(api_key="test-key") as client:
        client._client = httpx.Client(transport=httpx.MockTransport(handler))

        assert client.get_event_summary("ETH", **options) == payload
