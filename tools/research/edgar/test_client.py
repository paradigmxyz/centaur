"""Tests for the EDGAR client."""

from __future__ import annotations

import httpx
import pytest
from centaur_tool_edgar import client as edgar_client
from centaur_tool_edgar.client import EdgarClient, html_to_text, normalize_accession

TICKERS = {
    "0": {"cik_str": 320193, "ticker": "AAPL", "title": "Apple Inc."},
    "1": {"cik_str": 1067983, "ticker": "BRK-B", "title": "BERKSHIRE HATHAWAY INC"},
    "2": {"cik_str": 1418091, "ticker": "TWTR", "title": "Twitter, Inc."},
}


@pytest.fixture(autouse=True)
def no_sleep(monkeypatch):
    monkeypatch.setattr(edgar_client.time, "sleep", lambda _seconds: None)


def _make(handler, user_agent: str | None = "Test Agent test@example.com") -> EdgarClient:
    client = EdgarClient(user_agent=user_agent)
    client._client = httpx.Client(transport=httpx.MockTransport(handler))
    return client


def _columns(rows: list[dict]) -> dict[str, list]:
    keys = rows[0].keys()
    return {key: [row[key] for row in rows] for key in keys}


def test_user_agent_defaults_and_env_override(monkeypatch):
    monkeypatch.delenv("EDGAR_USER_AGENT", raising=False)
    assert EdgarClient().user_agent == edgar_client.DEFAULT_USER_AGENT
    monkeypatch.setenv("EDGAR_USER_AGENT", "Acme Research ops@acme.example")
    assert EdgarClient().user_agent == "Acme Research ops@acme.example"
    assert EdgarClient(user_agent="explicit").user_agent == "explicit"


def test_requests_send_user_agent_and_resolve_tickers():
    seen = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        assert request.headers["User-Agent"] == "Test Agent test@example.com"
        if request.url.path == "/files/company_tickers.json":
            return httpx.Response(200, json=TICKERS)
        assert str(request.url) == "https://data.sec.gov/submissions/CIK0001067983.json"
        return httpx.Response(200, json={"name": "BERKSHIRE HATHAWAY INC"})

    with _make(handler) as client:
        assert client.submissions("brk.b")["name"] == "BERKSHIRE HATHAWAY INC"
        assert client.resolve_cik("0000320193") == 320193
        assert [row["ticker"] for row in client.lookup("twitter")] == ["TWTR"]
        with pytest.raises(LookupError):
            client.resolve_cik("NOPE")
    # Ticker map is fetched once and cached.
    assert sum(r.url.path == "/files/company_tickers.json" for r in seen) == 1


def test_filings_filters_and_follows_older_pages():
    recent = _columns(
        [
            {
                "accessionNumber": "0000320193-24-000123",
                "form": "10-K",
                "filingDate": "2024-11-01",
                "reportDate": "2024-09-28",
                "primaryDocument": "aapl-20240928.htm",
                "primaryDocDescription": "10-K",
                "items": "",
            },
            {
                "accessionNumber": "0000320193-24-000100",
                "form": "8-K",
                "filingDate": "2024-08-01",
                "reportDate": "",
                "primaryDocument": "a8k.htm",
                "primaryDocDescription": "8-K",
                "items": "2.02",
            },
        ]
    )
    older = _columns(
        [
            {
                "accessionNumber": "0000320193-23-000106",
                "form": "10-K",
                "filingDate": "2023-11-03",
                "reportDate": "2023-09-30",
                "primaryDocument": "aapl-20230930.htm",
                "primaryDocDescription": "10-K",
                "items": "",
            },
            {
                "accessionNumber": "0000320193-22-000108",
                "form": "10-K",
                "filingDate": "2022-10-28",
                "reportDate": "2022-09-24",
                "primaryDocument": "aapl-20220924.htm",
                "primaryDocDescription": "10-K",
                "items": "",
            },
        ]
    )

    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path == "/submissions/CIK0000320193.json":
            return httpx.Response(
                200,
                json={
                    "name": "Apple Inc.",
                    "tickers": ["AAPL"],
                    "filings": {
                        "recent": recent,
                        "files": [{"name": "CIK0000320193-submissions-001.json"}],
                    },
                },
            )
        assert request.url.path == "/submissions/CIK0000320193-submissions-001.json"
        return httpx.Response(200, json=older)

    with _make(handler) as client:
        result = client.filings("320193", forms=["10-k"], since="2023-01-01", limit=5)

    assert [f["accession"] for f in result["filings"]] == [
        "0000320193-24-000123",
        "0000320193-23-000106",
    ]
    assert result["filings"][0]["url"] == (
        "https://www.sec.gov/Archives/edgar/data/320193/000032019324000123/aapl-20240928.htm"
    )


def test_search_builds_query_and_normalizes_hits():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.host == "efts.sec.gov"
        params = request.url.params
        assert params["q"] == '"tariff exposure"'
        assert params["forms"] == "10-K,10-Q"
        assert params["dateRange"] == "custom"
        assert params["startdt"] == "2024-01-01"
        assert params["enddt"] == "2024-12-31"
        assert params["from"] == "0"
        return httpx.Response(
            200,
            json={
                "hits": {
                    "total": {"value": 1, "relation": "eq"},
                    "hits": [
                        {
                            "_id": "0001007587-24-000003:kvhi-20231231.htm",
                            "_source": {
                                "adsh": "0001007587-24-000003",
                                "ciks": ["0001007587"],
                                "display_names": ["KVH INDUSTRIES INC (KVHI)"],
                                "form": "10-K",
                                "file_date": "2024-03-15",
                                "period_ending": "2023-12-31",
                            },
                        }
                    ],
                }
            },
        )

    with _make(handler) as client:
        result = client.search(
            '"tariff exposure"', forms=["10-K", "10-Q"], start="2024-01-01", end="2024-12-31"
        )

    assert result["total"] == 1
    hit = result["results"][0]
    assert hit["filename"] == "kvhi-20231231.htm"
    assert hit["url"] == (
        "https://www.sec.gov/Archives/edgar/data/1007587/000100758724000003/kvhi-20231231.htm"
    )


def test_retries_rate_limits_then_raises_on_client_errors():
    statuses = iter([429, 503, 200])

    def handler(request: httpx.Request) -> httpx.Response:
        status = next(statuses)
        return httpx.Response(status, json={"name": "Apple Inc."} if status == 200 else {})

    with _make(handler) as client:
        assert client.submissions("320193")["name"] == "Apple Inc."

    forbidden = _make(lambda request: httpx.Response(403, text="Undeclared Automated Tool"))
    with forbidden as client, pytest.raises(httpx.HTTPStatusError):
        client.submissions("320193")


def test_document_uses_primary_document_and_extracts_text():
    html = (
        "<html><head><title>x</title><style>p{}</style></head><body>"
        "<ix:header><ix:hidden>hidden</ix:hidden></ix:header>"
        "<p>Risk&nbsp;Factors</p><table><tr><td>Revenue</td><td>$100</td></tr></table>"
        "</body></html>"
    )

    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path == "/submissions/CIK0000320193.json":
            return httpx.Response(
                200,
                json={
                    "filings": {
                        "recent": {
                            "accessionNumber": ["0000320193-24-000123"],
                            "primaryDocument": ["aapl-20240928.htm"],
                        }
                    }
                },
            )
        assert (
            request.url.path == "/Archives/edgar/data/320193/000032019324000123/aapl-20240928.htm"
        )
        return httpx.Response(200, text=html)

    with _make(handler) as client:
        doc = client.document("320193", "000032019324000123", max_chars=12)

    assert doc["accession"] == "0000320193-24-000123"
    assert doc["filename"] == "aapl-20240928.htm"
    assert doc["text"] == "Risk Factors"
    assert doc["truncated"] is True
    assert "hidden" not in html_to_text(html)
    assert "Revenue | $100" in html_to_text(html)


def test_facts_filters_and_sorts_newest_first():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/api/xbrl/companyconcept/CIK0000320193/us-gaap/Revenues.json"
        return httpx.Response(
            200,
            json={
                "entityName": "Apple Inc.",
                "label": "Revenues",
                "units": {
                    "USD": [
                        {"end": "2022-09-24", "val": 1, "form": "10-K", "filed": "2022-10-28"},
                        {"end": "2024-09-28", "val": 3, "form": "10-K", "filed": "2024-11-01"},
                        {"end": "2024-06-29", "val": 2, "form": "10-Q", "filed": "2024-08-02"},
                    ]
                },
            },
        )

    with _make(handler) as client:
        result = client.facts("320193", "Revenues", form="10-K", limit=5)

    assert result["concept"] == "us-gaap:Revenues"
    assert [row["val"] for row in result["units"]["USD"]] == [3, 1]


def test_normalize_accession():
    assert normalize_accession("000032019324000123") == "0000320193-24-000123"
    assert normalize_accession("0000320193-24-000123") == "0000320193-24-000123"
    with pytest.raises(ValueError):
        normalize_accession("123")
